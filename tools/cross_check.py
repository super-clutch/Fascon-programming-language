#!/usr/bin/env python3
"""生成 C がアーキテクチャに依存しないことの確かめ: 移植用の参照プラットフォーム層（bsp/portable-stub/）と一緒に、ARMv7・
MIPS64・RISC-V64（と AArch64・x86_64）向けに -Werror で翻訳し、ld.lld でリンクまで通す（実行はしない）
checks that generated C is architecture independent: compile it with -Werror together with the reference platform layer
(bsp/portable-stub/) for ARMv7, MIPS64 and RISC-V64 (plus AArch64 and x86_64) and link it with ld.lld (not executed)

  python3 tools/cross_check.py <fascon> [--arch armv7,mips64,riscv64] [file.fsc ...]
"""
import os
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
STUB = os.path.join(ROOT, "bsp", "portable-stub")
ARCHS = {
    # 名前: (clang のフラグ, ポインタの幅, ELF の機械の名前) / name: (clang flags, pointer width, ELF machine)
    "armv7": (["--target=armv7a-none-eabi", "-mcpu=cortex-a15", "-mfloat-abi=soft", "-mtp=cp15", "-marm"], 32, "ARM"),
    "mips64": (["--target=mips64el-unknown-elf", "-march=mips64r2", "-mabi=64", "-G0"], 64, "MIPS"),
    "riscv64": (["--target=riscv64-unknown-elf", "-march=rv64gc", "-mabi=lp64d", "-mcmodel=medany", "-mno-relax"], 64, "RISC-V"),
    "aarch64": (["--target=aarch64-none-elf", "-mcpu=cortex-a72", "-mstrict-align"], 64, "AArch64"),
    "x86_64": (["--target=x86_64-none-elf", "-march=x86-64", "-mno-red-zone", "-fno-pic"], 64, "X86-64"),
}
COMMON = ["-O2", "-std=c11", "-ffreestanding", "-fno-builtin", "-nostdlib", "-Wall", "-Wextra", "-Werror", "-DFASCON2_FREESTANDING", "-DFASCON2_HEAP_SIZE=64", "-DFASCON2_FRAME_SIZE=64"]
DEFAULT_FILES = [
    "tests/ok/smp_preempt_user.fsc",   # マルチコア + irq + タスク + EL0 + システムコール / SMP + irq + tasks + EL0 + syscalls
    "tests/ok/smp_irq_tasks.fsc",      # マルチコア + irq + タスク + chan / SMP + irq + tasks + chan
    "tests/ok/gc_chan_roots.fsc",      # generic-os + 共有の gc_arena + chan / generic-os + shared gc_arena + chan
    "tests/ok/gc_temp_roots.fsc",      # fascon-gc の一時値の根 / fascon-gc temporary roots
    "tests/ok/gc_local_own_roots.fsc", # fascon-gc のローカルの own の根 / fascon-gc roots behind local owns
    "tests/ok/gc_shared_own.fsc",      # generic-os の共有の gc_arena と own の根 / a generic-os shared gc_arena with own roots
    "examples/smp_kernel.fsc",         # 共有アリーナ・par()・chan / shared arenas, par(), chan
    "tests/ok/smp_dma_ring.fsc",       # マルチコア + dma_arena + dma のリング / SMP + dma_arena + a dma ring
    "examples/kernel_user.fsc",        # EL0 のシステムコール・trap（1 コア） / EL0 system calls and trap (one core)
    "tests/ok/gc_shared.fsc",          # generic-os の共有の gc_arena・rcu_read / a generic-os shared gc_arena, rcu_read
    "examples/vruntime_scan.fsc",      # 列の走査（汎用ベクトル拡張） / column scans (generic vector extensions)
]
# 既存のソースそのものが警告を出すもの（どのアーキテクチャでも同じ。生成 C は変えない約束なので、ここでは -Werror の対象外）:
# tests/ok/critical.fsc・examples/kernel_storage.fsc・examples/kernel_net.fsc（使わない let）、tests/ok/kernel_preempt.fsc
# （使わない引数）、tests/ok/universal_lang.fsc（#[asm] の clobbers = "rdx"
# が AArch64 の命令列にも付く）
# existing sources that warn by themselves (the same on every architecture; their generated C must not change, so they
# are left out of -Werror here): tests/ok/critical.fsc, examples/kernel_storage.fsc and examples/kernel_net.fsc (unused
# lets), tests/ok/kernel_preempt.fsc (an unused parameter),
# tests/ok/universal_lang.fsc (#[asm] clobbers = "rdx" also lands on the AArch64 template)


