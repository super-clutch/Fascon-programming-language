Fascon v1.0.0 — Rust-like syntax with no lifetime annotations → readable, optimizable C
(日本語: README.ja.txt)

New in 1.0: arenas with generation-tagged 32-bit Ids (Id<@a> = generation 32-K | index K, K set by the capacity), own T trees freed
in one go, fixed-address MMIO regions with compile-time bounds checks, and a BIOS-less AArch64 boot in QEMU.
Files that use only 0.1 features keep working: fascon hands them to the bundled 0.1.0 engine (fascon-core).
November 2026 update (same version): branch-free table scans (live bitmap + ctz, masked counts / sums / min-max),
#[repr(align(N))], a cache-line table layout, 2 MiB-aligned arenas (1 GiB / 2 MiB blocks on the BSP, madvise on Linux)
and a frame region for own values that never escape. See docs/PERF-1.0.md.
Columns (same version): every `for id in @a` reduction reads a column of just the values it reads (8 / 4 bytes or 1 bit
per task instead of the 32-byte slot), so sums, filtered sums, counts and min / max picks run under one cycle per task;
writes keep the columns in step. --columns off gives the earlier tables; `fascon run` compiles for this CPU (--cpu baseline).
Fix (same version): `match` / `if let` binding a whole struct, array or Option<own T> place (e.g. `match @w[id].p { q => .. }`)
no longer generates broken C; the binding refers to the place. Arrays used as whole values through a reference copy the
whole array. Replacing an own through a binding of its place is error F0507. See CHANGELOG.md in the VS Code extension.
Extensions (version stays 1.0): type inference (`let task = create_process(100);` needs no type), `@` abolished
(`pcb_table[id].id = 100;` is the generation-verified .get_mut(id)), declarations in any order (two-pass analysis),
bump_arena (O(1) bulk release at function exit), port_io (out dx, al on x86_64, remapped to MMIO on AArch64),
own_alloc(TypeName), automatic reframing of plain Rust syntax (#![reframing(rust)] / --reframing=rust), the strict
functional-safety form (--emit-strict-fascon), and the Fascon virtual BIOS for x86_64 and AArch64 (one source, two -bios
images). Existing programs compile to the same C. See docs/SPEC-1.0-EXT.md.

■ Contents
  fascon(.exe)          1.0 compiler / language server / QEMU launcher (a single file)
  fascon-core(.exe)     the 0.1.0 engine for 0.1-only files (keep it next to fascon)
  include/fascon2_rt.h  runtime header used by 1.0 generated C (fascon build also copies it into build/)
  include/fascon_rt.h   runtime header used by 0.1 generated C
  bsp/qemu-virt-aarch64/
                        1.0 boot code for QEMU virt AArch64 (start.S, bsp.c, kernel.ld) and its virtual BIOS (firmware.S, firmware.ld)
  bsp/qemu-x86_64-bios/ the Fascon virtual BIOS for QEMU pc (x86_64) (bios.S, bsp.c, bios.ld): 4 cores, LAPIC timer,
                        ring 3 processes with per-task CR3, IPI shootdown, NMI panic stop
  bsp/user-aarch64/, bsp/user-x86_64/
                        start-up code of user programs (EL0 / ring 3) built with fascon build --user
  bsp/portable-stub/    the reference platform layer for porting (ARMv7, MIPS64, RISC-V64, ...; tools/cross_check.py)
  bsp/host/fascon2_plat_host.c
                        platform layer to run 1.0 generated C on a PC
  bsp/generic-aarch64/  the generic AArch64 BSP for real-hardware images (start.S, bsp.c; fascon build --board)
  boards/               board packages: raspberrypi3b (spin table, VideoCore mailbox), qemu-virt-generic (PSCI),
                        qemu-virt-riscv64 (own start.S / bsp.c) and device-window definitions (*.toml)
  lib/                  the standard library in pure Fascon (sync, collections, net, fs, ipc, loader, pm, hal)
  data/                 the error dictionary, explanation rules and fix rules (JSON; override with FASCON_DATA)
  examples/             kernel_core.fsc (Mission 1), gui_desktop.fsc (Mission 2), gui_dual.fsc (one GUI, both architectures),
                        bios_dual.fsc (one BIOS source, both architectures), rust_style.rs (plain Rust syntax),
                        app_wordfreq.fsc (an application), vruntime_scan.fsc, errors/ (one file per error code),
                        and the 0.1 examples (*.rs)
  runtime/<target>/libmas_runtime.a, bsp-examples/, lsp-config/
                        unchanged from 0.1.0 (for 0.1 files; see docs/USAGE.md)
  docs/                 SPEC-1.0.md (specification), SPEC-1.0-EXT.md (the extensions), USAGE-1.0.md (usage),
                        PERF-1.0.md (the November 2026 update),
                        and the 0.1 guides; Japanese: *.ja.md
                        SPEC-1.0-KERNEL.ja.md and kernel-design/ cover the kernel extensions, steps 1 to 4
                        (Japanese; implemented: this compiler accepts them)
                        SPEC-1.0-SMP.ja.md, SPEC-1.0-UNIVERSAL.ja.md / SPEC-1.0-UNIVERSAL.md is the multi-core (SMP) extension of Fascon 1.0
                        SPEC-1.0-INTEGRATION.md / .ja.md: multi-core kernels with interrupts, tasks and user space, 4 KiB demand
                        paging, panic stop on every core, fascon-gc roots, the multi-core x86_64 virtual BIOS (November 2026)
                        (Japanese; implemented; the packages keep the version name 1.0.0)

■ First steps
  fascon doctor                                       what is installed and what is missing
  fascon qemu examples/kernel_core.fsc                Mission 1: boots with no BIOS, prints FASCON-KERNEL-OK
  fascon qemu examples/kernel_user.fsc --headless     kernel extensions step 4: four EL0 tasks and system calls (FASCON-USER-OK)
  fascon qemu examples/smp_kernel.fsc --headless      the SMP extension: four cores (-smp 4), FASCON-SMP-OK; fascon run uses PC threads
  fascon qemu examples/gui_desktop.fsc                Mission 2: a window with two buttons on QEMU's ramfb
                                                      (type a key in the terminal to click Cancel; close QEMU's window to quit)
  fascon qemu examples/gui_desktop.fsc --screenshot shot.png --frames 3
                                                      no window; saves shot-1.png .. shot-3.png
  fascon run examples/gui_desktop.fsc                 runs on this PC; the screen goes to build/gui_desktop.png
  fascon build examples/kernel_core.fsc               build/kernel_core.fascon.c + build/fascon2_rt.h
  fascon regions examples/kernel_core.fsc             arenas, own, MMIO, and how many checks were removed
  fascon explain examples/errors/f2101_color.fsc      root cause, fix and mathematical explanation
  fascon qemu examples/bios_dual.fsc --target x86_64  boots through the x86_64 Fascon virtual BIOS (COM1 = out dx, al)
  fascon qemu examples/bios_dual.fsc --target aarch64 boots through the AArch64 Fascon virtual BIOS (same source)
  fascon bios examples/bios_dual.fsc                  one source → build/bios_dual.x86_64.bios.bin and .aarch64.bios.bin
  fascon qemu examples/gui_dual.fsc --target x86_64   the same GUI on x86_64 (--target aarch64 for AArch64)
  fascon run examples/app_wordfreq.fsc                an application (word counts; bump_arena, own_alloc, inference)
  fascon run examples/rust_style.rs                   runs plain Rust syntax as it is (automatic reframing)
  fascon build examples/rust_style.rs --emit-strict-fascon
                                                      strict Fascon with every type written → build/rust_style.strict.fsc
  fascon --lang ja ...                                Japanese output (or set FASCON_LANG=ja)

■ Tools you install yourself
  Booting in QEMU needs QEMU and LLVM (clang + ld.lld):
    Windows   winget install --id SoftwareFreedomConservancy.QEMU -e
              winget install --id LLVM.LLVM -e
    macOS     brew install llvm lld qemu
    Linux     sudo apt install clang lld qemu-system-arm qemu-system-x86
  Running on the PC (fascon run) needs a C compiler: gcc from MSYS2 on Windows, xcode-select --install on macOS.
  fascon also looks in C:\Program Files\qemu, C:\Program Files\LLVM\bin, C:\msys64\ucrt64\bin and Homebrew.

■ VS Code
  Install fascon-1.0.0.vsix (Extensions view → "…" → Install from VSIX). It contains these executables for
  Windows, Linux and macOS, so nothing else from this archive is needed for VS Code.

■ Honest limits (details: docs/SPEC-1.0.md, "Guarantees and limits")
  The compile-time checks cover code written in Fascon. They do not cover hardware faults (RAM bit flips),
  the hand-written C and assembly of the BSP and runtime, or an mmio declaration whose address does not match
  the hardware. Stack depth is not limited. An arena is finite (about 2.1-4.3 billion allocations), and a fault on
  an unproven Id stops at the next commit point (before a call, output or return) rather than at the access itself.
  A full bump_arena returns None from alloc, and a stale Id after a rewind gives None or a safe stop.
  The virtual BIOS targets QEMU (pc / virt); real hardware needs boot code written for that machine.

■ macOS
  The executables are not code-signed. If macOS says the developer cannot be verified, run in Terminal:
    xattr -d com.apple.quarantine ./fascon ./fascon-core
■ Linux
  Statically linked (musl), so they run on any distribution. There are two builds: x64 and arm64.
  Run `export PATH=$PWD/linux-x64:$PATH` first (linux-arm64 on ARM PCs).
