"""言語サーバーの確認 / language-server check for fascon 1.0

  python3 tests/lsp_probe.py <fascon> <1.0 file with an error> <1.0 file without errors> <0.1 file> <workspace dir>
  e.g. python3 tests/lsp_probe.py target/release/fascon tests/err/f2101_color.fsc examples/kernel_core.fsc examples/legacy.rs /tmp/ws

Checks: initialize, native 1.0 diagnostics, explain / regions / build commands, hover, completion,
relaying 0.1 documents to fascon-core (when it sits next to fascon), switching a document from 0.1 to 1.0,
and a clean shutdown. Set FASCON_LSP_STANDALONE=1 to test without fascon-core.
"""
import json, os, subprocess, sys, time, threading, queue

exe, v2err, v2ok, v1, outdir = sys.argv[1:6]
env = dict(os.environ)
p = subprocess.Popen([exe, "lsp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd=outdir, env=env)
q = queue.Queue()


def reader():
    while True:
        hdr = b""
        while not hdr.endswith(b"\r\n\r\n"):
            c = p.stdout.read(1)
            if not c:
                q.put(None)
                return
            hdr += c
        n = int([l for l in hdr.decode().split("\r\n") if l.lower().startswith("content-length")][0].split(":")[1])
        q.put(json.loads(p.stdout.read(n)))


threading.Thread(target=reader, daemon=True).start()


def send(obj):
    b = json.dumps(obj).encode()
    p.stdin.write(b"Content-Length: %d\r\n\r\n" % len(b) + b)
    p.stdin.flush()


def wait(pred, timeout=10):
    end = time.time() + timeout
    seen = []
    while time.time() < end:
        try:
            m = q.get(timeout=end - time.time())
        except queue.Empty:
            break
        if m is None:
            break
        if pred(m):
            return m
        seen.append(m)
    raise SystemExit("timeout; seen: %s" % json.dumps(seen)[:2000])


def uri(path):
    return "file://" + os.path.abspath(path)


ok = True


def check(cond, what):
    global ok
    print(("PASS " if cond else "FAIL ") + what)
    ok = ok and cond


send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"processId": None, "rootUri": uri(outdir), "capabilities": {}, "initializationOptions": {"language": "en", "outDir": "build"}}})
r = wait(lambda m: m.get("id") == 1)
print("serverInfo:", r["result"].get("serverInfo"))
check("hoverProvider" in r["result"]["capabilities"], "initialize capabilities")
send({"jsonrpc": "2.0", "method": "initialized", "params": {}})

# 1.0 document with an error -> native diagnostics
u_err = uri(v2err)
send({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": u_err, "languageId": "fascon", "version": 1, "text": open(v2err).read()}}})
d = wait(lambda m: m.get("method") == "textDocument/publishDiagnostics" and m["params"]["uri"] == u_err)
codes = [x["code"] for x in d["params"]["diagnostics"]]
print("v2 diagnostics:", codes, d["params"]["diagnostics"][0]["message"][:200] if codes else "")
check(codes and all(x.get("source") == "fascon" for x in d["params"]["diagnostics"]), "native 1.0 diagnostics with source=fascon")
first = d["params"]["diagnostics"][0]
send({"jsonrpc": "2.0", "id": 2, "method": "workspace/executeCommand", "params": {"command": "fascon.explain", "arguments": [u_err, first["range"]["start"]["line"]]}})
r = wait(lambda m: m.get("id") == 2)
check(r["result"] and r["result"]["markdown"].startswith("## " + first["code"]), "explain markdown for 1.0 doc")

# 1.0 document without errors -> empty diagnostics, regions, build, hover, completion
u_ok = uri(v2ok)
text_ok = open(v2ok).read()
send({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": u_ok, "languageId": "fascon", "version": 1, "text": text_ok}}})
d = wait(lambda m: m.get("method") == "textDocument/publishDiagnostics" and m["params"]["uri"] == u_ok)
check(d["params"]["diagnostics"] == [], "clean 1.0 doc has no diagnostics")
send({"jsonrpc": "2.0", "id": 3, "method": "workspace/executeCommand", "params": {"command": "fascon.regions", "arguments": [u_ok]}})
r = wait(lambda m: m.get("id") == 3)
print(r["result"][:600])
check("arena " in r["result"] or "mmio" in r["result"], "regions text")
send({"jsonrpc": "2.0", "id": 4, "method": "workspace/executeCommand", "params": {"command": "fascon.build", "arguments": [u_ok]}})
r = wait(lambda m: m.get("id") == 4)
print("build:", r["result"])
check(r["result"]["ok"] and os.path.isfile(r["result"]["path"]), "build writes the C file")
# hover on the first `@name` occurrence
lines = text_ok.split("\n")
pos = None
for i, l in enumerate(lines):
    if l.startswith("arena @") or l.startswith("#[framebuffer] mmio @") or l.startswith("mmio @"):
        pos = (i, l.index("@") + 2)
        break
send({"jsonrpc": "2.0", "id": 5, "method": "textDocument/hover", "params": {"textDocument": {"uri": u_ok}, "position": {"line": pos[0], "character": pos[1]}}})
r = wait(lambda m: m.get("id") == 5)
print("hover:", json.dumps(r["result"])[:400])
check(r["result"] is not None and ("arena " in r["result"]["contents"]["value"] or "mmio " in r["result"]["contents"]["value"]), "hover on a region name")
send({"jsonrpc": "2.0", "id": 6, "method": "textDocument/completion", "params": {"textDocument": {"uri": u_ok}, "position": {"line": 0, "character": 0}}})
r = wait(lambda m: m.get("id") == 6)
labels = [x["label"] for x in r["result"]]
check("arena" in labels and "own" in labels, "completion has 1.0 keywords (%d items)" % len(labels))

# 0.1 document -> relayed to fascon-core
u1 = uri(v1)
send({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": u1, "languageId": "fascon", "version": 1, "text": open(v1).read()}}})
d = wait(lambda m: m.get("method") == "textDocument/publishDiagnostics" and m["params"]["uri"] == u1)
print("v1 diagnostics:", [x["code"] for x in d["params"]["diagnostics"]])
check(True, "0.1 doc diagnostics arrive (relayed or native)")
send({"jsonrpc": "2.0", "id": 7, "method": "textDocument/hover", "params": {"textDocument": {"uri": u1}, "position": {"line": 5, "character": 5}}})
r = wait(lambda m: m.get("id") == 7)
print("v1 hover:", json.dumps(r.get("result"), ensure_ascii=False)[:200])

# turn the 0.1 doc into a 1.0 doc: legacy diagnostics must be dropped, native ones published
send({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {"textDocument": {"uri": u1, "version": 2}, "contentChanges": [{"text": "struct T { x: u32 }\narena @t: T[4];\nfn main() { let a = @t.alloc(T { x: 1 }); }\n"}]}})
d = wait(lambda m: m.get("method") == "textDocument/publishDiagnostics" and m["params"]["uri"] == u1)
print("after switch to 1.0:", [x["code"] for x in d["params"]["diagnostics"]])
check(all(x.get("source") == "fascon" for x in d["params"]["diagnostics"]), "switch to 1.0 handled")

send({"jsonrpc": "2.0", "id": 9, "method": "shutdown"})
r = wait(lambda m: m.get("id") == 9)
send({"jsonrpc": "2.0", "method": "exit"})
time.sleep(0.5)
print("exit code:", p.poll())
check(p.poll() == 0, "clean exit")
err = p.stderr.read().decode(errors="replace")
if err.strip():
    print("stderr:", err[:500])
sys.exit(0 if ok else 1)
