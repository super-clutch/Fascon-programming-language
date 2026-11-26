#!/usr/bin/env python3
"""生成 C の不変性の検査（注釈・空白を除く） / the byte-identity gate for generated C (comments and whitespace aside)

  python3 tools/compare_c.py                         # target/release/fascon を記録した基準と照合する
                                                     # compare target/release/fascon against the recorded baseline
  python3 tools/compare_c.py --fascon <fascon>       # 別のコンパイラを基準と照合する / compare another compiler
  python3 tools/compare_c.py --record <old fascon>   # 基準を記録し直す（意図して C を変えたときだけ）
                                                     # re-record the baseline (only when the C is meant to change)
  python3 tools/compare_c.py <new fascon> <old fascon> <file.fsc>...   # 2 つのコンパイラを直接比べる / compare two
                                                     # compilers directly

基準（tests/golden_c.json）は、examples/*.fsc・examples/*.rs・tests/**/*.fsc のすべてを既定・--columns mirrored・
--columns off の 3 通りでビルドした C（注釈と空白を除いて正規化）の SHA-256 と、ビルドできないファイルのエラー番号を持つ。
新しい機能（f32 / f64 の列・gpu_arena・--target hpc など）を足しても、それを使わない既存のプログラムの生成 C は 1 バイトも
変わらないことを、この基準との照合で確かめる。基準にないファイル（新しい機能のテスト）は NEW と表示して数えない。
The baseline (tests/golden_c.json) holds, for every examples/*.fsc, examples/*.rs and tests/**/*.fsc, the SHA-256 of the C
built with the default flags, --columns mirrored and --columns off (normalized without comments and whitespace), and the
error codes of files that do not build. Comparing against it confirms that new features (f32 / f64 columns, gpu_arena,
--target hpc, ...) leave the generated C of existing programs unchanged to the byte. Files missing from the baseline (tests
of the new features) are shown as NEW and not counted.

gc_arena を使うプログラム（fascon-gc）は、ローカルの own の根（SPEC-1.0-UNIVERSAL §5.7、2026-11）で生成 C が変わってよいので、
違っても GC と表示して数えない（gc_arena を使わないプログラムは 1 バイトも変わらないこと）。
Programs using gc_arena (fascon-gc) may change their C with the roots behind local owns (SPEC-1.0-UNIVERSAL section 5.7,
2026-11), so a difference there is shown as GC and not counted (programs without gc_arena must not change by a single byte).

終了コード 0 = gc_arena を使わないプログラムはすべて同じ / exit code 0 = every program without gc_arena matches
"""
import concurrent.futures
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GOLDEN = os.path.join(ROOT, "tests", "golden_c.json")
MODES = (("default", []), ("mirrored", ["--columns", "mirrored"]), ("off", ["--columns", "off"]))


def norm(c):
    c = re.sub(r"/\*.*?\*/", "", c, flags=re.S)
    c = re.sub(r"//[^\n]*", "", c)
    # _Static_assert の文言（名前の書き方だけが違う）は比べない / ignore the wording of _Static_assert messages
    c = re.sub(r'(_Static_assert\([^;]*?), "[^"]*"\)', r'\1, "")', c)
    return re.sub(r"\s+", " ", c).strip()


def stem_of(f):
    b = os.path.basename(f)
    for ext in (".fsc.rs", ".fsc", ".rs"):
        if b.endswith(ext):
            return b[: -len(ext)]
    return b


def build(fascon, rel, extra):
    """rel（ROOT からの相対パス）をビルドし、正規化した C か "ERROR <番号>" を返す / build rel (relative to ROOT) and return
    the normalized C or "ERROR <codes>" """
    d = tempfile.mkdtemp(prefix="fascon-cmp-")
    env = dict(os.environ)
    for k in ("FASCON_COLUMNS", "FASCON_NONTEMPORAL", "FASCON_GC", "FASCON_GPU", "FASCON_FAST_MATH"):
        env.pop(k, None)
    r = subprocess.run([fascon, "build", rel, "--engine", "1", "--out-dir", d, "--lang", "en"] + extra, capture_output=True, text=True, cwd=ROOT, env=env)
    p = os.path.join(d, stem_of(rel) + ".fascon.c")
    if r.returncode == 0 and os.path.exists(p):
        with open(p, encoding="utf-8") as fh:
            return norm(fh.read())
    codes = sorted(set(re.findall(r"error\[(F\d{4})\]", r.stderr)))
    return "ERROR " + " ".join(codes)


def digest(text):
    if text.startswith("ERROR"):
        return text
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


