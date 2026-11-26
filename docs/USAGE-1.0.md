# Using Fascon 1.0

日本語: [USAGE-1.0.ja.md](USAGE-1.0.ja.md) · Language specification: [SPEC-1.0.md](SPEC-1.0.md) · Extensions: [SPEC-1.0-EXT.md](SPEC-1.0-EXT.md) · 0.1 usage: `docs/USAGE.md` of the platform archives (not in the source tree)

## 1. What is included

| File | Purpose |
| --- | --- |
| `fascon` (`fascon.exe` on Windows) | the 1.0 compiler, language server and QEMU launcher; a single file with nothing else to install |
| `fascon-core` (`fascon-core.exe`) | the 0.1.0 engine; placed next to `fascon`, files that use only 0.1 syntax are handed to it automatically |
| `include/fascon2_rt.h` | the runtime used by 1.0 generated C (a single header; `fascon build` also writes it to the output folder) |
| `bsp/qemu-virt-aarch64/` | boot code for QEMU virt (AArch64) (`start.S`, `bsp.c`, `kernel.ld`; the virtual BIOS `firmware.S`, `firmware.ld`) |
| `bsp/qemu-x86_64-bios/` | the virtual BIOS for QEMU pc (x86_64) (`bios.S`: reset vector → long mode, `bsp.c`, `bios.ld`) |
| `bsp/host/fascon2_plat_host.c` | the platform layer that runs generated C on a PC |
| `examples/` | 1.0 examples (`kernel_core.fsc`, `gui_desktop.fsc`, `vruntime_scan.fsc`, `gui_dual.fsc`, `bios_dual.fsc`, `rust_style.rs`, `app_wordfreq.fsc`), error examples (`errors/`), 0.1 examples (`*.rs`) |

## 2. Setup (once)

`fascon doctor` lists what is missing and how to install it.

| For | Windows | macOS | Linux (Debian / Ubuntu) |
| --- | --- | --- | --- |
| Booting in QEMU (`fascon qemu`, `fascon bios`) | `winget install --id LLVM.LLVM -e` and `winget install --id SoftwareFreedomConservancy.QEMU -e` | `brew install llvm lld qemu` | `sudo apt install clang lld qemu-system-arm qemu-system-x86` |
| Running on the PC (`fascon run`) | gcc from MSYS2 | `xcode-select --install` | `sudo apt install gcc` |

Besides `PATH`, `fascon` looks in the usual install folders (`C:\Program Files\qemu`, `C:\Program Files\LLVM\bin`, `C:\msys64\ucrt64\bin`, Homebrew).

## 3. In VS Code

1. Install `fascon-1.0.0.vsix` from the Extensions view (Ctrl+Shift+X) → "…" → "Install from VSIX…".
2. Type "Fascon:" in the Command Palette (Ctrl+Shift+P) to see the commands.

| Command | What it does |
| --- | --- |
| Fascon: Open a 1.0 Example (kernel / GUI / dual-architecture / BIOS / Rust style / app / callbacks) | saves and opens an example: kernel, GUI, dual-architecture GUI, dual-architecture BIOS, Rust style, application, vruntime scan |
| Fascon: Boot in QEMU (target from settings; portable = BIOS-less AArch64) | Ctrl+Alt+Q; `fascon.target` portable = BIOS-less AArch64, x86_64 / aarch64 = the Fascon virtual BIOS. The screen is in the QEMU window, key input goes to the VS Code terminal |
| Fascon: Boot in QEMU (x86_64, Fascon virtual BIOS) | Ctrl+Alt+X |
| Fascon: Boot in QEMU (AArch64, Fascon virtual BIOS) | boots from flash address 0 |
| Fascon: Build BIOS Images for x86_64 and AArch64 (one source) | `build/<name>.x86_64.bios.bin` and `build/<name>.aarch64.bios.bin` |
| Fascon: Show the Strict Fascon Form (functional-safety review) | writes `build/<name>.strict.fsc` and opens it beside the source |
| Fascon: Toggle Automatic Reframing (plain Rust syntax) for This Workspace | accept plain Rust syntax in this workspace (`fascon.reframing`) |
| Fascon: Boot in QEMU and Save Screenshots (headless, target from settings) | boots with no window and saves a PNG at every `FASCON-GUI-READY` line, then opens them |
| Fascon: Run on This PC (screen saved as PNG) | builds the generated C for the PC, runs it and saves the screen to `build/<name>.png` |
| Fascon: Generate C Code (build/\<name>.fascon.c) | writes `build/<name>.fascon.c` and `fascon2_rt.h` (the source is never overwritten) |
| Fascon: Explain Error (timeline, fixed code, mathematical explanation) | Ctrl+Alt+E; root cause, how to fix it, mathematical explanation |
| Fascon: Show Region Assignment | arenas, own, MMIO, and how many checks were removed statically |
| Fascon: Search the Dictionary (keywords, error codes, second-class references) | Ctrl+Alt+D; every 0.1 and 1.0 entry |
| Fascon: Show Version and Tool Check | the output of `fascon version` and `fascon doctor` |

