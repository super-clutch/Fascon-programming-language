# Fascon 1.0 Universal OS specification (`--target rtos` / `--target generic-os`, fascon-gc, ecosystem and real hardware)

This is the condensed English version of [SPEC-1.0-UNIVERSAL.ja.md](SPEC-1.0-UNIVERSAL.ja.md), which has the full text, the
diagrams in code and the assembly. Every version name stays **Fascon 1.0 (1.0.0)**.

## 0. Compatibility promises

- Existing syntax and memory-safety rules (F0507, F0515, F0520, F2101, ...) are unchanged.
- The C generated for existing programs is unchanged to the byte: 42 programs × Japanese/English (84 files) were compared
  with the output of the original 1.0.0 executable — **all identical**.
  `python3 tools/compare_c.py <new fascon> <old fascon> <file>...` makes a comparable check (English output, comments and
  whitespace ignored).
- New features only add C to programs that use them. No external crates.
- Contributors extend Fascon without touching the compiler: `lib/` (standard library), `lib/hal/` (HAL), `boards/<name>/`
  (board packages), `data/*.json` (error dictionary, explanation rules, fix rules).

## 1. Profiles

| | classic (default) | `--target rtos` | `--target generic-os` |
|---|---|---|---|
| Ids | 32-bit | 32-bit | **64-bit** `[gen 32 | idx 32]`, `0` = None |
| Arenas | `.bss.fascon_arena` | `.fascon_arena` | **VA-reserved**, 2 MiB mapped on demand |
| Recursion | allowed | **F3001** | allowed (guard pages) |
| Stack | — | `fascon stack` proves the maximum from the call graph | 4 KiB guard pages |
| Allocation | free list | free list | **per-CPU magazines** + RCU-deferred free |

Selection order: `// fascon-target:` in the file → `--target` / `--profile` → `FASCON_PROFILE` → `board.profile` → classic.

## 2. Language additions

`Result<T, E>` and `?`; `catch_fault { .. }` (latched faults → `Result<T, FaultError>` with local cleanup); `user [u8; N]` /
`user Str` with one-time `copy_from_user(buf)?` (F3101 double read); `#[syscall(N)]` / `#[trap]`; `dma own T`
(`dma_alloc` / `submit_dma` / `complete_dma`, F3401–F3403); `bind_mmio(Regs, addr)` (F3404); `atomic { .. }` (F3201);
`rcu_read { .. }` (F3202); the Sleep effect (F3203); `#[secret]` (F3301–F3304); `#[on_drop(f)]` (F3505), `#[no_column]`,
`#[stable_abi]` (F3502; **Id<@a> fields are F3501**); `#[asm(..)]` and `extern "C"` (F3503, F3601); `import a::b;`
(F3701–F3703); `gc_arena` (section 5). Drop order for types with both `#[on_drop]` and `#[secret]`: **1. on_drop →
2. volatile zeroing + barrier → 3. free**.

## 3. rtos

32-bit Ids, arenas in `.fascon_arena`, recursion rejected (F3001, Tarjan over the call graph including function-pointer
candidates), and `fascon stack` proves the worst-case stack from GCC `-fcallgraph-info=su,da`.

## 4. generic-os

64-bit Ids, VA-reserved shared arenas grown 2 MiB at a time under a bit lock, per-CPU magazines, RCU. Process arenas keep a
hidden `cpu_mask`; `tlb_flush` does a local TLBI for the current core and sends **SGIs only to the cores running that
process**. The QEMU BSP (`bsp/qemu-virt-aarch64/genos.c`, used automatically for generic-os C, `-cpu max`):

- null guard (virtual 0–2 MiB unmapped), 4 KiB guard pages for kernel and task stacks; the exception entry checks SP-1 and
  SP-800 with `AT S1E1W` **before** pushing the frame and switches to a per-core emergency stack on overflow;
- W^X: kernel image split into 4 KiB pages at boot (before the MMU is on, so no break-before-make): .text RX + GP (BTI),
  .rodata RO, everything else RW + PXN;
- PAN = 1, user memory copied only with `LDTRB` / `STTRB`, faults go to `__ex_table` fixups (EFAULT);
- PAC (APIA key, EnIA) and BTI (BT1, GP pages), built with `-mbranch-protection=standard` and `-fstack-clash-protection`;
- device mirror at `0x60_0000_0000` for `mmio` / `bind_mmio`; DTB walk registers device windows; PCIe ECAM BARs assigned.

Boot output: `PAN on, PAC on, BTI on`, `0 shared pages`, `exception-table fixup on the null guard ok, PAN: EL1 load of a user
page blocked, LDTRB/STTRB ok`. Existing kernels (`kernel_user`, `step4_userspace`, `smp_kernel`) boot unchanged with
`FASCON_PROFILE=generic-os`.