# ボードのパッケージで作るプログラムは、そのボードでもビルドする（キーは「パス@board=名前」）
# programs made with a board package are also built for that board (the key is "path@board=name")
BOARDS = {
    "tests/board/hal_hello.fsc": ("raspberrypi3b", "qemu-virt-generic", "qemu-virt-riscv64"),
    "tests/board/rpi3_hello.fsc": ("raspberrypi3b",),
}


def split_key(key):
    """キーからパスと追加のフラグ / the path and the extra flags of a key"""
    if "@board=" in key:
        rel, b = key.split("@board=", 1)
        return rel, ["--board", b]
    return key, []


def programs():
    out = []
    for f in sorted(os.listdir(os.path.join(ROOT, "examples"))):
        if f.endswith(".fsc") or f.endswith(".rs"):
            out.append("examples/" + f)
    for dirpath, _, files in sorted(os.walk(os.path.join(ROOT, "tests"))):
        for f in sorted(files):
            if f.endswith(".fsc"):
                rel = os.path.relpath(os.path.join(dirpath, f), ROOT).replace(os.sep, "/")
                out.append(rel)
                for b in BOARDS.get(rel, ()):
                    out.append(rel + "@board=" + b)
    return out


def all_digests(fascon, files):
    jobs = [(f, m, x) for f in files for (m, x) in MODES]
    res = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=max(2, (os.cpu_count() or 2) * 2)) as ex:
        futs = {ex.submit(build, fascon, split_key(f)[0], split_key(f)[1] + x): (f, m) for (f, m, x) in jobs}
        for fu in concurrent.futures.as_completed(futs):
            f, m = futs[fu]
            res.setdefault(f, {})[m] = digest(fu.result())
    return res


def two_binaries(new, old, files):
    bad = 0
    for f in files:
        rel = os.path.relpath(os.path.abspath(f), ROOT)
        a, b = build(new, rel, []), build(old, rel, [])
        if a == b:
            print("SAME", f)
        else:
            bad += 1
            i = next((k for k in range(min(len(a), len(b))) if a[k] != b[k]), min(len(a), len(b)))
            print("DIFF", f, "\n   new:", a[max(0, i - 150) : i + 250], "\n   old:", b[max(0, i - 150) : i + 250])
    return 1 if bad else 0


def main():
    argv = sys.argv[1:]
    if argv and not argv[0].startswith("--"):
        if len(argv) < 3:
            print(__doc__)
            return 2
        return two_binaries(argv[0], argv[1], argv[2:])
    fascon = os.path.join(ROOT, "target", "release", "fascon")
    if "--fascon" in argv:
        fascon = argv[argv.index("--fascon") + 1]
    if "--record" in argv:
        old = argv[argv.index("--record") + 1]
        files = programs()
        res = all_digests(old, files)
        data = {
            "about": "SHA-256 of the normalized generated C (tools/compare_c.py) per program and mode; ERROR = the error codes",
            "modes": {m: " ".join(x) or "(default)" for m, x in MODES},
            "files": {f: res[f] for f in files},
        }
        with open(GOLDEN, "w", encoding="utf-8") as fh:
            json.dump(data, fh, ensure_ascii=False, indent=1, sort_keys=True)
            fh.write("\n")
        print("recorded %d programs x %d modes into %s" % (len(files), len(MODES), os.path.relpath(GOLDEN, ROOT)))
        return 0
    with open(GOLDEN, encoding="utf-8") as fh:
        golden = json.load(fh)["files"]
    files = programs()
    known = [f for f in files if f in golden]
    res = all_digests(fascon, known)
    bad = 0
    gc_diff = 0
    gc_files = 0
    for f in known:
        uses_gc = re.search(r"\bgc_arena\b", open(os.path.join(ROOT, split_key(f)[0]), encoding="utf-8").read()) is not None
        gc_files += 1 if uses_gc else 0
        for m, _ in MODES:
            want, got = golden[f].get(m), res[f][m]
            if want != got:
                if uses_gc:
                    gc_diff += 1
                    print("GC   %s (%s): uses gc_arena, allowed to change (own roots): baseline %s, now %s" % (f, m, want[:24], got[:24]))
                    continue
                bad += 1
                print("DIFF %s (%s): baseline %s, now %s" % (f, m, want[:24], got[:24]))
    for f in files:
        if f not in golden:
            print("NEW  %s (not in the baseline: a test of a new feature)" % f)
    gone = sorted(set(golden) - set(files))
    for f in gone:
        print("GONE %s (in the baseline but missing)" % f)
    print("%d programs x %d modes compared (%d without gc_arena, %d with): %s; gc_arena programs that changed: %d build(s)" % (len(known), len(MODES), len(known) - gc_files, gc_files, "every program without gc_arena SAME" if not bad and not gone else "%d differ, %d missing" % (bad, len(gone)), gc_diff))
    return 1 if bad or gone else 0


if __name__ == "__main__":
    sys.exit(main())