Settings (`fascon.*`):

| Setting | Default | Meaning |
| --- | --- | --- |
| `fascon.language` | auto | display language (auto / ja / en) |
| `fascon.outDir` | build | output folder for C, ELF and images (relative to the workspace) |
| `fascon.extraPath` | [] | folders added to PATH when running QEMU and C compilers |
| `fascon.screenshotFrames` | 3 | screens saved by "Fascon: Boot in QEMU and Save Screenshots (headless, target from settings)" |
| `fascon.qemuTimeout` | 90 | time limit (seconds) for headless QEMU runs |
| `fascon.ptrBits` | 64 | pointer width of the target |
| `fascon.serverPath` | (empty) | use a different `fascon` executable |
| `fascon.columns` | true | give each `for id in @a` reduction a column of just the values it reads (false = the 1.0.0 tables; restart the language server to apply it to "Fascon: Generate C Code (build/\<name>.fascon.c)") |
| `fascon.cpu` | native | "Fascon: Run on This PC (screen saved as PNG)" compiles the C for this PC's CPU (`native`) or for the architecture's baseline (`baseline`) |
| `fascon.target` | portable | the form of I/O ports (port_io) and what QEMU boots: `portable` (both under `#if`; QEMU boots BIOS-less AArch64) / `x86_64` (`in` / `out`; QEMU pc + virtual BIOS) / `aarch64` (remapped to MMIO; QEMU virt + virtual BIOS) |
| `fascon.reframing` | auto | automatic reframing: `auto` (follow `#![reframing(rust)]` in the file) / `rust` (always on) / `off` |
| `fascon.emitStrictOnBuild` | false | "Fascon: Generate C Code (build/\<name>.fascon.c)" also writes `<name>.strict.fsc` (the strict Fascon form) |

## 4. On the command line

```text
fascon build <file> [--out-dir D]      generate C (D/<name>.fascon.c and D/fascon2_rt.h)
fascon check <file> [--json]           check only (exit code 1 when there are errors)
fascon explain <file> [--line N] [--code F2101]
fascon fix <file> [--in-place]         verified quick fixes (default output: <name>.fixed.fsc)
fascon regions <file>                  the regions chosen, each arena's Id split (K) and lifetime, and how the checks were handled
fascon run <file> [--screenshot out.png] [--cpu native|baseline]
fascon qemu <file> [--target x86_64|aarch64] [--headless] [--screenshot out.png] [--frames N] [--timeout seconds] [--build-only] [--cc gcc|clang]
fascon bios <file> [--target x86_64|aarch64]   x86_64 and AArch64 virtual-BIOS images (*.bios.bin) from one source
fascon example [kernel|gui|dual|bios|rust|app] [--out <file>]
fascon bsp [--out <dir>]               write the boot code (AArch64 and the x86_64 virtual BIOS)
fascon runtime-header [--out <dir>]
fascon dict [term] [--json|--markdown]   reference dictionary (exact name / alias matches first; for a term starting with `--`, `fascon dict -- --target`)
fascon doctor
fascon lsp                             language server (stdio)
```