## 5. fascon-gc (`gc_arena`)

```fsc
struct Node { v: u64, next: Option<Id<nodes>> }
gc_arena nodes: Node[1024];          // also `gc_arena @nodes: ..` and `shared gc_arena g: T[N];`
let freed = gc_collect(nodes);       // u32: slots reclaimed
let kept = promote_to_gc(nodes, bump_id);
```

- Elements are named by the coloured `Id<nodes>` (F2101 / F0515 / F0520 unchanged); Id width follows the profile.
- **Type descriptors** in .rodata (`f20_type_desc_t`: `F20_GC_ID`, `F20_GC_OWN`, `F20_GC_ARR`, `F20_GC_OPT`) give a precise
  object graph — no conservative guessing.
- **Roots**: statics, live slots of ordinary / task / bump arenas, a **shadow stack** (frames only in functions that touch gc
  arenas; roots registered after their declaration and removed by `__attribute__((cleanup))` at scope end), suspended tasks
  (the BSP saves and restores each task's shadow-stack top).
- **Roots behind local owns (2026-11)**: locals, by-value parameters, temporaries and return slots whose type reaches gc Ids
  through `own T` / `Option<own T>` (inside structs and arrays too) are registered with descriptors that follow own
  (`F20_GC_OWN` rows; a NULL own is skipped). The C variable is always either a valid value or 0 / NULL: moves clear the
  source, drops go through `fsc_own_drop_at_<type>(&owner)` (children dropped while `#[on_drop]` still reaches them →
  owner = NULL → storage freed), `.take()` leaves None, frame-region owns are cleared before the frame is popped, and a
  dropped struct / Option / array variable is zeroed. Temporaries made in the middle of an expression are rooted until all
  operands that may collect have run, then leave the roots right before the call or literal takes them, so no own is ever
  reached from two roots (a second generation-refresh rewrite of the same Id would turn it into None). Drop order:
  `#[on_drop]` runs while the value is still a root (a `gc_collect` inside it keeps its Ids), then the children, the owner
  (NULL), the storage, and the cleanup that removes the root at scope end. Runtime (inside `FASCON2_GC` only): a nested
  `gc_collect` (an `#[on_drop]` inside a sweep, another task on the same core) returns 0; with `FASCON2_PREEMPT` interrupts
  are masked while marking; each chain is walked once; on the QEMU AArch64 / x86_64 BSPs a task switch during a shared
  collection parks before the new task runs; entry safepoints fence before reading the request (`F20_GC_POLL_ENTRY`);
  `-DFASCON2_GC_ALIAS_CHECK` stops when one walk reaches the same own twice (tests only).
- **Mark** with `mark[]` bits; when every Id of the element is a plain field, only the **Id-only projection columns** (made
  by the existing `F20_DEFINE_TABLE_COLS` machinery) are read, not the slot bodies. **Sweep** `live & ~mark` with ctz / blsr
  and hand each dead slot to the arena's own free (own drop, zero fill, generation + 1, column unmirror, free list or magazine;
  shared arenas after an RCU grace period).
- Modes: (1) **effect-driven local collection** — per-core gc arenas are collected on their core alone; shared gc arenas
  park only cores currently inside functions whose effects touch them (safepoints at entry, loop heads, channel / lock
  waits); (2) **generation refresh** for 32-bit Ids (rewrite every live Id to generation 1, stale ones to 0, reuse retired
  slots; compiled out for 64-bit Ids); (3) **evacuation** from bump arenas with `promote_to_gc` and **automatic promotion by
  escape analysis** (a bump Id passed where a gc Id of the same element type is expected).
- Compile-time rules: **F2621** free / remove on a gc arena; **F2622** allocation / collection / promotion inside `#[trap]`,
  `rcu_read`, `atomic`; **F2623** gc_arena with `--gc=off` (`FASCON_GC=off`; the GC runtime is only compiled with
  `FASCON2_GC`); **F2624** `dma own T` in gc elements; **F2625** shared-gc Ids in per-core arenas; **F3501** Ids in
  `#[stable_abi]` structs.
- Error-code note: the request named F2601–F2604, but F2601, F2602, F2603 and F2605 already mean other things in Fascon 1.0;
  keeping existing diagnostics unchanged, the GC codes live at **F2621–F2624 (same last digit) plus F2625**. `#[on_drop]`
  shape errors moved to F3505 so that F3501 means what the request asked.
