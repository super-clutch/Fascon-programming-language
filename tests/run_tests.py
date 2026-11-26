#!/usr/bin/env python3
"""Fascon 1.0 のテスト一式 / the Fascon 1.0 test suite

  python3 tests/run_tests.py [path/to/fascon] [--qemu] [--fuzz N]

- tests/err/*.fsc : 1 行目の `// expect: F....` のエラーコードが出ること / the error code on line 1 must be reported
- tests/ok/*.fsc  : PC 上で実行し、出力が *.expected と一致すること（C コンパイラが要る）
                    run on the PC; the output must match *.expected (needs a C compiler)
- tests/ok/tsoa_*: 完全列化（True SoA）・非時間的ストア・浮動小数点の列（#[fast_math]）・条件付き一括更新（Masked Column RMW）。
                    --columns と --nontemporal のすべての組み合わせで同じ出力、生成 C の形、fascon regions の判定と理由。
                    走査関数の単体テスト（tests/cols/rmw_kernels.c、-O2 と ASan / UBSan）とあふれの panic の一致
                    True SoA, non-temporal stores, float columns (#[fast_math]) and masked column RMW: the same output for every
                    combination of --columns and --nontemporal, the shape of the C, the decisions and reasons in fascon regions,
                    the kernel unit test (tests/cols/rmw_kernels.c, -O2 and ASan / UBSan) and the same overflow panic
- gpu_arena       : tests/ok/gpu_offload.fsc の生成 C が fascon_plat_gpu_* だけを使い（スレッド・GPU・NUMA の API もアセンブリも
                    書かない）、PC の真似を ASan + UBSan と TSan で走らせ、--gpu=off でも同じ出力。regions とパッケージのラベル
                    the C of tests/ok/gpu_offload.fsc uses fascon_plat_gpu_* only (no thread, GPU or NUMA API, no assembly),
                    the PC emulation runs under ASan + UBSan and TSan, --gpu=off prints the same; regions and the package label
- --target hpc    : tests/ok/hpc_compute.fsc の生成 C（64 ビット Id・コアの入口のピン留めと NUMA の固定）と regions の HPC の節、
                    tests/hpc/io_noise.fsc の警告 F3901 / the C of tests/ok/hpc_compute.fsc (64-bit Ids, pinning and NUMA binding
                    at core entries), the HPC section of regions, and the F3901 warning of tests/hpc/io_noise.fsc
- fascon-gc の own: tests/ok/gc_local_own_roots.fsc の生成 C（own の先まで辿る根・解放の前の NULL・フレームの NULL・戻り値の根・
                    一時値の受け渡し）、gc のテストを ASan + UBSan + LeakSanitizer + 別名の検査（-DFASCON2_GC_ALIAS_CHECK）で、
                    共有の gc_arena を TSan で / fascon-gc owns: the C of tests/ok/gc_local_own_roots.fsc (roots followed behind
                    own, NULL before free, NULL before a frame pop, return slots, hand-over of temporaries), the gc tests under
                    ASan + UBSan + LeakSanitizer + the alias check (-DFASCON2_GC_ALIAS_CHECK), the shared gc_arena under TSan
- tests/float/    : fascon_put_f64 / _f32 をビットの並み 40 万個で Python と比べる / the float printer against Python on 400,000
                    bit patterns
- examples/*.fsc  : 検査が通り、PC 上の実行が FASCON-KERNEL-OK / 画面保存まで進むこと
                    must check clean; the PC run must reach FASCON-KERNEL-OK / save the screen
- tests/cols/      : 列（集計の列・ビット列・鍵の列）。cols.fsc の出力が --columns on / mirrored / off のどれでも cols.expected と一致し、
                    生成 C が列の走査を使うこと。さらに colfuzz.py が作るランダムなプログラムを on / mirrored / off で走らせて比べる（--tsoa・
                    --float・--rmw のプログラムは --nontemporal on も）
                    columns (aggregate, bit and key columns): cols.fsc must print cols.expected with --columns on, mirrored and off and
                    its C must use the column scans; random programs from colfuzz.py are run with on / mirrored / off and compared
                    (the --tsoa, --float and --rmw programs also with --nontemporal on)
- 厳格形の往復  : tests/ok と examples の各ファイルを --emit-strict-fascon で書き出し、それをもう一度コンパイルした C が
                    元と同じ（注釈・行番号を除く）こと / the strict form round trip: compiling the --emit-strict-fascon output
                    again gives the same C (comments and line numbers aside)
- --qemu          : さらに QEMU で起動する: BIOS なし AArch64（-kernel）と、Fascon の仮想 BIOS（x86_64 / AArch64 の -bios）
                    （clang + ld.lld か gcc、qemu-system-aarch64 と qemu-system-x86_64 が要る）
                    also boot on QEMU: BIOS-less AArch64 (-kernel) and the Fascon virtual BIOS (x86_64 / AArch64 -bios)
                    (needs clang + ld.lld or gcc, qemu-system-aarch64 and qemu-system-x86_64)
- --fuzz N        : ランダムなプログラムの数（既定 8、0 で省く） / how many random programs (default 8, 0 skips them)
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
args = [a for a in sys.argv[1:] if not a.startswith("--")]
FASCON = args[0] if args else shutil.which("fascon") or os.path.join(ROOT, "target", "release", "fascon")
QEMU = "--qemu" in sys.argv
FUZZ = 8
if "--fuzz" in sys.argv:
    i = sys.argv.index("--fuzz")
    FUZZ = int(sys.argv[i + 1]) if i + 1 < len(sys.argv) else FUZZ
    if i + 1 < len(sys.argv) and sys.argv[i + 1] in args:
        args.remove(sys.argv[i + 1])
failed = 0
passed = 0


def run(cmd, **kw):
    return subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8", errors="replace", **kw)


def report(ok, name, detail=""):
    global failed, passed
    if ok:
        passed += 1
        print("PASS", name)
    else:
        failed += 1
        print("FAIL", name, detail)


tmp = tempfile.mkdtemp(prefix="fascon-tests-")

for f in sorted(os.listdir(os.path.join(ROOT, "tests", "err"))):
    if not f.endswith(".fsc"):
        continue
    path = os.path.join(ROOT, "tests", "err", f)
    head = open(path, encoding="utf-8").read().split("\n")[:3]
    expect = head[0].split("expect:")[1].strip()
    # 2 行目の `// env: K=V` は検査のときだけ環境変数を足す（--gc=off = FASCON_GC=off など）
    # a second line `// env: K=V` adds an environment variable for the check only (--gc=off = FASCON_GC=off, ...)
    extra_env = None
    if len(head) > 1 and "env:" in head[1]:
        k, v = head[1].split("env:")[1].strip().split("=", 1)
        extra_env = dict(os.environ)
        extra_env[k.strip()] = v.strip()
    r = run([FASCON, "check", path, "--json", "--lang", "en", "--engine", "1"], env=extra_env) if extra_env else run([FASCON, "check", path, "--json", "--lang", "en", "--engine", "1"])
    try:
        got = sorted({d["code"] for d in json.loads(r.stdout) if d.get("severity") == "error"})
    except Exception as e:  # noqa: BLE001
        got = ["<%s>" % e]
    report(got == [expect], "err/" + f, "expected %s, got %s" % (expect, got))

have_cc = any(shutil.which(c) for c in ("cc", "gcc", "clang"))
# 1 行目の印: `// fascon-test: qemu-only` は QEMU でだけ動かす（EL0 のタスクなど。下の --qemu の節）、`// fascon-abi:` は
# ユーザープログラム（EL0 の像。カーネルのテストが作って使う）
# first-line markers: `// fascon-test: qemu-only` runs only on QEMU (EL0 tasks and the like; the --qemu section below),
# `// fascon-abi:` marks a user program (an EL0 image the kernel test builds and uses)
def first_line(path):
    with open(path, encoding="utf-8") as fh:
        return fh.readline()


qemu_only_ok = []
for f in sorted(os.listdir(os.path.join(ROOT, "tests", "ok"))):
    if not f.endswith(".fsc"):
        continue
    path = os.path.join(ROOT, "tests", "ok", f)
    head = first_line(path)
    r = run([FASCON, "check", path, "--lang", "en", "--engine", "1"])
    report(r.returncode == 0, "ok/" + f + " (check)", r.stderr[:400])
    if "fascon-test: qemu-only" in head:
        qemu_only_ok.append(f)
        continue
    if not have_cc or "fascon-abi:" in head:
        continue
    out = os.path.join(tmp, f)
    r = run([FASCON, "run", path, "--engine", "1", "--out-dir", out])
    expected = open(path[:-4] + ".expected", encoding="utf-8").read()
    report(r.returncode == 0 and r.stdout == expected, "ok/" + f + " (run)", r.stderr[-400:] + r.stdout[-400:])

# 厳格形（--emit-strict-fascon）の往復: 書き出した厳格な Fascon をもう一度コンパイルすると、元と同じ C になること
# the strict form (--emit-strict-fascon) round trip: compiling the strict Fascon output again gives the same C
def norm_c(c):
    c = re.sub(r"/\*.*?\*/", "", c, flags=re.S)
    c = re.sub(r"//[^\n]*", "", c)
    c = re.sub(r'"[^"\n]*:\d+:\d+"', '"SITE"', c)
    return re.sub(r"\s+", " ", c).strip()


def stem_of(f):
    b = os.path.basename(f)
    for ext in (".fsc.rs", ".fsc", ".rs"):
        if b.endswith(ext):
            return b[: -len(ext)]
    return b


rt_files = [os.path.join(ROOT, "tests", "ok", f) for f in sorted(os.listdir(os.path.join(ROOT, "tests", "ok"))) if f.endswith(".fsc")]
rt_files += [os.path.join(ROOT, "examples", f) for f in sorted(os.listdir(os.path.join(ROOT, "examples"))) if f.endswith(".fsc") or f.endswith(".rs")]
rt_files += [os.path.join(ROOT, "tests", "cols", "cols.fsc"), os.path.join(ROOT, "tests", "latch", "latch.fsc")]
bad = []
for f in rt_files:
    d1 = os.path.join(tmp, "strict-" + os.path.basename(f))
    r = run([FASCON, "build", f, "--engine", "1", "--out-dir", d1, "--emit-strict-fascon", "--lang", "en"])
    st = os.path.join(d1, stem_of(f) + ".strict.fsc")
    if r.returncode != 0 or not os.path.isfile(st):
        bad.append(os.path.basename(f) + " (build)")
        continue
    d2 = os.path.join(d1, "again")
    os.makedirs(d2, exist_ok=True)
    again = os.path.join(d2, stem_of(f) + ".fsc")
    shutil.copy(st, again)
    r2 = run([FASCON, "build", again, "--out-dir", d2, "--lang", "en", "--reframing", "off"])
    c1 = os.path.join(d1, stem_of(f) + ".fascon.c")
    c2 = os.path.join(d2, stem_of(f) + ".fascon.c")
    if r2.returncode != 0 or not os.path.isfile(c2) or norm_c(open(c1, encoding="utf-8").read()) != norm_c(open(c2, encoding="utf-8").read()):
        bad.append(os.path.basename(f) + " (" + (r2.stderr[:200] if r2.returncode else "C differs") + ")")
report(not bad, "strict form round trip (%d files: strict Fascon compiles to the same C)" % len(rt_files), "; ".join(bad))

# 厳格形の中身: 型を書いた let・検証付きのルックアップ・ポートの読み書き・`@` なし
# what the strict form contains: typed lets, verified lookups, port reads / writes, no `@`
st = os.path.join(tmp, "strict-noat_order.fsc", "noat_order.strict.fsc")
text = open(st, encoding="utf-8").read() if os.path.isfile(st) else ""
# `@` は同じ名前のローカルがリージョンを隠している所にだけ付く / `@` appears only where a local hides the region
body = text.split("\n\n", 1)[1] if "\n\n" in text else ""
ok = "let a: Id<pcb_table> = spawn_proc(1u32);" in text and "pcb_table.get_mut(a).unwrap().state = 1u8;" in text and "pcb_table.get(a).unwrap()" in text
ok = ok and "print_int(@pcb_table.get(a).unwrap().pid as i64);" in text and body.count("@") == 1
st = os.path.join(tmp, "strict-ports.fsc", "ports.strict.fsc")
text = open(st, encoding="utf-8").read() if os.path.isfile(st) else ""
ok = ok and "com1.write(c);" in text and "scratch_reg.write(scratch_reg.read() | 0x40u8);" in text and "com1_ctl.read_at(4i32)" in text
report(ok, "strict form (typed lets, get / get_mut + unwrap, port read / write, no @)")

# 遅延フォールト（ラッチ）と 32 ビット Id: C のハーネスが停止を longjmp で受け止め、止まったあとのメモリを調べる
# deferred faults (the latch) and 32-bit Ids: a C harness catches stops with longjmp and inspects memory afterwards
cc = next((shutil.which(c) for c in ("cc", "gcc", "clang") if shutil.which(c)), None)
if cc:
    # 完全列化（on）・写しの列（mirrored）・1.0.0 の表（off）のどれでも同じ検査が通ること / the same checks pass with True
    # SoA (on), mirrored columns (mirrored) and the 1.0.0 tables (off)
    for mode in ("on", "mirrored", "off"):
        src = os.path.join(ROOT, "tests", "latch", "latch.fsc")
        out = os.path.join(tmp, "latch-" + mode)
        r = run([FASCON, "build", src, "--out-dir", out, "--columns", mode])
        ok = r.returncode == 0
        if ok:
            exe = os.path.join(out, "harness")
            r = run([cc, "-O2", "-std=c11", "-I" + out, os.path.join(ROOT, "tests", "latch", "harness.c"), "-o", exe])
            ok = r.returncode == 0
            if ok:
                r = run([exe])
                ok = r.returncode == 0 and " 0 failures" in r.stdout
        report(ok, "latch/latch.fsc (C harness, --columns %s)" % mode, r.stderr[-600:] + r.stdout[-600:])

# 総和の特殊化の例: 走査関数が出ていて、結果が正しいこと / the sum-specialization example: a scan function and the right totals
src = os.path.join(ROOT, "examples", "vruntime_scan.fsc")
out = os.path.join(tmp, "vruntime_scan")
r = run([FASCON, "run", src, "--out-dir", out])
gen = os.path.join(out, "vruntime_scan.fascon.c")
ok = r.returncode == 0 and r.stdout.split() == ["1497000", "1494003"] and os.path.isfile(gen) and "fsc_reduce_task_1(&arena_task, &_flt" in open(gen, encoding="utf-8").read()
report(ok, "examples/vruntime_scan.fsc (sum specialization)", r.stderr[-600:] + r.stdout[-600:])

# エスケープしない own はフレームリージョンへ（生成 C がフレームの確保と解放を使い、ヒープに出ないこと）
# non-escaping own values go to the frame region (the generated C allocates and pops frames, nothing reaches the heap)
src = os.path.join(ROOT, "tests", "ok", "frame.fsc")
out = os.path.join(tmp, "frame-gen")
r = run([FASCON, "build", src, "--out-dir", out])
gen = os.path.join(out, "frame.fascon.c")
text = open(gen, encoding="utf-8").read() if os.path.isfile(gen) else ""
ok = r.returncode == 0 and text.count("fascon_frame_alloc(") >= 5 and text.count("fascon_frame_release(") >= 8 and "fascon_own_alloc(FASCON_OWN_SPAN(fsc_Big)" in text
report(ok, "ok/frame.fsc (frame region from escape analysis)", r.stderr[-400:])

# 分岐なしの走査の形: 生存ビットマップ + ctz、マスク付きの集計、ループの中にコミット点がないこと
# the shape of branch-free scans: live bitmap + ctz, masked reductions, no commit point inside fixed-trip loops
src = os.path.join(ROOT, "tests", "latch", "latch.fsc")
r = run([FASCON, "regions", src, "--lang", "en"])
ok = r.returncode == 0 and "(9 masked: filters, counts, min / max)" in r.stdout and "driven by the live bitmap + ctz (no per-slot branch): 3" in r.stdout
report(ok, "latch/latch.fsc (bitmap loops and masked reductions)", r.stdout[-600:])

# 列: 同じプログラムを --columns on / mirrored / off で走らせ、どれも cols.expected と一致すること。mirrored の C は列の
# 走査と写しの作り直しを使い（完全列化の前の 1.0 と同じ C）、off の C は 1.0.0 と同じ表（列なし）であること。on の C は
# 条件を満たすアリーナ（tiny・big）を完全列化し、写しの列に残る task では書き込みの多いフィールドの列を作らないこと
# columns: the same program run with --columns on / mirrored / off must print cols.expected every time; the "mirrored"
# C must use the column scans and mirror rebuilds (the same C as 1.0 before True SoA), the "off" C must have the 1.0.0
# tables (no columns); the "on" C must make the eligible arenas (tiny, big) True SoA and give the write-heavy fields of
# task (which stays mirrored) no columns
src = os.path.join(ROOT, "tests", "cols", "cols.fsc")
expected = open(os.path.join(ROOT, "tests", "cols", "cols.expected"), encoding="utf-8").read()
gens = {}
for mode in ("on", "mirrored", "off"):
    out = os.path.join(tmp, "cols-" + mode)
    if have_cc:
        r = run([FASCON, "run", src, "--out-dir", out, "--columns", mode])
        report(r.returncode == 0 and r.stdout == expected, "cols/cols.fsc (run, --columns %s)" % mode, r.stderr[-400:] + r.stdout[-200:])
    else:
        run([FASCON, "build", src, "--out-dir", out, "--columns", mode])
    g = os.path.join(out, "cols.fascon.c")
    gens[mode] = open(g, encoding="utf-8").read() if os.path.isfile(g) else ""
on, mir, off = gens["on"], gens["mirrored"], gens["off"]
need = ["f20_col_sum_u64(arena->col.", "f20_col_sum_u32(arena->col.", "f20_col_count_bits(arena->col.", "f20_col_pick_key(arena->col.", "F20_DEFINE_TABLE_COLS(task,", "arena_task_mirror_p(_w", "arena_task_unmirror(", "f20_col_key_fresh(arena_tiny.col."]
missing = [x for x in need if x not in mir]
# get_mut の借用の終わりでは、書き込みが届く列だけを作り直す / at the end of a get_mut borrow only the columns its writes reach are rebuilt
if not re.search(r"arena_task_mirror_m[0-9a-f]+\(p\);", mir):
    missing.append("arena_task_mirror_m<mask>(p);")
# スロットの中の場所を束縛した match / if let も、借用の終わり（return を含むどの出口でも）で届く列を作り直す
# a match / if-let binding of a place inside the slot also rebuilds the columns its writes reach when the borrow ends
# (at every exit, return included)
if len(re.findall(r"arena_task_mirror_(?:p|m[0-9a-f]+)\(_w[0-9]+\); /\* (?:end of the &mut borrow `[a-z]+` \(a place inside the slot\)|`[a-z]+` の &mut の借用（スロットの中の場所）の終わり)", mir)) < 8:
    missing.append("arena_task_mirror_*(_wN) at the end of a match binding of a place in the slot")
report(not missing and mir.count(", 1);") >= 1, "cols/cols.fsc (--columns mirrored: C uses the column scans)", "missing: %s" % missing)
need_on = ["F20_DEFINE_TABLE_COLS(tiny,", "f20_part_tiny_t", "arena_tiny.col.ts_vruntime[", "f20_col_sum_u64(arena->col.ts_", "f20_col_pick64_m((const uint64_t *)(const void *)arena->col.ts_", "arena_tiny_put(i, v);", "arena_tiny_ts_zero(", "F20_DEFINE_TABLE_COLS(task,", "arena_task_mirror_p("]
missing = [x for x in need_on if x not in on]
if "f20_col_key_fresh(arena_tiny.col." in on or "arena_tiny_mirror_m" in on:
    missing.append("no key column / mirror rebuild for the True-SoA field of tiny")
report(not missing, "cols/cols.fsc (--columns on: True SoA for tiny / big, mirrored columns for task)", "missing: %s" % missing)
report(off != "" and "f20_col_" not in off.split("/* ───── arena task:")[1] and "_mirror" not in off and "F20_DEFINE_TABLE(task," in off, "cols/cols.fsc (--columns off = 1.0.0 tables)")
r = run([FASCON, "regions", src, "--lang", "en", "--columns", "mirrored"])
report(r.returncode == 0 and "reductions that read columns (they scan a column of just the values they read, never the slots): 14" in r.stdout and "task.min7_vruntime" in r.stdout, "cols/cols.fsc (regions --columns mirrored lists the columns)", r.stdout[-600:])
r = run([FASCON, "regions", src, "--lang", "en"])
ok = r.returncode == 0 and re.search(r"tiny\.ts_vruntime +True-SoA \(single-write\)", r.stdout) and re.search(r"task\.fb0 +Mirrored-SoA \(dual-write\)", r.stdout) and "task.vruntime: a match / if let binding the element (line 85)" in r.stdout and "`vruntime` is write-heavy (bulk-write weight" in r.stdout
report(bool(ok), "cols/cols.fsc (regions: True-SoA / Mirrored-SoA, why task stays mirrored, the write-heavy exclusion)", r.stdout[-900:])

# 完全列化（True SoA）: tests/ok/tsoa_*.fsc を --columns on / mirrored / off × --nontemporal auto / on / off のすべてで走らせ、
# どれも *.expected と同じこと。on の C が完全列化の形（スロットに残る部分・列の正本・番兵へ逸らす添字・マスク付きの走査・
# #[on_drop] の組み立て直し）を使い、表ごとの判定と理由・非時間的ストアの判定が fascon regions に出ること
# True SoA: run tests/ok/tsoa_*.fsc with every combination of --columns on / mirrored / off and --nontemporal
# auto / on / off, each printing *.expected; the "on" C must use the True-SoA forms (the slot part, the column as the
# only copy, sentinel-diverting indices, masked scans, rebuilding for #[on_drop]), and fascon regions must show each
# table's decision, its reasons and the non-temporal decisions
ts_gen = {}
for name in ("tsoa_life", "tsoa_fallback", "tsoa_nt", "tsoa_float", "tsoa_float_nt", "tsoa_rmw"):
    src = os.path.join(ROOT, "tests", "ok", name + ".fsc")
    expected = open(os.path.join(ROOT, "tests", "ok", name + ".expected"), encoding="utf-8").read()
    bad = []
    for cm in ("on", "mirrored", "off"):
        for nm in ("auto", "on", "off"):
            out = os.path.join(tmp, "%s-%s-%s" % (name, cm, nm))
            if have_cc:
                r = run([FASCON, "run", src, "--out-dir", out, "--columns", cm, "--nontemporal", nm], timeout=300)
                if r.returncode != 0 or r.stdout != expected:
                    bad.append("%s/%s" % (cm, nm))
            else:
                run([FASCON, "build", src, "--out-dir", out, "--columns", cm, "--nontemporal", nm])
            g = os.path.join(out, name + ".fascon.c")
            ts_gen[(name, cm, nm)] = open(g, encoding="utf-8").read() if os.path.isfile(g) else ""
    if have_cc:
        report(not bad, "ok/%s.fsc (same output for --columns on|mirrored|off x --nontemporal auto|on|off)" % name, "differs: %s" % bad)
life = ts_gen[("tsoa_life", "on", "auto")]
need = ["f20_part_task_t", "arena_task.col.ts_vruntime[", "arena_task.col.ts_weight[", "arena_task_put(i, v);", "arena_task_ts_zero(", "*out = arena_task_load(", "arena_task_ix(", "arena_task_ix_r(", "arena_task_ix_p(", "f20_col_sum_u64_m(arena->col.ts_vruntime, arena->col.fb", "f20_col_pick64_m((const uint64_t *)(const void *)arena->col.ts_vruntime", "f20_col_pick32_m((const uint32_t *)(const void *)arena->col.ts_weight", "{ fsc_Job _tv = arena_jobs_load(F20_ID_INDEX(id, ARENA_jobs_K)); fsc_drop_Job(&_tv); }", "if (id == 0u) { fsc_Job *p = (fsc_Job *)v; fsc_drop_Job(&(*p)); }", "arena_jobs_ix_strict("]
missing = [x for x in need if x not in life]
if "arena_task_mirror_m" in life or "f20_col_key_fresh(arena_task" in life:
    missing.append("no mirror rebuilds / key columns for the True-SoA fields of task")
report(not missing, "ok/tsoa_life.fsc (C: slot part, column as the only copy, sentinel indices, masked scans, #[on_drop] rebuilt from the columns)", "missing: %s" % missing)
mir = ts_gen[("tsoa_life", "mirrored", "auto")]
report("f20_part_" not in mir and "ts_vruntime" not in mir and "F20_DEFINE_TABLE_COLS(task," in mir, "ok/tsoa_life.fsc (--columns mirrored keeps the slot whole)")
r = run([FASCON, "regions", os.path.join(ROOT, "tests", "ok", "tsoa_life.fsc"), "--lang", "en"])
ok = r.returncode == 0 and all(re.search(p, r.stdout) for p in (r"task\.ts_vruntime +True-SoA \(single-write\)", r"task\.ts_weight +True-SoA \(single-write\)", r"jobs\.ts_cost +True-SoA \(single-write\)", r"task\.fb0 +Mirrored-SoA \(dual-write\)"))
report(bool(ok), "ok/tsoa_life.fsc (regions: True-SoA (single-write) / Mirrored-SoA (dual-write))", r.stdout[-800:])
r = run([FASCON, "regions", os.path.join(ROOT, "tests", "ok", "tsoa_fallback.fsc"), "--lang", "en"])
reasons = ["abi line 40: the element `Abi` is #[stable_abi]", "wire.x: the element `Wire` is #[wire]", "ffi.v: the element `Ffi` or an Id of @ffi crosses the extern \"C\" boundary of `ffi_weight`", "esc.v: a reference to the element `&@a[id]`", "addr.v: its address is taken (`&@a[id].v`"]
missing = [x for x in reasons if x not in r.stdout]
if not re.search(r"addr\.ts_w +True-SoA \(single-write\)", r.stdout):
    missing.append("addr.ts_w True-SoA")
report(r.returncode == 0 and not missing, "ok/tsoa_fallback.fsc (#[stable_abi] / #[wire] / extern \"C\" / escaping references / address-taken fields fall back, with reasons)", "missing: %s" % missing)
nt = {m: ts_gen[("tsoa_nt", "on", m)] for m in ("auto", "on", "off")}
ok = nt["auto"].count("F20_NT_STORE_U64(&arena_big.col.ts_v[") == 1 and nt["auto"].count("F20_NT_STORE") == 1 and nt["auto"].count("F20_SFENCE();") == 1
ok = ok and nt["on"].count("F20_NT_STORE") == 3 and "F20_NT_STORE_U64(&arena_irqd.col.ts_v[" in nt["on"] and nt["on"].count("F20_SFENCE();") == 3
ok = ok and "F20_NT_STORE" not in nt["off"] and "F20_SFENCE();" not in nt["off"]
ok = ok and all("F20_NT_STORE" not in ts_gen[("tsoa_nt", cm, nm)] for cm in ("mirrored", "off") for nm in ("auto", "on", "off"))
report(ok, "ok/tsoa_nt.fsc (C: non-temporal stores + F20_SFENCE only where allowed; --nontemporal auto / on / off)")
r = run([FASCON, "regions", os.path.join(ROOT, "tests", "ok", "tsoa_nt.fsc"), "--lang", "en"])
lines = ["big.v line 60: non-temporal stores (F20_NT_STORE)", "big.w line 67: plain stores — the next statement reads the same column", "irqd.v line 79: plain stores — an arena shared with interrupt handlers or tasks", "conns.bytes: a shared arena (one instance for every core): never True SoA"]
missing = [x for x in lines if x not in r.stdout]
report(r.returncode == 0 and not missing, "ok/tsoa_nt.fsc (regions: bulk-overwrite decisions, shared and interrupt-shared tables)", "missing: %s" % missing)

# 浮動小数点の完全列化（#[fast_math]）: on の C が float / double の列・ベクトルの累計（f20_col_sum_f32 / _f64、条件付きは _m）・
# IEEE-754 の順序の鍵の列（f20_fkey_* と f20_col_pick_key）を使い、属性のないループは書いた順のまま。8 MiB 以上の列への一括
# 上書きは F20_NT_STORE_F32 / _F64。mirrored / off に浮動小数点の列は無い
# float True SoA (#[fast_math]): the "on" C uses float / double columns, vector accumulators (f20_col_sum_f32 / _f64, _m
# when filtered) and IEEE-754 order key columns (f20_fkey_* with f20_col_pick_key), loops without the attribute stay in
# written order; bulk overwrites of columns of 8 MiB or more use F20_NT_STORE_F32 / _F64; mirrored / off have no float
# columns
fl = ts_gen[("tsoa_float", "on", "auto")]
need = ["F20_ALIGN128 float ts_x[", "F20_ALIGN128 float ts_vx[", "F20_ALIGN128 double ts_mass[", "f20_col_sum_f64(arena->col.ts_mass, arena->live, n)", "f20_col_sum_f32(arena->col.ts_x, arena->live, n)", "f20_col_sum_f32_m(arena->col.ts_vx, arena->col.fb", "f20_fkey_f64((*v).charge, 0)", "f20_fkey_f64((*v).charge, 1)", "f20_col_pick_key(arena->col.min", "if (f20_f64_is_nan(*acc)) return;", "#define FASCON2_FLOAT 1"]
missing = [x for x in need if x not in fl]
report(not missing, "ok/tsoa_float.fsc (C: float / double True-SoA columns, vector sums, IEEE-754 key columns, NaN start)", "missing: %s" % missing)
report(all("ts_mass" not in ts_gen[("tsoa_float", cm, "auto")] and "f20_col_sum_f" not in ts_gen[("tsoa_float", cm, "auto")] for cm in ("mirrored", "off")), "ok/tsoa_float.fsc (--columns mirrored / off: no float True-SoA columns, loops in written order)")
ok = ts_gen[("tsoa_float_nt", "on", "auto")].count("F20_NT_STORE_F64(&arena_grid.col.ts_v[") == 1 and ts_gen[("tsoa_float_nt", "on", "auto")].count("F20_NT_STORE_F32(&arena_grid.col.ts_w[") == 1
ok = ok and "F20_NT_STORE" not in ts_gen[("tsoa_float_nt", "on", "off")]
report(ok, "ok/tsoa_float_nt.fsc (C: F20_NT_STORE_F32 / _F64 for float columns of 8 MiB or more)")
r = run([FASCON, "regions", os.path.join(ROOT, "tests", "ok", "tsoa_float.fsc"), "--lang", "en"])
ok = r.returncode == 0 and all(re.search(p, r.stdout) for p in (r"parts\.ts_x +True-SoA \(single-write\) +value column \(f32\)", r"parts\.ts_mass +True-SoA \(single-write\) +value column \(f64\)", r"parts\.min\d+_charge +Mirrored-SoA \(dual-write\) +float pick-key column \(min\)"))
report(bool(ok), "ok/tsoa_float.fsc (regions: float value columns and float pick-key columns)", r.stdout[-900:])

# 条件付き一括更新（Masked Column RMW）: on の C が f20_col_masked_add_* / _set_* を呼び、整数の加算は桁あふれした語から
# 逐次のループへ続ける。8 MiB 以上の列では全部 1 の語だけ非時間的ストア（最後の引数が 1）。mirrored / off では使わない。
# fascon regions に列・ビット列・カーネル・ストアの種類が出ること。カーネルの単体テスト（0 の語・全部 1 の語・ばらばらの語・
# 端数・-0.0・NaN・桁あふれの語）を -O2 と ASan / UBSan で走らせ、桁あふれのパニックが on と off で同じ場所・同じ出力のこと
# masked column RMW: the "on" C calls f20_col_masked_add_* / _set_*, and integer adds continue from an overflowing word
# with the sequential loop; columns of 8 MiB or more use non-temporal stores for all-one words only (last argument 1);
# mirrored / off never use them. fascon regions lists the column, the bit column, the kernel and the store kind. The
# kernel unit test (zero, all-one and mixed words, tails, -0.0, NaN, overflowing words) runs at -O2 and under ASan /
# UBSan, and the overflow panic happens at the same place with the same output for on and off
rm = ts_gen[("tsoa_rmw", "on", "auto")]
need = ["f20_col_masked_add_u32(arena_items.col.ts_hits, arena_items.col.fb0, ", "f20_col_masked_set_u64(arena_items.col.ts_total, arena_items.col.fb1, ", "f20_col_masked_add_i32(arena_items.col.ts_delta, arena_items.col.fb2, ", "f20_col_masked_set_i64(arena_items.col.ts_stamp, arena_items.col.fb3, ", "f20_col_masked_add_f32(arena_items.col.ts_heat, arena_items.col.fb0, ", "f20_col_masked_set_f64(arena_items.col.ts_level, arena_items.col.fb4, ", "if (FASCON_UNLIKELY(_stop"]
missing = [x for x in need if x not in rm]
if re.search(r"f20_col_masked_\w+\([^;]*, 1\);", rm):
    missing.append("no non-temporal stores below 8 MiB")
report(not missing, "ok/tsoa_rmw.fsc (C: f20_col_masked_add_* / _set_* for u32 / u64 / i32 / i64 / f32 / f64, overflow continues sequentially)", "missing: %s" % missing)
report(all("f20_col_masked_" not in ts_gen[("tsoa_rmw", cm, nm)] for cm in ("mirrored", "off") for nm in ("auto", "on", "off")), "ok/tsoa_rmw.fsc (--columns mirrored / off: no masked column RMW)")
fnt = ts_gen[("tsoa_float_nt", "on", "auto")]
report(re.search(r"f20_col_masked_add_f64\(arena_grid\.col\.ts_v, arena_grid\.col\.fb0, _n\d+, _x\d+, 1\);", fnt) is not None and re.search(r"f20_col_masked_add_f64\([^;]*, 0\);", ts_gen[("tsoa_float_nt", "on", "off")]) is not None, "ok/tsoa_float_nt.fsc (masked RMW: non-temporal all-one words at 8 MiB or more, plain with --nontemporal off)")
r = run([FASCON, "regions", os.path.join(ROOT, "tests", "ok", "tsoa_rmw.fsc"), "--lang", "en"])
lines = ["items.hits line 92: f20_col_masked_add_u32 (ts_hits & fb0) — plain stores", "items.total line 97: f20_col_masked_set_u64 (ts_total & fb1)", "items.delta line 102: f20_col_masked_add_i32 (ts_delta & fb2)", "items.stamp line 108: f20_col_masked_set_i64 (ts_stamp & fb3)", "items.heat line 115: f20_col_masked_add_f32 (ts_heat & fb0)", "items.level line 121: f20_col_masked_set_f64 (ts_level & fb4)"]
missing = [x for x in lines if x not in r.stdout]
r2 = run([FASCON, "regions", os.path.join(ROOT, "tests", "ok", "tsoa_float_nt.fsc"), "--lang", "en"])
if "grid.v line 32: f20_col_masked_add_f64 (ts_v & fb0) — all-one words use non-temporal stores" not in r2.stdout:
    missing.append("tsoa_float_nt grid.v non-temporal")
report(r.returncode == 0 and not missing, "ok/tsoa_rmw.fsc (regions: masked column RMW kernels, bit columns and store kinds)", "missing: %s" % missing)
if have_cc:
    CC = next(c for c in ("cc", "gcc", "clang") if shutil.which(c))
    d = os.path.join(tmp, "rmwk")
    os.makedirs(d, exist_ok=True)
    bad = []
    for tag, flags in (("O2", ["-O2"]), ("san", ["-O1", "-g", "-fsanitize=address,undefined", "-fno-sanitize-recover=all"])):
        exe = os.path.join(d, "rmw-" + tag)
        r = run([CC, "-std=gnu11", "-Wall", "-Wextra", "-Werror", "-DFASCON2_HOSTED_MMIO", "-I", os.path.join(ROOT, "runtime"), os.path.join(ROOT, "tests", "cols", "rmw_kernels.c"), "-o", exe] + flags)
        if r.returncode != 0:
            if tag == "san" and any(k in r.stderr for k in ("sanitize", "asan", "ubsan", "libclang_rt")):
                continue
            bad.append("%s: %s" % (tag, r.stderr[-300:]))
            continue
        r = run([exe], timeout=120)
        if r.returncode != 0 or r.stdout.strip() != "ok":
            bad.append("%s: rc=%d %s %s" % (tag, r.returncode, r.stdout[-200:], r.stderr[-300:]))
    report(not bad, "cols/rmw_kernels.c (masked RMW kernels against a scalar model: zero / all-one / mixed words, tails, -0.0, NaN, overflow; -O2 and ASan / UBSan)", "; ".join(bad))
    # 0 の語は列の行に触れない: 0 の語だけが覆う列のページを PROT_NONE にしてから 12 本の走査関数を走らせる（触れば SIGSEGV）。
    # 列と条件の語の直後にも PROT_NONE の番兵ページを置く（n の先を読まない）。Linux だけ（mmap / mprotect）
    # zero words never touch the column: the pages only zero words cover are PROT_NONE while each of the 12 kernels runs
    # (a touch is a SIGSEGV), with PROT_NONE guard pages right after the column and the filter words (no read past n).
    # Linux only (mmap / mprotect)
    if sys.platform.startswith("linux"):
        bad = []
        for tag, flags in (("O2", ["-O2"]), ("san", ["-O1", "-g", "-fsanitize=address,undefined", "-fno-sanitize-recover=all"])):
            exe = os.path.join(d, "rmwu-" + tag)
            r = run([CC, "-std=gnu11", "-Wall", "-Wextra", "-Werror", "-DFASCON2_HOSTED_MMIO", "-I", os.path.join(ROOT, "runtime"), os.path.join(ROOT, "tests", "cols", "rmw_untouched.c"), "-o", exe] + flags)
            if r.returncode != 0:
                if tag == "san" and any(k in r.stderr for k in ("sanitize", "asan", "ubsan", "libclang_rt")):
                    continue
                bad.append("%s: %s" % (tag, r.stderr[-300:]))
                continue
            r = run([exe], timeout=300)
            if r.returncode != 0 or not r.stdout.strip().startswith("ok"):
                bad.append("%s: rc=%d %s %s" % (tag, r.returncode, r.stdout[-300:], r.stderr[-300:]))
        report(not bad, "cols/rmw_untouched.c (all 12 masked RMW kernels, nt 0 / 1: zero words never load or store the column (PROT_NONE pages), no read past n, overflowing words untouched; -O2 and ASan / UBSan)", "; ".join(bad))
    src = os.path.join(ROOT, "tests", "cols", "rmw_overflow.fsc")
    res = {}
    for cm in ("on", "off"):
        r = run([FASCON, "run", src, "--out-dir", os.path.join(tmp, "rmw-ov-" + cm), "--columns", cm], timeout=300)
        res[cm] = (r.returncode, r.stdout, r.stderr)
    gen = os.path.join(tmp, "rmw-ov-on", "rmw_overflow.fascon.c")
    used = os.path.isfile(gen) and "f20_col_masked_add_u32(" in open(gen, encoding="utf-8").read()
    ok = used and res["on"] == res["off"] and res["on"][0] == 70 and "add overflow" in res["on"][2]
    report(ok, "cols/rmw_overflow.fsc (an overflowing masked add panics at the same slot with the same output: on == off, rc 70)", "on=%r off=%r used=%s" % (res["on"][:2], res["off"][:2], used))

# gpu_arena: 生成 C は fascon_plat_gpu_* の 4 つだけを（runtime の f20_gpu_* を通して）使い、CUDA・Vulkan・libnuma・
# pthread・インラインアセンブリを直接書かないこと。PC の真似（ワーカースレッド）を ASan + UBSan と TSan で走らせて
# ok/gpu_offload.expected と同じ出力で、--gpu=off（その場で走る・F2707 の警告）も同じ出力。fascon regions の報告と、
# パッケージの安全性ラベル（"GPU / Accelerator offload"）
# gpu_arena: the generated C uses only the four fascon_plat_gpu_* hooks (through the runtime's f20_gpu_*) and writes no
# CUDA, Vulkan, libnuma, pthread or inline assembly itself. The PC emulation (a worker thread) runs under ASan + UBSan and
# TSan with the output of ok/gpu_offload.expected, and --gpu=off (in place, warning F2707) prints the same. Also the
# fascon regions report and the package safety label ("GPU / Accelerator offload")
src = os.path.join(ROOT, "tests", "ok", "gpu_offload.fsc")
expected = open(os.path.join(ROOT, "tests", "ok", "gpu_offload.expected"), encoding="utf-8").read()
out = os.path.join(tmp, "gpu-gen")
r = run([FASCON, "build", src, "--out-dir", out])
gen = open(os.path.join(out, "gpu_offload.fascon.c"), encoding="utf-8").read() if r.returncode == 0 else ""
BANNED_C = (r"\bpthread_", r"\bcuda[A-Z_]", r"\bcu[A-Z][a-z]", r"\bvk[A-Z]", r"\bnuma_", r"\bmbind\b", r"\bset_mempolicy\b", r"\bsched_setaffinity\b", r"\bSetThreadAffinityMask\b", r"__asm__", r"\basm\s*\(", r"\basm\s+volatile")


def c_code_only(c):
    """注釈と文字列を除いた C（禁止した API の検査用） / C without comments and strings (for the banned-API check)"""
    c = re.sub(r"/\*.*?\*/", " ", c, flags=re.S)
    c = re.sub(r"//[^\n]*", " ", c)
    return re.sub(r'"(?:[^"\\\n]|\\.)*"', '""', c)


banned = [w for w in BANNED_C if re.search(w, c_code_only(gen))]
ok = bool(gen) and not banned and "#define FASCON2_GPU 1" in gen and "f20_gpu_dispatch(ARENA_parts_GPU_ID, i, kernel, grid, block, &arena_parts.slot[i].v)" in gen and "F20_GPU_STORAGE(f20_arena_t(parts))" in gen and "const FasconGpuArena fascon2_gpu_arenas[]" in gen
report(ok, "ok/gpu_offload.fsc (C: .bss.fascon_gpu table, dispatch / reclaim through f20_gpu_* -> fascon_plat_gpu_*, no direct thread / GPU / NUMA API or asm)", "banned: %s" % banned)
r = run([FASCON, "run", src, "--out-dir", os.path.join(tmp, "gpu-off"), "--gpu=off", "--lang", "en"]) if have_cc else None
if r is not None:
    goff = os.path.join(tmp, "gpu-off", "gpu_offload.fascon.c")
    ok = r.returncode == 0 and r.stdout == expected and "warning[F2707]" in r.stderr and os.path.isfile(goff) and "#define FASCON2_GPU_OFF 1" in open(goff, encoding="utf-8").read()
    report(ok, "ok/gpu_offload.fsc (--gpu=off: kernels run in place, same output, warning F2707)", r.stderr[-400:] + r.stdout[-200:])
if have_cc and shutil.which("gcc"):
    bad = []
    for tag, flags in (("asan", ["-fsanitize=address,undefined", "-fno-sanitize-recover=all"]), ("tsan", ["-fsanitize=thread"])):
        exe = os.path.join(out, "gpu-" + tag)
        r = run(["gcc", "-std=c11", "-O1", "-g", "-pthread", "-DFASCON2_HOSTED_MMIO", "-DFASCON2_GPU=1", "-I" + out, os.path.join(out, "gpu_offload.fascon.c"), os.path.join(ROOT, "bsp", "host", "fascon2_plat_host.c"), os.path.join(ROOT, "tests", "ok", "gpu_offload.gpu.c"), "-o", exe] + flags)
        if r.returncode != 0:
            if any(k in r.stderr for k in ("sanitize", "asan", "tsan", "libclang_rt")):
                continue
            bad.append("%s build: %s" % (tag, r.stderr[-300:]))
            continue
        r = run([exe], timeout=120)
        if r.returncode != 0 or r.stdout != expected or "WARNING: ThreadSanitizer" in r.stderr or "ERROR: AddressSanitizer" in r.stderr:
            bad.append("%s: rc=%d %s" % (tag, r.returncode, r.stderr[-400:]))
    report(not bad, "ok/gpu_offload.fsc (PC GPU emulation under ASan + UBSan and TSan: no races, no memory errors)", "; ".join(bad))
r = run([FASCON, "regions", src, "--lang", "en"])
report(r.returncode == 0 and "gpu_arena: .bss.fascon_gpu (2 MiB aligned, pinned); dispatch / reclaim / reclaim_wait go through fascon_plat_gpu_register / dispatch / poll / wait only" in r.stdout, "ok/gpu_offload.fsc (regions: gpu_arena placement and hooks)", r.stdout[-600:])
pk = os.path.join(tmp, "gpu-pkg")
os.makedirs(pk, exist_ok=True)
with open(os.path.join(pk, "fascon.toml"), "w", encoding="utf-8") as fh:
    fh.write('[package]\nname = "gpkg"\nversion = "1.0.0"\n')
shutil.copy(src, os.path.join(pk, "main.fsc"))
r = run([FASCON, "build", os.path.join(pk, "main.fsc"), "--out-dir", os.path.join(pk, "build"), "--lang", "en"])
toml = open(os.path.join(pk, "fascon.toml"), encoding="utf-8").read()
report(r.returncode == 0 and 'safety = "Low-Level / Hardware"' in toml and "GPU / Accelerator offload (main.fsc:14)" in toml and "GPU / Accelerator offload (main.fsc:27)" in toml, "pkg safety label (gpu_arena / .dispatch: GPU / Accelerator offload)", toml[-500:])

# 標準ライブラリのモジュール（net::tcp は net::wire を取り込む）を使っても、今いるフォルダに <std>/ を作らない（キャッシュは
# 取り込んだファイルのフォルダの .fascon-cache/ だけ）
# a standard library module that imports another one (net::tcp imports net::wire) leaves no <std>/ folder in the current
# folder (the only cache is .fascon-cache/ next to the importing file)
sd = os.path.join(tmp, "std-cache")
os.makedirs(sd, exist_ok=True)
shutil.copy(os.path.join(ROOT, "tests", "ok", "lib_net.fsc"), sd)
r = run([FASCON, "build", "lib_net.fsc", "--engine", "1", "--out-dir", "out", "--lang", "en"], cwd=sd)
report(r.returncode == 0 and not os.path.exists(os.path.join(sd, "<std>")) and os.path.isdir(os.path.join(sd, ".fascon-cache", "modules")), "module cache: no <std>/ folder in the current folder for standard library modules", r.stderr[-400:] + " " + " ".join(sorted(os.listdir(sd))))

# --target hpc: 生成 C が FASCON2_HPC・64 ビット Id・計算ノードの論理コアの数を定義し、各コアの入口（main と spawn_on の
# 入口）で fascon_plat_pin_core / fascon_plat_numa_node / fascon_plat_numa_bind だけを呼ぶ（sched_setaffinity・mbind・
# libnuma・pthread を直接書かない）。#[numa_node(1)] の共有の表は main が固定する。fascon regions の HPC の節、F3901 の
# 警告の場所と #[allow(hpc_io)] での解除
# --target hpc: the generated C defines FASCON2_HPC, 64-bit Ids and the node's logical core count, and at each core's
# entry (main and spawn_on entries) calls only fascon_plat_pin_core / fascon_plat_numa_node / fascon_plat_numa_bind (no
# direct sched_setaffinity, mbind, libnuma or pthread). main binds the #[numa_node(1)] shared table. Also the HPC section
# of fascon regions, where the F3901 warning lands and its lifting with #[allow(hpc_io)]
src = os.path.join(ROOT, "tests", "ok", "hpc_compute.fsc")
out = os.path.join(tmp, "hpc-gen")
r = run([FASCON, "build", src, "--out-dir", out, "--lang", "en"])
gen = open(os.path.join(out, "hpc_compute.fascon.c"), encoding="utf-8").read() if r.returncode == 0 else ""
need = ["#define FASCON2_HPC 1", "#define FASCON2_ID64 1", "#define FASCON2_MAX_CPUS 8u", "static FASCON_MAYBE_UNUSED void f20_hpc_core_enter(void) {", "fascon_plat_pin_core(core);", "const uint32_t node = fascon_plat_numa_node(core);", "(void)fascon_plat_numa_bind((void *)&arena_part, sizeof arena_part, node);", "(void)fascon_plat_numa_bind((void *)&fascon__cpu[core % FASCON2_MAX_CPUS], sizeof fascon__cpu[0], node);", "(void)fascon_plat_numa_bind((void *)&arena_totals, sizeof arena_totals, 1u);", "static void fsc_spawn_worker(uint64_t a) {\n    (void)a;\n    f20_hpc_core_enter();"]
missing = [x for x in need if x not in gen]
missing += ["banned " + w for w in BANNED_C if re.search(w, c_code_only(gen))]
report(bool(gen) and not missing, "ok/hpc_compute.fsc (C: FASCON2_HPC, 64-bit Ids, pin + NUMA bind at every core entry through fascon_plat_* only, #[numa_node] table)", "missing: %s" % missing)
r = run([FASCON, "regions", src, "--lang", "en"])
lines = ["--target hpc: compute node cores=8 smt=2 numa=2 (// fascon-topology:), #[no_smt]", "core 1   compute root: `worker` (line 72) — physical core 1, NUMA node 0", "core 2   compute root: `worker` (line 73) — physical core 2, NUMA node 1", "#[numa_node(1)] totals → NUMA node 1", "functions reachable from compute cores: worker, beacon", "F3901 I/O reachable from compute cores: the I/O port `post` — `beacon` (line 66) (lifted by #[allow(hpc_io)])", "Id = 64 bits [generation 32 bits | index 32 bits]"]
missing = [x for x in lines if x not in r.stdout]
report(r.returncode == 0 and not missing, "ok/hpc_compute.fsc (regions: compute node, core roles, NUMA nodes, reachable functions, lifted F3901)", "missing: %s" % missing)
r = run([FASCON, "check", os.path.join(ROOT, "tests", "hpc", "io_noise.fsc"), "--json", "--lang", "en"])
try:
    ds = [(d["code"], d["severity"], d.get("line")) for d in json.loads(r.stdout)]
except Exception as e:  # noqa: BLE001
    ds = ["<%s>" % e]
r2 = run([FASCON, "check", src, "--json", "--lang", "en"])
try:
    d2 = [d["code"] for d in json.loads(r2.stdout)]
except Exception as e:  # noqa: BLE001
    d2 = ["<%s>" % e]
report(ds == [("F3901", "warning", 11)] and "F3901" not in d2, "hpc/io_noise.fsc (F3901 warning on the port write reached from compute core 1; main's gpu_dispatch and #[allow(hpc_io)] are quiet)", "%s / %s" % (ds, d2))

# fascon-gc とローカルの own: 生成 C の形（own の先まで辿る記述子で根にする・解放の前に持ち主を NULL にする・フレームの
# 領域を戻す前に NULL・戻り値の入れ物も根・一時値は受け手へ渡すときに根から外す）。gc_local_own_roots.fsc・gc_shared_own.fsc・
# gc_own_refresh.fsc と既存の gc のテストを、AddressSanitizer + UBSan + LeakSanitizer（-DFASCON2_HEAP_MALLOC: own の確保を
# malloc に）と別名の検査（-DFASCON2_GC_ALIAS_CHECK: 1 回の走査で同じ own へ 2 度届いたら止まる）で走らせ、共有の gc_arena は
# ThreadSanitizer でも走らせる（データ競合なし・同じ出力）
# fascon-gc and local owns: the shape of the C (roots registered with descriptors that follow own, owners cleared to NULL
# before freeing, NULL before the frame space is popped, return slots as roots, temporaries leaving the roots when handed
# over). gc_local_own_roots.fsc, gc_shared_own.fsc, gc_own_refresh.fsc and the existing gc tests run under AddressSanitizer
# + UBSan + LeakSanitizer (-DFASCON2_HEAP_MALLOC: own allocations through malloc) with the alias check
# (-DFASCON2_GC_ALIAS_CHECK: stop when one walk reaches the same own twice); the shared gc_arena tests also run under
# ThreadSanitizer (no data race, the same output)
src = os.path.join(ROOT, "tests", "ok", "gc_local_own_roots.fsc")
out = os.path.join(tmp, "gc-own-gen")
r = run([FASCON, "build", src, "--out-dir", out, "--lang", "en"])
gen = os.path.join(out, "gc_local_own_roots.fascon.c")
text = open(gen, encoding="utf-8").read() if os.path.isfile(gen) else ""
need = ["F20_GC_ROOT(0, &h, &fsc_td_Own_Holder);", "fsc_own_drop_at_Holder(&", "*pp = NULL;\n        fascon_own_free(x);", "f = NULL; /* fascon-gc: off the roots before the frame space is popped */", "(a root through own) */", "/* the own temporaries go to their receiver (removed from the roots; from here the receiver is the root) */", "(an #[on_drop] may collect) */"]
missing = [x for x in need if x not in text]
report(r.returncode == 0 and not missing, "ok/gc_local_own_roots.fsc (C: own roots with F20_GC_OWN descriptors, NULL before free, frame NULL before pop, return slots, hand-over of temporaries)", "missing: %s" % missing)
if have_cc and shutil.which("gcc") and not sys.platform.startswith("win"):
    env = dict(os.environ)
    env["ASAN_OPTIONS"] = "detect_leaks=1:abort_on_error=1"
    env["UBSAN_OPTIONS"] = "halt_on_error=1:print_stacktrace=1"
    env["CC"] = "gcc"
    bad = []
    tried = 0
    for f in ("gc_local_own_roots.fsc", "gc_shared_own.fsc", "gc_own_refresh.fsc", "gc_basic.fsc", "gc_own_promote.fsc", "gc_temp_roots.fsc", "gc_chan_roots.fsc", "gc_shared.fsc", "gc_auto_promote.fsc"):
        e = dict(env)
        e["FASCON_CFLAGS"] = "-O1 -g -fsanitize=address,undefined -fno-sanitize-recover=all -fno-omit-frame-pointer -DFASCON2_HEAP_MALLOC -DFASCON2_GC_ALIAS_CHECK"
        r = run([FASCON, "run", os.path.join(ROOT, "tests", "ok", f), "--out-dir", os.path.join(tmp, "gc-san-" + f), "--lang", "en"], env=e, timeout=600)
        if r.returncode != 0 and any(k in r.stderr for k in ("cannot find", "libasan", "libubsan", "unrecognized")):
            continue
        tried += 1
        exp = open(os.path.join(ROOT, "tests", "ok", f[:-4] + ".expected"), encoding="utf-8").read()
        if r.returncode != 0 or r.stdout != exp:
            bad.append("%s: rc=%d %s" % (f, r.returncode, (r.stderr + r.stdout)[-300:]))
    report(not bad, "fascon-gc under ASan + UBSan + LeakSanitizer + alias check (%d programs: local own roots, shared, refresh, the earlier gc tests)" % tried, "; ".join(bad))
    bad = []
    tried = 0
    for f in ("gc_shared_own.fsc", "gc_shared.fsc"):
        e = dict(env)
        e["FASCON_CFLAGS"] = "-O1 -g -fsanitize=thread"
        d = os.path.join(tmp, "gc-tsan-" + f)
        r = run([FASCON, "run", os.path.join(ROOT, "tests", "ok", f), "--out-dir", d, "--lang", "en"], env=e, timeout=600)
        if r.returncode != 0 and any(k in r.stderr for k in ("cannot find", "libtsan", "unrecognized")):
            continue
        tried += 1
        exp = open(os.path.join(ROOT, "tests", "ok", f[:-4] + ".expected"), encoding="utf-8").read()
        exe = os.path.join(d, f[:-4])
        for k in range(5):
            if k > 0:
                r = run([exe], timeout=300)
            if r.returncode != 0 or r.stdout != exp or "ThreadSanitizer" in r.stderr:
                bad.append("%s run %d: rc=%d %s" % (f, k, r.returncode, r.stderr[-400:]))
                break
    report(not bad, "fascon-gc shared gc_arena under ThreadSanitizer (%d programs x 5 runs: no data race, same output)" % tried, "; ".join(bad))

# fascon_put_f64 / _f32: 乱数のビットの並み 40 万個を、Python の repr と Decimal で確かめる（往復・最短・近い方・形）
# fascon_put_f64 / _f32: 400,000 random bit patterns checked with Python's repr and Decimal (round trip, shortest,
# nearest, shape)
if have_cc:
    CC = next(c for c in ("cc", "gcc", "clang") if shutil.which(c))
    d = os.path.join(tmp, "fmtfuzz")
    os.makedirs(d, exist_ok=True)
    exe = os.path.join(d, "fmtfuzz")
    r = run([CC, "-O2", "-std=c11", "-DFASCON2_HOSTED_MMIO", "-I", os.path.join(ROOT, "runtime"), os.path.join(ROOT, "tests", "float", "fmtfuzz.c"), os.path.join(ROOT, "bsp", "host", "fascon2_plat_host.c"), "-o", exe])
    ok, detail = r.returncode == 0, r.stderr
    if ok:
        r = run([exe], timeout=120)
        with open(os.path.join(d, "out.txt"), "w") as fh:
            fh.write(r.stdout)
        c = run([sys.executable, "-I", os.path.join(ROOT, "tests", "float", "fmtcheck.py"), os.path.join(d, "out.txt")], timeout=300)
        ok, detail = r.returncode == 0 and c.returncode == 0, c.stdout
    report(ok, "float/fmtfuzz.c (fascon_put_f64 / _f32: shortest round-trip decimals, ties to even, Rust {:?} shape)", detail[-600:])

# ランダムなプログラム（要素の形・集計・書き込みの道筋がばらばら）を --columns on / off で走らせて比べる
# random programs (random element layouts, reductions and write paths) run with --columns on / off and compared
if have_cc and FUZZ > 0:
    bad = []
    for seed in range(1, FUZZ + 1):
        d = os.path.join(tmp, "fuzz-%d" % seed)
        os.makedirs(d, exist_ok=True)
        p = os.path.join(d, "p.fsc")
        run([sys.executable, "-I", os.path.join(ROOT, "tests", "cols", "colfuzz.py"), str(seed * 7919 + 13), p])
        res = []
        for mode in ("on", "mirrored", "off"):
            r = run([FASCON, "run", p, "--engine", "1", "--out-dir", os.path.join(d, mode), "--columns", mode], timeout=300)
            res.append((r.returncode, r.stdout, r.stderr))
        if res[0] != res[1] or res[0] != res[2] or res[0][0] not in (0, 70):
            bad.append(seed)
    report(not bad, "cols/colfuzz.py (%d random programs, --columns on == mirrored == off)" % FUZZ, "seeds %s differ (see %s)" % (bad, tmp))
    # 完全列化のモード: 完全列化を壊す使い方を外したプログラムを on / on + --nontemporal on / mirrored / off で比べる
    # the True-SoA mode: programs without the uses that rule True SoA out, compared across on / on + --nontemporal on /
    # mirrored / off
    bad = []
    for seed in range(1, FUZZ + 1):
        d = os.path.join(tmp, "tsfuzz-%d" % seed)
        os.makedirs(d, exist_ok=True)
        p = os.path.join(d, "p.fsc")
        run([sys.executable, "-I", os.path.join(ROOT, "tests", "cols", "colfuzz.py"), str(seed * 7919 + 13), p, "--tsoa"])
        res = []
        for k, mode in enumerate((["--columns", "on"], ["--columns", "on", "--nontemporal", "on"], ["--columns", "mirrored"], ["--columns", "off"])):
            r = run([FASCON, "run", p, "--engine", "1", "--out-dir", os.path.join(d, str(k))] + mode, timeout=300)
            res.append((r.returncode, r.stdout, r.stderr))
        if any(x != res[0] for x in res) or res[0][0] not in (0, 70):
            bad.append(seed)
    report(not bad, "cols/colfuzz.py --tsoa (%d random programs, --columns on == on + --nontemporal on == mirrored == off)" % FUZZ, "seeds %s differ (see %s)" % (bad, tmp))
    # 浮動小数点のモード: NaN・-0.0・無限大を含む f32 / f64 の総和と最小 / 最大を、#[fast_math] あり・なしの 2 通りで書いた
    # プログラムを on / on + --nontemporal on / mirrored / off で比べ、同じ実行の中でも 2 通りの結果が同じこと
    # the float mode: f32 / f64 sums and min / max over values with NaN, -0.0 and infinities, each written with and without
    # #[fast_math]; on / on + --nontemporal on / mirrored / off must agree, and so must the two forms within one run
    bad = []
    for seed in range(1, FUZZ + 1):
        d = os.path.join(tmp, "flfuzz-%d" % seed)
        os.makedirs(d, exist_ok=True)
        p = os.path.join(d, "p.fsc")
        run([sys.executable, "-I", os.path.join(ROOT, "tests", "cols", "colfuzz.py"), str(seed * 7919 + 13), p, "--float"])
        nred = len(re.findall(r"^fn r\d+f\(", open(p, encoding="utf-8").read(), re.M))
        res = []
        for k, mode in enumerate((["--columns", "on"], ["--columns", "on", "--nontemporal", "on"], ["--columns", "mirrored"], ["--columns", "off"])):
            r = run([FASCON, "run", p, "--engine", "1", "--out-dir", os.path.join(d, str(k))] + mode, timeout=300)
            res.append((r.returncode, r.stdout, r.stderr))
        pairs_ok = True
        lines = res[0][1].split("\n")
        i = 0
        while i < len(lines) and lines[i] != "":
            blk = lines[i + 1 : i + 1 + 2 * nred]
            if len(blk) < 2 * nred or any(blk[2 * j] != blk[2 * j + 1] for j in range(nred)):
                pairs_ok = False
                break
            i += 2 * nred + 2
        if any(x != res[0] for x in res) or res[0][0] != 0 or not pairs_ok:
            bad.append(seed)
    report(not bad, "cols/colfuzz.py --float (%d random programs: NaN / -0.0 / inf, #[fast_math] == written order, on == on + nontemporal == mirrored == off)" % FUZZ, "seeds %s differ (see %s)" % (bad, tmp))
    # 条件付き一括更新のモード: u32 / u64 / i32 / i64 / f32 / f64 の列への `if 条件 { += / = }` を確保・解放と混ぜ、生成 C が
    # 実際に f20_col_masked_* を使うことも確かめる
    # the masked RMW mode: `if cond { += / = }` on u32 / u64 / i32 / i64 / f32 / f64 columns mixed with allocs and frees;
    # the generated C must actually use f20_col_masked_*
    bad = []
    unused = []
    for seed in range(1, FUZZ + 1):
        d = os.path.join(tmp, "rmwfuzz-%d" % seed)
        os.makedirs(d, exist_ok=True)
        p = os.path.join(d, "p.fsc")
        run([sys.executable, "-I", os.path.join(ROOT, "tests", "cols", "colfuzz.py"), str(seed * 7919 + 13), p, "--rmw"])
        res = []
        for k, mode in enumerate((["--columns", "on"], ["--columns", "on", "--nontemporal", "on"], ["--columns", "mirrored"], ["--columns", "off"])):
            r = run([FASCON, "run", p, "--engine", "1", "--out-dir", os.path.join(d, str(k))] + mode, timeout=300)
            res.append((r.returncode, r.stdout, r.stderr))
        g = os.path.join(d, "0", "p.fascon.c")
        if not (os.path.isfile(g) and "f20_col_masked_" in open(g, encoding="utf-8").read()):
            unused.append(seed)
        if any(x != res[0] for x in res) or res[0][0] != 0:
            bad.append(seed)
    report(not bad and not unused, "cols/colfuzz.py --rmw (%d random programs: masked += / = on six column types, on == on + nontemporal == mirrored == off)" % FUZZ, "seeds %s differ, %s without kernels (see %s)" % (bad, unused, tmp))

# 辞書: 監査（出しうるエラー番号がすべて載っている・日英がそろう・拡張の辞書が同じ文を持つ）と、すべての項目と別名が
# `fascon dict` で日英とも引けること。`fascon explain` が tests/err のどのファイルにも 3 つのステップを出すこと
# the dictionary: the audit (every error code the compiler can emit has an entry, both languages are present, the
# extension dictionaries hold the same text), every entry and alias found by `fascon dict` in both languages, and
# `fascon explain` printing its three steps for every file in tests/err
r = run([sys.executable, "-I", os.path.join(ROOT, "tools", "check_dictionary.py")])
report(r.returncode == 0, "tools/check_dictionary.py (dictionary audit)", r.stdout[-1500:] + r.stderr[-300:])
r = run([sys.executable, "-I", os.path.join(ROOT, "tools", "sync_dictionary.py"), "--check"])
report(r.returncode == 0, "tools/sync_dictionary.py --check (VS Code extension dictionaries in sync)", r.stdout[-500:] + r.stderr[-300:])
with open(os.path.join(ROOT, "data", "dictionary.json"), encoding="utf-8") as fh:
    dict_entries = json.load(fh)
for lang in ("ja", "en"):
    r = run([FASCON, "dict", "--json", "--lang", lang])
    try:
        listed = [x["name"] for x in json.loads(r.stdout)]
    except ValueError:
        listed = []
    misses = []
    for e in dict_entries:
        for term in [e["name"]] + e["aliases"]:
            q = run([FASCON, "dict", "--json", "--lang", lang, "--", term])
            try:
                found = [x["name"] for x in json.loads(q.stdout)]
            except ValueError:
                found = []
            if e["name"] not in found:
                misses.append(term)
    ok = listed == [e["name"] for e in dict_entries] and not misses
    report(ok, "fascon dict --lang %s (all %d entries and their aliases)" % (lang, len(dict_entries)), "listed %d, not found: %s" % (len(listed), misses[:10]))
explain_bad = []
for f in sorted(os.listdir(os.path.join(ROOT, "tests", "err"))):
    if f.endswith(".fsc"):
        path = os.path.join(ROOT, "tests", "err", f)
        head = open(path, encoding="utf-8").read().split("\n")[:2]
        code = head[0].split("expect:")[1].strip()
        env = dict(os.environ)
        if len(head) > 1 and "env:" in head[1]:
            k, v = head[1].split("env:")[1].strip().split("=", 1)
            env[k.strip()] = v.strip()
        r = run([FASCON, "explain", path, "--code", code, "--lang", "en"], env=env)
        if not all(s in r.stdout for s in ("## " + code, "Step 1", "Step 2", "Step 3")):
            explain_bad.append(f)
report(not explain_bad, "fascon explain on every tests/err file (timeline, fix, explanation)", "missing steps: %s" % explain_bad[:10])

for f, marker in (("kernel_core.fsc", "FASCON-KERNEL-OK"), ("gui_desktop.fsc", "FASCON-GUI-READY 3"), ("gui_dual.fsc", "FASCON-GUI-READY 3"), ("bios_dual.fsc", "FASCON-BIOS-OK"), ("rust_style.rs", "FASCON-RUST-OK"), ("app_wordfreq.fsc", "  the: 6"), ("own_callbacks.fsc", "FASCON-CALLBACK-OK"), ("smp_kernel.fsc", "FASCON-SMP-OK")):
    path = os.path.join(ROOT, "examples", f)
    r = run([FASCON, "check", path, "--lang", "en"])
    report(r.returncode == 0, "examples/" + f + " (check)", r.stderr[:400])
    r = run([FASCON, "regions", path, "--lang", "en"])
    report(r.returncode == 0 and "lifetime annotations in the source (count of `'`): 0" in r.stdout, "examples/" + f + " (zero apostrophes)", r.stdout[-300:])
    if have_cc:
        out = os.path.join(tmp, "host-" + f)
        r = run([FASCON, "run", path, "--out-dir", out])
        ok = r.returncode == 0 and marker in r.stdout
        if f.startswith("gui"):
            ok = ok and os.path.isfile(os.path.join(out, stem_of(f) + ".png"))
        report(ok, "examples/" + f + " (run on PC)", r.stderr[-400:])
    if QEMU and f in ("kernel_core.fsc", "gui_desktop.fsc", "own_callbacks.fsc"):
        out = os.path.join(tmp, "qemu-" + f)
        cmd = [FASCON, "qemu", path, "--out-dir", out, "--timeout", "90"]
        if f.startswith("gui"):
            cmd += ["--screenshot", os.path.join(out, "shot.png"), "--frames", "3"]
        r = run(cmd, stdin=subprocess.DEVNULL, timeout=180)
        ok = r.returncode == 0 and marker in r.stdout
        if f.startswith("gui"):
            ok = ok and all(os.path.isfile(os.path.join(out, "shot-%d.png" % i)) for i in (1, 2, 3))
        report(ok, "examples/" + f + " (QEMU, no BIOS)", r.stderr[-600:])

if QEMU:
    # 大きなアリーナ（2 MiB 境界の .fascon_arena）とフレームリージョンを BIOS なしの QEMU で
    # a large arena (the 2 MiB-aligned .fascon_arena) and the frame region on BIOS-less QEMU
    # カーネル拡張: タイマー割り込みによるタスクの切り替え（GICv2 + 仮想タイマー）と閉じた区間
    # kernel extensions: task switching by the timer interrupt (GICv2 + the virtual timer) and closed sections
    for f, expect in (("examples/vruntime_scan.fsc", "1497000\n1494003\n"), ("tests/ok/frame.fsc", None), ("tests/cols/cols.fsc", None), ("tests/ok/kernel_preempt.fsc", None), ("tests/ok/critical.fsc", None), ("tests/ok/smp_basic.fsc", None), ("tests/ok/smp_irq_tasks.fsc", None), ("tests/ok/smp_dma_ring.fsc", None)):
        path = os.path.join(ROOT, f)
        if expect is None:
            expect = open(path[:-4] + ".expected", encoding="utf-8").read()
        out = os.path.join(tmp, "qemu-" + os.path.basename(f))
        r = run([FASCON, "qemu", path, "--out-dir", out, "--timeout", "90"], stdin=subprocess.DEVNULL, timeout=180)
        body = "".join(l + "\n" for l in r.stdout.replace("\r", "").splitlines() if not l.startswith("[bsp]") and not l.startswith("[fascon]") and not l.startswith("generated:"))
        ok = r.returncode == 0 and body == expect and "no 4 KiB pages" in r.stdout
        if f.startswith("examples"):
            ok = ok and "2 MiB aligned" in r.stdout
        report(ok, f + " (QEMU, no BIOS)", r.stdout[-600:] + r.stderr[-300:])

if QEMU:
    # QEMU でだけ動く tests/ok（`// fascon-test: qemu-only`）: ユーザープログラム（<名前>_init.fsc）があれば先に像を作り直し、
    # カーネルを起動して、[bsp] などの行を除いた出力が .expected と一致すること
    # QEMU-only tests/ok (`// fascon-test: qemu-only`): rebuild the user program image first when <name>_init.fsc exists,
    # boot the kernel, and the output without [bsp] lines must match .expected
    for f in qemu_only_ok:
        stem = f[:-4]
        out = os.path.join(tmp, "qemu-ok-" + stem)
        os.makedirs(out, exist_ok=True)
        shutil.copy(os.path.join(ROOT, "tests", "ok", f), out)
        user = os.path.join(ROOT, "tests", "ok", stem + "_init.fsc")
        ok = True
        if os.path.isfile(user):
            shutil.copy(user, out)
            r = run([FASCON, "build", os.path.join(out, stem + "_init.fsc"), "--out", os.path.join(out, stem + "_init.bin"), "--out-dir", os.path.join(out, "u")], stdin=subprocess.DEVNULL, timeout=180)
            ok = r.returncode == 0 and os.path.isfile(os.path.join(out, stem + "_init.bin"))
            report(ok, "ok/%s_init.fsc (user program image for EL0)" % stem, r.stdout[-300:] + r.stderr[-300:])
        if ok:
            r = run([FASCON, "qemu", os.path.join(out, f), "--out-dir", os.path.join(out, "k"), "--timeout", "150"], stdin=subprocess.DEVNULL, timeout=300)
            body = "".join(l + "\n" for l in r.stdout.replace("\r", "").splitlines() if not l.startswith("[bsp]") and not l.startswith("[fascon]") and not l.startswith("generated:") and not l.startswith("[genos]"))
            expect = open(os.path.join(ROOT, "tests", "ok", stem + ".expected"), encoding="utf-8").read()
            report(r.returncode == 0 and body == expect, "ok/%s (QEMU)" % f, r.stdout[-900:] + r.stderr[-300:])

if QEMU:
    # カーネル拡張 Step 2: virtio-blk + ブロックキャッシュ + ファイル。同じディスク像で 2 回起動し、2 回目に 1 回目の内容が
    # 残っていること（formatted → mounted、boots=1 → boots=2）
    # kernel extensions step 2: virtio-blk + block cache + files. Boot twice on the same disk image; the second boot must
    # find what the first one wrote (formatted → mounted, boots=1 → boots=2)
    disk = os.path.join(tmp, "storage-disk.img")
    with open(disk, "wb") as fh:
        fh.write(b"\0" * (512 * 128))
    outs = []
    for k in (1, 2):
        out = os.path.join(tmp, "qemu-storage-%d" % k)
        r = run([FASCON, "qemu", os.path.join(ROOT, "examples", "kernel_storage.fsc"), "--disk", disk, "--out-dir", out, "--timeout", "90"], stdin=subprocess.DEVNULL, timeout=180)
        outs.append(r.stdout.replace("\r", ""))
    ok = "formatted a new disk" in outs[0] and "boots=1\n" in outs[0] and "mounted: files=2" in outs[1] and "boots=2\n" in outs[1]
    ok = ok and all("pattern.bin: 700 bytes written and read back" in o and "errors: 0" in o for o in outs)
    report(ok, "examples/kernel_storage.fsc (QEMU AArch64, virtio-blk: persistent across two boots)", outs[0][-300:] + outs[1][-300:])

if QEMU:
    # カーネル拡張 Step 3: virtio-net。相手役（tests/net_peer.py）が UDP で生の Ethernet フレームを流し、ARP 応答と
    # ICMP エコー応答 3 つ（32・301・1400 バイト）の中身とチェックサムを確かめる
    # kernel extensions step 3: virtio-net. The peer (tests/net_peer.py) exchanges raw Ethernet frames over UDP and checks
    # the ARP reply and three ICMP echo replies (32, 301 and 1400 bytes): contents and checksums
    out = os.path.join(tmp, "qemu-net")
    r = run([sys.executable, os.path.join(ROOT, "tests", "net_peer.py"), FASCON, os.path.join(ROOT, "examples", "kernel_net.fsc"), "--out-dir", out], stdin=subprocess.DEVNULL, timeout=240)
    ok = r.returncode == 0 and "ARP reply: 10.0.2.15 is at 52:54:00:12:34:56" in r.stdout and "echo replies=3 dropped=0" in r.stdout and r.stdout.count("returned intact, checksums valid") == 3
    report(ok, "examples/kernel_net.fsc (QEMU AArch64, virtio-net: ARP + 3 ICMP echo replies, zero copy)", r.stdout[-500:] + r.stderr[-300:])

    # generic-os の QEMU BSP: 守りページ・PAN/LDTR と例外表・PAC/BTI・W^X・装置の高位の写し・アリーナの伸長・SGI の TLB 撃ち落とし
    # the generic-os QEMU BSP: guard pages, PAN/LDTR and the exception table, PAC/BTI, W^X, the device mirror, arena growth,
    # SGI TLB shootdown
    out = os.path.join(tmp, "qemu-genos")
    r = run([FASCON, "qemu", os.path.join(ROOT, "examples", "genos_qemu.fsc"), "--out-dir", out, "--timeout", "120"], stdin=subprocess.DEVNULL, timeout=240)
    o = r.stdout
    ok = r.returncode == 0 and "FASCON-GENOS-QEMU-OK" in o and "PAN on, PAC on, BTI on" in o and "self-test: exception-table fixup on the null guard ok, PAN: EL1 load of a user page blocked, LDTRB/STTRB ok" in o and "shootdown IPIs 1 (served 1)" in o and " 0 shared pages" in o
    report(ok, "examples/genos_qemu.fsc (QEMU generic-os: guard, PAN/LDTR, PAC/BTI, W^X, MMIO mirror, arena grow, SGI shootdown)", o[-900:] + r.stderr[-300:])
    out = os.path.join(tmp, "qemu-genos-guard")
    r = run([FASCON, "qemu", os.path.join(ROOT, "tests", "qemu", "genos_guard.fsc"), "--out-dir", out, "--timeout", "90"], stdin=subprocess.DEVNULL, timeout=180)
    report(r.returncode == 70 and "GUARD: kernel stack overflow into a guard page" in r.stdout and "UNREACHABLE" not in r.stdout, "tests/qemu/genos_guard.fsc (generic-os guard page: stack overflow caught, exit 70)", r.stdout[-500:])
    # fascon-gc を QEMU で: 単一コア（古典）と generic-os の共有 gc_arena（4 コア） / fascon-gc on QEMU: single core and shared (4 cores)
    for f in ("gc_basic.fsc", "gc_own_promote.fsc", "gc_shared.fsc", "gc_temp_roots.fsc", "gc_chan_roots.fsc", "gc_local_own_roots.fsc", "gc_shared_own.fsc"):
        out = os.path.join(tmp, "qemu-" + f)
        r = run([FASCON, "qemu", os.path.join(ROOT, "tests", "ok", f), "--out-dir", out, "--timeout", "120"], stdin=subprocess.DEVNULL, timeout=240)
        exp = open(os.path.join(ROOT, "tests", "ok", f[:-4] + ".expected"), encoding="utf-8").read().split()
        got = [l for l in r.stdout.split() if not l.startswith("[")]
        report(r.returncode == 0 and all(x in r.stdout for x in exp) and "exit code 0" in r.stdout, "tests/ok/" + f + " (QEMU)", r.stdout[-500:])
    # パニック時の全コア停止: CPU0 がパニックすると SGI 2 がロックにもコンソールにも触れずに回り続ける 3 つのコアを止める
    # the all-core stop on panic: when CPU0 panics, SGI 2 stops three cores that spin without touching locks or the console
    out = os.path.join(tmp, "qemu-panic-broadcast")
    r = run([FASCON, "qemu", os.path.join(ROOT, "tests", "qemu", "smp_panic_broadcast.fsc"), "--out-dir", out, "--timeout", "90"], stdin=subprocess.DEVNULL, timeout=180)
    report(r.returncode == 70 and "FASCON PANIC 0x000e" in r.stdout and "panic broadcast (SGI 2): 3 other core(s) stopped" in r.stdout and "UNREACHABLE" not in r.stdout, "tests/qemu/smp_panic_broadcast.fsc (QEMU -smp 4: panic stops every other core by SGI, exit 70)", r.stdout[-500:])
    # コアが出力の最中（コンソールのロックを持っている）でもパニックの報告が出て、全コアが止まったと数えられること
    # the panic report appears and every core is counted as stopped even while cores are printing (holding the console lock)
    out = os.path.join(tmp, "qemu-panic-console")
    r = run([FASCON, "qemu", os.path.join(ROOT, "tests", "qemu", "smp_panic_console.fsc"), "--out-dir", out, "--timeout", "90"], stdin=subprocess.DEVNULL, timeout=180)
    o = r.stdout.replace("\r", "")
    report(r.returncode == 70 and "FASCON PANIC 0x000e" in o and "panic broadcast (SGI 2): 3 other core(s) stopped" in o and "UNREACHABLE" not in o, "tests/qemu/smp_panic_console.fsc (QEMU -smp 4: panic while cores print, 3 cores stopped, exit 70)", o[-500:])

if QEMU and shutil.which("qemu-system-x86_64"):
    # x86_64 の仮想 BIOS（QEMU pc）でのカーネル: AArch64 と同じ抽象プラットフォーム API を、LAPIC タイマー（irq 27）・HPET・
    # INIT-SIPI-SIPI・FS の土台の TLS・CR3 ごとのアドレス空間・Ring 3・int $0x80・SMAP の写し・IPI の撃ち落とし・NMI で実装した
    # BSP で、同じソースが同じ出力になること（timer_freq の値だけは機械の時計: QEMU virt 62.5 MHz / QEMU pc の HPET 100 MHz）
    # kernels on the x86_64 virtual BIOS (QEMU pc): the BSP implements the same abstract platform API as AArch64 with the
    # LAPIC timer (irq 27), the HPET, INIT-SIPI-SIPI, TLS through the FS base, per-CR3 address spaces, ring 3, int $0x80,
    # SMAP copies, IPI shootdowns and NMIs; the same source prints the same output (only timer_freq is the clock of the
    # machine: 62.5 MHz on QEMU virt, the 100 MHz HPET on QEMU pc)
    def x86_body(text):
        return "".join(l + "\n" for l in text.replace("\r", "").splitlines() if not l.startswith("[bsp]") and not l.startswith("[fascon]") and not l.startswith("generated:"))
    for f in ("kernel_preempt.fsc", "critical.fsc", "smp_basic.fsc", "smp_irq_tasks.fsc", "smp_dma_ring.fsc", "gc_basic.fsc", "gc_own_promote.fsc", "gc_temp_roots.fsc", "gc_chan_roots.fsc", "gc_shared.fsc", "gc_local_own_roots.fsc", "gc_shared_own.fsc"):
        out = os.path.join(tmp, "qemu-x86-" + f)
        r = run([FASCON, "qemu", os.path.join(ROOT, "tests", "ok", f), "--target", "x86_64", "--out-dir", out, "--timeout", "150"], stdin=subprocess.DEVNULL, timeout=300)
        expect = open(os.path.join(ROOT, "tests", "ok", f[:-4] + ".expected"), encoding="utf-8").read().replace("62500000\n", "100000000\n")
        ok = r.returncode == 0 and x86_body(r.stdout) == expect and "Fascon virtual BIOS" in r.stdout
        report(ok, "tests/ok/%s (QEMU x86_64, Fascon virtual BIOS%s)" % (f, ", -smp 4" if "4 cores online" in r.stdout else ""), r.stdout[-600:] + r.stderr[-300:])
    # Ring 3 のユーザープロセス: ユーザープログラムを --target x86_64 で作り直し（ABI は AArch64 の版と同じカーネルの宣言）、
    # 4 コア × Ring 3 × 4 KiB の要求時ページング × TOCTOU なしのシステムコール × 敵対的なプログラム
    # ring 3 user processes: rebuild the user program with --target x86_64 (its ABI is the same kernel declarations as the
    # AArch64 version), then 4 cores × ring 3 × 4 KiB demand paging × TOCTOU-free system calls × a hostile program
    out = os.path.join(tmp, "qemu-x86-user")
    os.makedirs(out, exist_ok=True)
    for f in ("tests/ok/smp_preempt_user.fsc", "tests/ok/smp_preempt_user_init.fsc", "tests/qemu/smp_preempt_user_x86.fsc"):
        shutil.copy(os.path.join(ROOT, f), out)
    img = os.path.join(out, "smp_preempt_user_x86_init.bin")
    r = run([FASCON, "build", os.path.join(out, "smp_preempt_user_init.fsc"), "--target", "x86_64", "--out", img, "--out-dir", os.path.join(out, "u")], stdin=subprocess.DEVNULL, timeout=180)
    ok = r.returncode == 0 and os.path.isfile(img)
    if ok:
        ok = open(img, "rb").read() == open(os.path.join(ROOT, "tests", "qemu", "smp_preempt_user_x86_init.bin"), "rb").read()
    report(ok, "ok/smp_preempt_user_init.fsc --target x86_64 (ring 3 image, identical to tests/qemu/smp_preempt_user_x86_init.bin)", r.stdout[-300:] + r.stderr[-300:])
    if ok:
        r = run([FASCON, "qemu", os.path.join(out, "smp_preempt_user_x86.fsc"), "--target", "x86_64", "--out-dir", os.path.join(out, "k"), "--timeout", "200"], stdin=subprocess.DEVNULL, timeout=300)
        expect = open(os.path.join(ROOT, "tests", "qemu", "smp_preempt_user_x86.expected"), encoding="utf-8").read()
        ok = r.returncode == 0 and x86_body(r.stdout) == expect and "IPI 0xf0 answered by 3 of 3" in r.stdout
        report(ok, "tests/qemu/smp_preempt_user_x86.fsc (QEMU pc -smp 4: ring 3 processes, demand paging, per-task CR3, TOCTOU-free syscalls, IPI shootdown)", r.stdout[-900:] + r.stderr[-300:])
    # パニック時の全コア停止（NMI）: 回り続けるコアと、出力の最中のコア / the all-core stop on panic (NMI): spinning cores
    # and cores in the middle of printing
    for f in ("smp_panic_broadcast.fsc", "smp_panic_console.fsc"):
        out = os.path.join(tmp, "qemu-x86-" + f)
        r = run([FASCON, "qemu", os.path.join(ROOT, "tests", "qemu", f), "--target", "x86_64", "--out-dir", out, "--timeout", "90"], stdin=subprocess.DEVNULL, timeout=180)
        o = r.stdout.replace("\r", "")
        report(r.returncode == 70 and "FASCON PANIC 0x000e" in o and "panic broadcast (NMI IPI): 3 other core(s) stopped" in o and "UNREACHABLE" not in o, "tests/qemu/%s (QEMU pc -smp 4: panic stops every other core by NMI, exit 70)" % f, o[-500:])
    # ボードのパッケージ（board.toml + 汎用 BSP）: Raspberry Pi 3（スピンテーブル・メールボックス・非一貫 DMA）と QEMU virt（PSCI）
    # board packages (board.toml + the generic BSP): Raspberry Pi 3 (spin table, mailbox, non-coherent DMA) and QEMU virt (PSCI)
    for board, f, marks in (
        ("raspberrypi3b", "rpi3_hello.fsc", ["SMP: 4 core(s) online (spin-table)", "board revision 0xa02082", "squares from 3 cores 50", "FASCON-RPI3-OK"]),
        ("raspberrypi3b", "hal_hello.fsc", ["SMP: 4 core(s) online (spin-table)", "2997000", "FASCON-BOARD-OK"]),
        ("qemu-virt-generic", "hal_hello.fsc", ["SMP: 4 core(s) online (psci-hvc)", "2997000", "FASCON-BOARD-OK"]),
    ):
        out = os.path.join(tmp, "board-" + board + "-" + f)
        r = run([FASCON, "qemu", "--board", board, os.path.join(ROOT, "tests", "board", f), "--out-dir", out], stdin=subprocess.DEVNULL, timeout=180, cwd=ROOT)
        report(r.returncode == 0 and all(m in r.stdout for m in marks), "tests/board/%s (--board %s on QEMU)" % (f, board), r.stdout[-600:] + r.stderr[-300:])
    # RISC-V64 のボードのパッケージ（M モード・4 ハート・ns16550）を QEMU virt で起動（qemu-system-riscv64 があるとき）
    # the RISC-V64 board package (M-mode, 4 harts, ns16550) booted on QEMU virt (when qemu-system-riscv64 is installed)
    if shutil.which("qemu-system-riscv64"):
        out = os.path.join(tmp, "board-riscv64")
        r = run([FASCON, "qemu", "--board", "qemu-virt-riscv64", os.path.join(ROOT, "tests", "board", "hal_hello.fsc"), "--out-dir", out, "--timeout", "90"], stdin=subprocess.DEVNULL, timeout=180, cwd=ROOT)
        report(r.returncode == 0 and "4 hart(s) online" in r.stdout and "2997000" in r.stdout and "FASCON-BOARD-OK" in r.stdout, "tests/board/hal_hello.fsc (--board qemu-virt-riscv64 on QEMU: RISC-V M-mode, 4 harts)", r.stdout[-600:] + r.stderr[-300:])

    # SMP 拡張: 4 コア（-smp 4）。コアごとのアリーナ・チャネルでの own の受け渡し・共有アリーナ・別のコアでの解放
    # the SMP extension: four cores (-smp 4): per-core arenas, own values through channels, shared arenas, cross-core frees
    out = os.path.join(tmp, "qemu-smp")
    r = run([FASCON, "qemu", os.path.join(ROOT, "examples", "smp_kernel.fsc"), "--out-dir", out, "--timeout", "120"], stdin=subprocess.DEVNULL, timeout=240)
    o = r.stdout
    ok = r.returncode == 0 and "FASCON-SMP-OK" in o and "SMP: 4 cores online (PSCI CPU_ON via HVC)" in o and "-smp 4" in (o + r.stderr) and "jobs 48, workers done 3, checksum 42336 (expected 42336)" in o and "own blocks still live: 0" in o
    report(ok, "examples/smp_kernel.fsc (QEMU AArch64 -smp 4: per-core arenas, chan, shared arena, remote frees)", o[-700:] + r.stderr[-300:])

    # カーネル拡張 Step 4: ユーザー空間。ユーザープログラム（EL0）の像を作り直し、カーネルを起動する: 4 つの EL0 タスク
    # （タイマーで切り替わる）、境界の検証（-14）、ユーザーのパニック（exit 70）、不正な命令（trap）、カーネルは止まらない
    # kernel extensions step 4: user space. Rebuild the user program (EL0) image and boot the kernel: 4 EL0 tasks
    # preempted by the timer, boundary checks (-14), user panics (exit 70), a faulting instruction (trap); the kernel
    # keeps running
    out = os.path.join(tmp, "qemu-user")
    os.makedirs(out, exist_ok=True)
    for n in ("kernel_user.fsc", "user_init.fsc"):
        shutil.copy(os.path.join(ROOT, "examples", n), out)
    r = run([FASCON, "build", os.path.join(out, "user_init.fsc"), "--out", os.path.join(out, "user_init.bin"), "--out-dir", os.path.join(out, "u")], stdin=subprocess.DEVNULL, timeout=180)
    ok = r.returncode == 0 and os.path.isfile(os.path.join(out, "user_init.bin"))
    report(ok, "examples/user_init.fsc (user program image for EL0, ABI from kernel_user.fsc)", r.stdout[-300:] + r.stderr[-300:])
    r = run([FASCON, "qemu", os.path.join(out, "kernel_user.fsc"), "--out-dir", os.path.join(out, "k"), "--timeout", "120"], stdin=subprocess.DEVNULL, timeout=240)
    o = r.stdout
    ok = r.returncode == 0 and "FASCON-USER-OK" in o and o.count("hello from EL0, list sum = 1275") == 3 and "pid 1 exit 0" in o and "pid 2 exit 70" in o and "pid 3 exit 70" in o and "pid 4 fault: class 0x0" in o and "pc 0x38" in o
    ok = ok and "USER PANIC 0x0000000b" in o and "USER PANIC 0x0000000d" in o and not re.search(r"preempted 0 times", o)
    report(ok, "examples/kernel_user.fsc (QEMU AArch64, EL0: 4 user tasks, syscalls, boundary checks, trap, preemption)", o[-700:] + r.stderr[-300:])
    r = run([FASCON, "qemu", os.path.join(ROOT, "docs", "kernel-design", "step4_userspace.fsc"), "--out-dir", os.path.join(out, "s"), "--timeout", "60"], stdin=subprocess.DEVNULL, timeout=180)
    report(r.returncode == 0 and "hello from EL0" in r.stdout, "docs/kernel-design/step4_userspace.fsc (QEMU AArch64, EL0)", r.stdout[-400:] + r.stderr[-300:])

# マルチアーキテクチャ: 生成 C は特定の CPU の命令を含まず、fascon_plat_* の抽象 API だけを呼ぶ。移植用の参照プラットフォーム層
# （bsp/portable-stub/）と一緒に ARMv7・MIPS64・RISC-V64・AArch64・x86_64 向けに -Werror で翻訳し、ld.lld でリンクする
# multi-architecture: generated C holds no CPU-specific instruction and calls only the abstract fascon_plat_* API; compile it
# with -Werror for ARMv7, MIPS64, RISC-V64, AArch64 and x86_64 together with the reference platform layer
# (bsp/portable-stub/) and link it with ld.lld
if shutil.which("clang") and shutil.which("ld.lld"):
    r = run([sys.executable, os.path.join(ROOT, "tools", "cross_check.py"), FASCON], timeout=1200)
    oks = r.stdout.count("\nOK ") + (1 if r.stdout.startswith("OK ") else 0)
    report(r.returncode == 0 and "0 failure(s)" in r.stdout, "tools/cross_check.py (generated C on armv7 / mips64 / riscv64 / aarch64 / x86_64 with -Werror, linked: %d builds)" % oks, r.stdout[-1500:])

# 1 つのソースから 2 つの仮想 BIOS: --target ごとの C の形（x86_64 = in / out 命令、AArch64 = コンパイラが写した MMIO）
# one source, two virtual BIOS images: the form of the C per --target (x86_64 = in / out, AArch64 = MMIO picked by the compiler)
src = os.path.join(ROOT, "examples", "bios_dual.fsc")
gens = {}
for t in ("x86_64", "aarch64"):
    out = os.path.join(tmp, "target-" + t)
    r = run([FASCON, "build", src, "--target", t, "--out-dir", out])
    g = os.path.join(out, "bios_dual.fascon.c")
    gens[t] = open(g, encoding="utf-8").read() if r.returncode == 0 and os.path.isfile(g) else ""
ok = "fascon_outb((uint16_t)(0x3f8u + (uint32_t)i * 1u), v); /* out dx, al */" in gens["x86_64"] and "f20_com1_out" not in gens["x86_64"]
ok = ok and "f20_com1_out(0u, (uint32_t)v); break; /* COM1 THR / RBR → PL011 DR */" in gens["aarch64"] and "fascon_outb(" not in gens["aarch64"]
report(ok, "examples/bios_dual.fsc (--target x86_64: out dx, al / --target aarch64: PL011 MMIO)")

if QEMU:
    # Fascon の仮想 BIOS（x86_64: QEMU pc の -bios、AArch64: QEMU virt の -bios）で、同じソースが同じ出力になること
    # the Fascon virtual BIOS (x86_64: QEMU pc -bios, AArch64: QEMU virt -bios): the same source prints the same output
    bios_cases = [
        ("tests/ok/bump.fsc", None),
        ("tests/ok/own_alloc.fsc", None),
        ("tests/ok/infer.fsc", None),
        ("tests/ok/noat_order.fsc", None),
        ("tests/ok/frame.fsc", None),
        ("tests/ok/reframe_rust.fsc", None),
        ("examples/vruntime_scan.fsc", "1497000\n1494003\n"),
        ("examples/bios_dual.fsc", "FASCON-BIOS-OK"),
        ("examples/rust_style.rs", "FASCON-RUST-OK"),
        ("examples/kernel_core.fsc", "FASCON-KERNEL-OK"),
    ]
    out_all = os.path.join(tmp, "bios-both")
    r = run([FASCON, "bios", os.path.join(ROOT, "examples", "bios_dual.fsc"), "--out-dir", out_all])
    ok = r.returncode == 0 and all(os.path.isfile(os.path.join(out_all, "bios_dual.%s.bios.bin" % t)) for t in ("x86_64", "aarch64"))
    if ok:
        size = os.path.getsize(os.path.join(out_all, "bios_dual.x86_64.bios.bin"))
        ok = size % 65536 == 0
    report(ok, "fascon bios examples/bios_dual.fsc (both images from one source)", r.stdout[-400:] + r.stderr[-400:])
    for f, expect in bios_cases:
        path = os.path.join(ROOT, f)
        full = expect is None
        if full:
            expect = open(path.rsplit(".", 1)[0] + ".expected", encoding="utf-8").read()
        for t in ("x86_64", "aarch64"):
            out = os.path.join(tmp, "vbios-%s-%s" % (t, os.path.basename(f)))
            r = run([FASCON, "qemu", path, "--target", t, "--out-dir", out, "--timeout", "90"], stdin=subprocess.DEVNULL, timeout=200)
            text = r.stdout.replace("\r", "")
            body = "".join(l + "\n" for l in text.splitlines() if not l.startswith("[bsp]") and not l.startswith("[fascon]") and not l.startswith("generated:") and not l.startswith("strict form:"))
            ok = r.returncode == 0 and (body == expect if full or "\n" in expect else expect in body)
            ok = ok and ("Fascon virtual BIOS" in text)
            report(ok, "%s (QEMU %s, Fascon virtual BIOS)" % (f, t), text[-500:] + r.stderr[-300:])
    # GUI: x86_64 と AArch64 の両方で ramfb の画面を 3 枚保存し、キー入力（port_io の COM1）で進むこと
    # GUI: on both x86_64 and AArch64, three ramfb screens are saved and key input (port_io COM1) advances them
    for t in ("x86_64", "aarch64"):
        out = os.path.join(tmp, "gui-%s" % t)
        r = run([FASCON, "qemu", os.path.join(ROOT, "examples", "gui_dual.fsc"), "--target", t, "--out-dir", out, "--timeout", "120", "--screenshot", os.path.join(out, "shot.png"), "--frames", "3"], stdin=subprocess.DEVNULL, timeout=240)
        ok = r.returncode == 0 and "FASCON-GUI-READY 3" in r.stdout and all(os.path.isfile(os.path.join(out, "shot-%d.png" % i)) for i in (1, 2, 3))
        report(ok, "examples/gui_dual.fsc (QEMU %s, virtual BIOS, ramfb + port_io keys)" % t, r.stdout[-500:] + r.stderr[-300:])

print()
print("%d passed, %d failed" % (passed, failed))
if failed == 0:
    shutil.rmtree(tmp, ignore_errors=True)
sys.exit(1 if failed else 0)