Common options: `--lang ja|en`, `--engine auto|1|0.1`, `--out-dir <dir>`, `--ptr-bits 32|64`, `--columns on|mirrored|off`, `--nontemporal auto|on|off`, `--fast-math`, `--gpu=on|off`, `--target x86_64|aarch64|portable|rtos|generic-os|hpc`, `--topology cores=N,smt=K,numa=M`, `--reframing rust|off`, `--emit-strict-fascon` (`--name=value` works too).

- `--target x86_64|aarch64|portable` (default portable; environment `FASCON_TARGET`): the C form of `port_io` reads and writes. `x86_64` uses the `in` / `out` instructions (`com1 = c;` → `out dx, al`), `aarch64` lets the compiler remap ports to MMIO (COM1 → the PL011), `portable` selects both and the PC run with `#if`. For `fascon qemu` it also selects what boots (x86_64 / aarch64 use the Fascon virtual BIOS).
- `--reframing rust|off` (default: follow `#![reframing(rust)]` at the top of the file; environment `FASCON_REFRAMING`): rewrite ordinary Rust syntax into Fascon before compiling (docs/SPEC-1.0-EXT.md §6).
- `--emit-strict-fascon` (`build` / `check` / `qemu` / `bios`): write the strict Fascon form for functional-safety review to `<out dir>/<name>.strict.fsc`.

- `--columns on|mirrored|off` (default on; the environment variable `FASCON_COLUMNS=off` / `mirrored` changes the default): with `on`, every `for id in @a` reduction (sum, filtered sum, filtered count, min / max pick) reads a column holding just the values it reads instead of the slots. Eligible fields leave the slot and their column becomes the only copy (True SoA: one write); the rest get mirrored columns that every write reaching them keeps in step (write-heavy fields get none). `mirrored` gives mirrored columns everywhere (the same C as 1.0 before True SoA), `off` the 1.0.0 tables and scans. `fascon regions` shows True-SoA (single-write) / Mirrored-SoA (dual-write) per column and why fields were kept off True SoA (docs/PERF-1.0.md, phases 4 and 5).
- `--nontemporal auto|on|off` (default auto; the environment variable `FASCON_NONTEMPORAL`): whether bulk-overwrite loops over True-SoA columns (bodies made only of `@a[id].f = value;`) use non-temporal stores (MOVNTI on x86-64, etc.). `auto`: only for columns of 8 MiB or more, in tables not shared with interrupt handlers or tasks, when the following statements (up to the next loop) do not read the same column. `on`: always (and prefetch in scans of large tables' columns). Both put `F20_SFENCE()` at the loop exit. `off`: never (docs/PERF-1.0.md, 5-4).
- `--fast-math` (environment `FASCON_FAST_MATH=1`): treat the whole program as `#[fast_math]`: float sums and min / max picks may run in parallel lanes over f32 / f64 True-SoA columns (four vector accumulators, IEEE-754 order keys). Without it only loops and functions marked `#[fast_math]` do; everything else keeps the written order, bit for bit (docs/SPEC-1.0-HPC.md §1).
- `--gpu=on|off` (default on; environment `FASCON_GPU=off`): with `off`, `gpu_arena` dispatches run in place on the calling core (the C function registered with `FASCON_GPU_KERNEL(id)`, nothing when none) and complete at once; the generated C calls no `fascon_plat_gpu_*` hook (warning F2707). `fascon run` builds `<name>.gpu.c` next to the source along with the program (docs/SPEC-1.0-HPC.md §3).
- `--target hpc` (or `// fascon-target: hpc`) with `--topology cores=N,smt=K,numa=M[,layout=adjacent]` (or `// fascon-topology: ..`; default cores=128 smt=2 numa=8): the HPC profile: 64-bit Ids, every table on a 2 MiB boundary (MADV_HUGEPAGE), each core pinned to a physical core and its per-core memory bound to its NUMA node at main and spawn_on entries, and the checks F3901 to F3903. `fascon regions` shows the node shape and each core role (docs/SPEC-1.0-HPC.md §4).
- `--cpu native|baseline` (`fascon run`, default native): `native` compiles the generated C with `-march=native` (x86-64) or `-mcpu=native` (AArch64), so column scans use the widest vectors the PC has (AVX2 / AVX-512 / NEON); a compiler that rejects the flag is retried without it. `baseline` uses only the architecture's baseline instructions (SSE2 on x86-64), for a binary that also runs on older CPUs.

### Which engine handles a file

A file that uses any of `arena` / `mmio` / `own` / `type` / `@name` / `Id` / `Str` / `if let` / `[T; N]`, or starts with `#![fascon(1)]`, is handled by the 1.0 engine. Everything else goes to `fascon-core` (0.1.0). `--engine 1` / `--engine 0.1` forces one of them (`--engine 2` from the preview still means 1.0). `run` and `qemu` always use the 1.0 engine.

### BIOS images for x86_64 and AArch64 from one source (`fascon bios`)

```text
fascon bios examples/bios_dual.fsc
  → build/bios_dual.x86_64.bios.bin   (QEMU pc -bios: reset vector → long mode → kernel)
  → build/bios_dual.aarch64.bios.bin  (QEMU virt -bios: flash → RAM 0x4008_0000 → kernel)
fascon qemu examples/bios_dual.fsc --target x86_64     # build and boot
fascon qemu examples/gui_dual.fsc --target aarch64 --screenshot shot.png --frames 3
```

x86_64 boots with `qemu-system-x86_64 -M pc -m 2G -bios <bin> -nic none -no-reboot -device isa-debug-exit,iobase=0xf4,iosize=0x04`
(plus `-vga none -device ramfb` when there is a screen). The program's exit code travels through isa-debug-exit (QEMU returns
`(code << 1) | 1`), and `fascon qemu` turns it back into the original code.