- Limits: concurrent / incremental marking with a write barrier is designed but not implemented. A task switch caused by an
  interrupt on another core during a shared collection parks only on the BSPs that do it in `bsp_switch` (QEMU virt
  AArch64 and x86_64), and a task preempted while holding a `par()` reader switches without parking; neither was tried on
  real hardware. Resolved in 2026-11: the contents of local `own T` (locals, parameters, temporaries, return slots,
  `Option<own T>`, structs with own fields and arrays of them) are roots (`tests/ok/gc_local_own_roots.fsc`,
  `gc_shared_own.fsc`, `gc_own_refresh.fsc`); Ids held only by temporaries in the middle of an expression are roots (the
  compiler spills them into locals registered with `F20_GC_ROOT`); and shared gc_arena Ids inside values waiting in a
  channel (inside own too) are roots as well (see [SPEC-1.0-INTEGRATION.md](SPEC-1.0-INTEGRATION.md)).

## 6. Ecosystem

- Modules: `import net::tcp;`, per-module parallel checking and caching; search order: this folder, `fsc_modules/`,
  `FASCON_PATH`, the embedded standard library.
- **Package safety labels**: `fascon pkg label` (also `pkg install` and `build` inside a package) scans every source and
  writes `safety = "Pure Safe"` or `"Low-Level / Hardware"` with `safety_reasons` into `fascon.toml`; `fascon.lock` carries a
  label per dependency; `fascon pkg std` shows the labels of the standard library.
- **Standard HAL** (`lib/hal/`): `Console`, `Timer`, `BlockDevice`, `NetDevice` as structs of function pointers. Board packages
  provide `board_hal.fsc` with `fn console() -> console::Console`, so applications switch boards with `import board_hal;`.
- Data-driven `data/dictionary.json`, `data/rules.json`, `data/fixes.json` (overridable through `FASCON_DATA` or `data/` next
  to the executable). After changing the dictionary, `python3 tools/sync_dictionary.py` copies it into the dictionaries of
  the VS Code extension and `python3 tools/check_dictionary.py` checks it (every error code the compiler can emit has an
  entry, both languages are present, the extension says the same); both run in `tests/run_tests.py`.
- Automatic reframing (`#![reframing(rust)]`) is frozen at its current scope as a migration entrance; converted code is
  settled into pure Fascon 1.0 with `--emit-strict-fascon`.

## 7. Real hardware

`board.toml` gains `[boot]` (`load_addr`, `stack_size`, `smp = none | spin-table | psci-hvc | psci-smc | ipi`,
`release_addrs`, `exit = loop | psci | semihosting`, `entry_el`), `[dma]` (`coherent`, `cache_line`, `bus_offset`),
`[qemu]` (`machine`, `args`, `binary`) and `[toolchain]` (`cc`, `ld`, `cflags` for non-AArch64 packages).

- `fascon build --board <name|path> app.fsc` → `app.<board>.elf` and a raw `app.<board>.img` for flash / SD cards (an
  objcopy-equivalent written in Rust). `fascon qemu --board <name>` boots it on `[qemu] machine` (exit via semihosting).
- The generic AArch64 BSP (`bsp/generic-aarch64/`) drops to EL1 from EL3 / EL2 / EL1, sets per-core stacks, an identity MMU
  (RAM Normal WB, the rest Device), TLS, PL011 / ns16550 consoles, the generic timer and the selected SMP release protocol.
- Non-coherent DMA: with `coherent = false`, `submit_dma` cleans to PoC (`dc cvac`) and `complete_dma` cleans + invalidates
  (`dc civac`) per cache line; `bus_offset` makes the IOVA the device-side address (`bus_addr`).
- Board packages: `boards/raspberrypi3b/` (PL011 HAL, VideoCore mailbox in pure Fascon; verified on QEMU raspi3b: 4 cores via
  spin table, board revision 0xa02082, 960 MiB), `boards/qemu-virt-generic/` (PSCI; verified), `boards/qemu-virt-riscv64/`
  (own start.S / bsp.c, IPI wake-up, `[toolchain]`; 4 harts boot on QEMU virt with `FASCON-BOARD-OK`; not verified on physical
  boards).

## 8. Verification

Generated C of existing programs against the original 1.0.0 executable: 84/84 identical (section 0).
`tests/run_tests.py --qemu`: all pass (GC tests on the PC and on QEMU, error tests F2621–F2625 /
F3501 / F3505, HAL, generic-os BSP, guard-page overflow, board packages on raspi3b and virt).

## 9. Not done / not verified

Concurrent marking with a write barrier for shared gc arenas; boot on physical Raspberry Pi boards and RISC-V hardware (QEMU
raspi3b, and since 2026-11 the RISC-V board boots 4 harts on QEMU virt); interrupts / tasks / EL0 in the generic BSP; real page
tables behind the `f20_aspace` process tables of generic-os (EL0 / ring 3 tasks have per-ASID / per-CR3 page tables since
2026-11); the macOS executable is cross-compiled and was not run; the Windows one was run only under Wine.