def run(cmd, **kw):
    return subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, **kw)


def profile_defs(c_text):
    head = c_text.split("\n")[:40]
    d = []
    if any(l.startswith("#define FASCON2_SMP 1") for l in head):
        d += ["-DFASCON2_SMP=1", "-ftls-model=local-exec"]
    for k in ("FASCON2_RTOS", "FASCON2_GENERIC_OS", "FASCON2_ID64", "FASCON2_UNIVERSAL", "FASCON2_BOARD", "FASCON2_GC"):
        if any(l.startswith("#define " + k) for l in head):
            d.append("-D%s=1" % k)
    return d


def main():
    args = [a for a in sys.argv[1:]]
    if not args:
        print(__doc__)
        return 2
    fascon = os.path.abspath(args.pop(0))
    archs = list(ARCHS)
    if "--arch" in args:
        i = args.index("--arch")
        archs = args[i + 1].split(",")
        del args[i : i + 2]
    files = args or DEFAULT_FILES
    clang = shutil.which("clang")
    lld = shutil.which("ld.lld")
    if not clang or not lld:
        print("SKIP: clang and ld.lld are needed")
        return 0
    bad = 0
    tmp = tempfile.mkdtemp(prefix="fascon-cross-")
    for arch in archs:
        flags, bits, machine = ARCHS[arch]
        for f in files:
            src = os.path.join(ROOT, f)
            stem = os.path.basename(f).rsplit(".", 1)[0]
            d = os.path.join(tmp, arch + "-" + stem)
            os.makedirs(d, exist_ok=True)
            r = run([fascon, "build", src, "--out-dir", d, "--lang", "en", "--ptr-bits", str(bits)], cwd=ROOT)
            c = os.path.join(d, stem + ".fascon.c")
            if r.returncode != 0 or not os.path.isfile(c):
                print("FAIL %-8s %-34s fascon build: %s" % (arch, f, (r.stderr or r.stdout)[-300:]))
                bad += 1
                continue
            defs = profile_defs(open(c, encoding="utf-8").read())
            objs = []
            srcs = [c, os.path.join(STUB, "fascon2_plat_stub.c")]
            if arch == "armv7":
                srcs.append(os.path.join(STUB, "stub_divmod_arm.S"))
            ok = True
            for s in srcs:
                o = os.path.join(d, os.path.basename(s) + ".o")
                r = run([clang] + flags + COMMON + defs + ["-I" + d, "-c", s, "-o", o])
                if r.returncode != 0 or r.stderr.strip():
                    print("FAIL %-8s %-34s %s: %s" % (arch, f, os.path.basename(s), r.stderr[-600:]))
                    ok = False
                    break
                objs.append(o)
            if ok:
                elf = os.path.join(d, stem + "." + arch + ".elf")
                r = run([lld, "-T", os.path.join(STUB, "link.ld"), "-static", "--no-undefined"] + objs + ["-o", elf])
                if r.returncode != 0 or r.stderr.strip():
                    print("FAIL %-8s %-34s link: %s" % (arch, f, r.stderr[-600:]))
                    ok = False
                else:
                    rr = run(["llvm-readelf", "-h", elf]) if shutil.which("llvm-readelf") else run(["readelf", "-h", elf])
                    m = [l.split(":", 1)[1].strip() for l in rr.stdout.splitlines() if "Machine:" in l]
                    if not m or machine.lower() not in m[0].lower().replace("advanced micro devices x86-64", "x86-64"):
                        print("FAIL %-8s %-34s unexpected ELF machine %s" % (arch, f, m))
                        ok = False
            if ok:
                print("OK   %-8s %-34s compiled with -Werror and linked (%d-bit pointers%s)" % (arch, f, bits, ", " + " ".join(x[2:] for x in defs if x.startswith("-DFASCON2")) if defs else ""))
            else:
                bad += 1
    shutil.rmtree(tmp, ignore_errors=True) if bad == 0 else print("kept:", tmp)
    print("%d failure(s)" % bad)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