### What `fascon qemu` does

1. Generates `build/<name>.fascon.c` and writes the boot code to `build/bsp-qemu-virt-aarch64/`.
2. Links `build/<name>.elf` with clang + ld.lld (or an aarch64 gcc when clang is missing).
3. Starts `qemu-system-aarch64 -M virt -cpu cortex-a72 -m 256M -nic none -semihosting -kernel <elf>`, adding `-device ramfb` when there is a `#[framebuffer]` region.
4. With `--screenshot`, QEMU runs with no window; every time `FASCON-GUI-READY` appears on the UART, the screen is saved with a QMP screendump and then a key (`n`) is sent.

When the program calls `exit(code)` or returns from `main`, QEMU exits through semihosting with that exit code.

### Switches for the generated C

| Switch | Effect |
| --- | --- |
| `FASCON_NO_HUGEPAGE=1` (environment, `fascon run` on Linux) | do not ask for 2 MiB pages for arenas of 1 MiB or more (`madvise(MADV_HUGEPAGE)` is the default) |
| `-DFASCON2_PICK_BRANCHLESS` (C compiler) | the min / max update of masked reductions becomes a conditional select (CSEL / CMOV) too, so no branch depends on the data; slower on average (docs/PERF-1.0.md) |
| `-DFASCON2_FRAME_SIZE=<bytes>` (C compiler) | size of the frame region for `own` values that never escape, on a PC (default 64 KiB; the QEMU BSP uses its 1 MiB `.frame` section) |
| `-DF20_VECTOR_BYTES=16\|32\|64` (C compiler) | the vector width of column scans (default: the target's own width: 64 with AVX-512VL, 32 with AVX2, 16 otherwise) |
| `-DF20_VEC_I64_CMP=0\|1` (C compiler) | whether keyed min / max picks use vector 64-bit compares (default 1 with SSE4.2 / AVX / AArch64, 0 = four scalar CMOV / CSEL chains) |
| `FASCON_HPC_NO_PIN=1` (environment, `--target hpc` on a PC) | do not pin cores or bind memory to NUMA nodes (both are advisory anyway: a refusal never stops the program) |
| `-DFASCON2_GPU_KERNELS_MAX=<n>` (C compiler) | how many `FASCON_GPU_KERNEL(id)` kernels the registry holds (default 64) |
| `-DFASCON2_BOARD_GPU` / `-DFASCON2_BOARD_HPC` (C compiler, bare metal) | the board package provides `fascon_plat_gpu_*` / the HPC hooks itself (without them, bare-metal builds run dispatches in place and pin nothing) |

## 5. Troubleshooting

| Symptom | What to do |
| --- | --- |
| `qemu-system-aarch64 not found` | install QEMU; if it lives somewhere else, add the folder to `fascon.extraPath` |
| `no C compiler for AArch64 found` | install LLVM (clang and ld.lld) |
| `the 0.1 engine (fascon-core) was not found` | put `fascon-core` next to `fascon`, or point the `FASCON_CORE` environment variable at it |
| macOS: "cannot verify the developer" | `xattr -d com.apple.quarantine fascon fascon-core` |
| `FASCON PANIC 0x000d` | `@a[id]` was used with an Id whose slot was freed (`if let Some(x) = @a.get(id)` handles it as None) |
| `FASCON PANIC 0x000c` | an index that could not be proven was out of range |
| `FASCON PANIC 0x0001` | the own heap (8 MiB by default) ran out |
| `qemu-system-x86_64 not found` | install QEMU (the `qemu-system-x86` package on Linux) |
| `no C compiler for x86_64 found` | install LLVM (clang and ld.lld; one installation serves x86_64 and AArch64) |
| `CPU EXCEPTION 13 (#GP general protection)` (x86_64) | printed by the virtual BIOS; look the RIP up in `build/<name>.x86_64.elf` with `llvm-objdump -d` |
| F0282 | a still-unknown type was used as the base of a field, a method or an index; write one type on that let |
| F2401 / F2402 | a bump_arena slot was freed one by one / its Id was stored in a long-lived table or a static (use an `arena`) |
| `FASCON PANIC 0x0014` | a `gpu_arena` dispatch failed: no kernel registered with that id (`FASCON_GPU_KERNEL(id)` in `<name>.gpu.c`), or the platform refused the slot array |
| `[host] gpu: no kernel registered with id N` | add `FASCON_GPU_KERNEL(N) { .. }` to `<name>.gpu.c` next to the source (or run with `--gpu=off`) |
| F2705 / F2706 | a gpu_arena element holds own / Id / Str / bool / Option / a reference / a function pointer, or a handle or ticket of another gpu_arena was used |
| F3901 / F3902 / F3903 (`--target hpc`) | I/O, dynamic allocation or a table-wide mirror rebuild reachable from a compute core, or a core / NUMA node outside the node shape (`fascon regions` shows the HPC section) |

## 6. Building from source

```text
cargo build --release                       # target/release/fascon (Rust 1.74 or later, no external crates)
python3 tests/run_tests.py target/release/fascon --qemu
```

`tests/run_tests.py` checks the error cases, run results, the column test (`tests/cols/cols.fsc` with `--columns on`, `mirrored` and `off`), the True-SoA tests (`tests/ok/tsoa_*.fsc`, the float and masked-RMW ones included, with every combination of `--columns` and `--nontemporal`), the masked RMW kernels against a scalar model at -O2 and under ASan / UBSan, the float printer (400,000 bit patterns), the gpu_arena emulation under ASan / UBSan and TSan, the `--target hpc` C and report, random programs from `tests/cols/colfuzz.py` (`--tsoa`, `--float` and `--rmw` too) run with each setting (`--fuzz N`, default 8), the examples (runs on the PC and boots on BIOS-less QEMU), the strict-form round trip, the C form per `--target`, and with `--qemu` boots through the x86_64 and AArch64 virtual BIOS (including the GUI screens and key input). `python3 tools/compare_c.py` checks that every existing program (examples, tests, in the default, mirrored and off column modes) still compiles to the same C as recorded in `tests/golden_c.json` (comments and whitespace aside); `python3 tools/compare_c.py <new fascon> <old fascon> <file>...` compares two builds directly. For the benchmarks, see `bench/RESULTS.md`.
