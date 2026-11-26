# Fascon 1.0 specification: integrating multi-core, interrupts, user space and fascon-gc on a multi-architecture base

The version name **stays Fascon 1.0 (1.0.0)**. This document is the specification of the 2026-11 integration work and the
results of implementing and verifying it, limits included. Japanese: [SPEC-1.0-INTEGRATION.ja.md](SPEC-1.0-INTEGRATION.ja.md).
Background: [SPEC-1.0-KERNEL.ja.md](SPEC-1.0-KERNEL.ja.md) (interrupts, tasks, user space), [SPEC-1.0-SMP.ja.md](SPEC-1.0-SMP.ja.md)
(multi-core), [SPEC-1.0-UNIVERSAL.md](SPEC-1.0-UNIVERSAL.md) (profiles, fascon-gc, ecosystem).

## 1. What changed

| Item | Before | Now |
| --- | --- | --- |
| `spawn_on` together with `irq`, `task_arena`, `dma_arena`, `syscall`, `trap` | rejected by F2614 | allowed (**F2614 is retired**); the task table, the current task, the run queue and the timer are per core |
| Shape of `user_mem` | multiple of 2 MiB | **multiple of 4 KiB** (F2803); pages are mapped on first touch (demand paging) |
| Address spaces | one table rewritten, TLB flushed on every switch | **one address space per task** (ASID on AArch64, CR3 + PCID on x86_64); switches do not flush the TLB |
| User processes | CPU0 only | **EL0 / ring 3 processes run preemptively on every core** |
| System-call windows | identity-map windows handed to the body | on multi-core the entry **copies each window once into a shadow buffer** (no second read); `&mut` windows are written back on exit |
| Panic | a flag; only cores in waiting loops stop | **a stop interrupt to every core** (SGI 2 on AArch64, NMI on x86_64); stopped cores are counted and reported |
| fascon-gc temporaries | Ids in the middle of an expression are not roots | the compiler spills them into locals registered with `F20_GC_ROOT` |
| Ids inside channels | not roots | `gc_collect` walks the published cells of each ring buffer as roots |
| x86_64 virtual BIOS | one core, no tasks, no interrupts | 4 cores (INIT-SIPI-SIPI), LAPIC timer, HPET, ring 3, IPIs, NMI |
| Other architectures | none | generated C compiled with `-Werror` and linked for ARMv7, MIPS64, RISC-V64, AArch64 and x86_64; the RISC-V64 board boots 4 harts on QEMU |

## 2. How to write it: no new syntax

Programs combine the existing `spawn_on`, `irq`, `task_arena`, `user_mem`, `syscall`, `trap`, `critical`, `shared arena` and
`gc_arena`. No lifetime annotations, generics, raw pointers or `unsafe` were added. This is the skeleton of a kernel in which
each of four cores switches its own EL0 / ring 3 tasks with its own timer (the whole program: `tests/ok/smp_preempt_user.fsc`).

```fascon
task_arena task: Task[4] stack 16384;               // one instance per core (slot i is a different task on each core)
user_mem umem for task: 0x8_0000 at 0x1_0000_0000;  // 512 KiB; any multiple of 4 KiB works
shared arena tally: Tally[1];                        // one for all cores (inside critical only)

irq tick at 27 {                                     // the per-core timer: on to the next task of this core
    timer_arm(ms(4));
    // walk the task table, pick the next live task n and call task.resume(n)
}

syscall write at 1 (fd: u32, buf: &[u8]) -> i64 {    // on multi-core, buf is a shadow buffer (copied once)
    critical { put_bytes(buf); }
    buf.len() as i64
}

trap user_fault(esr: u64, far: u64, pc: u64) {       // only the faulting task ends
    critical { /* count it */ }
}

fn core_main(c: u64) {
    // create user tasks in the task table of this core, arm the timer of this core, enter the first task (never returns)
}

fn main() {
    spawn_on(1, core_main, 1);
    spawn_on(2, core_main, 2);
    spawn_on(3, core_main, 3);
    core_main(0);
}
```

With fascon-gc, Ids no longer need to be split into `let`s to survive a collection:

```fascon
gc_arena nodes: Node[8];
fn churn(n: u64) -> Option<Id<nodes>> { gc_collect(nodes); nodes.alloc(Node { v: n, next: None }) }

let s = link(nodes.alloc(Node { v: 1, next: None }), churn(2));
// the temporary of the first argument (a fresh Id) stays a root during the collection of the second (tests/ok/gc_temp_roots.fsc)
```

## 3. Rules and errors

| Code | Meaning |
| --- | --- |
| F2614 | **retired**; it used to reject irq, task_arena, dma_arena, syscall and trap in programs that use `spawn_on` |
| **F2616** (new) | on multi-core, the handler of a shared peripheral interrupt (INTID 32 and up, delivered to CPU0 only) touches a per-core arena (it would look up Ids made on other cores in the table of CPU0) |
| F2601 (wider) | on multi-core an irq body only masks the interrupts of its own core; the effects of every root on other cores (task, syscall, irq) are part of the environment effect E_env, and unprotected access to shared instances is rejected |
| F2603 (wider) | blocking operations inside `critical` (channel `send` / `recv_wait`, task switches and waits) are rejected on multi-core too |
| F2613 (unchanged) | unprotected access to MMIO, ports, `dma` or `dma_arena` reachable from two or more core roots |
| F2622 (wider) | gc_arena allocation, collection or promotion inside `#[trap]`, `rcu_read`, `atomic`, and now also `critical` and `irq` handlers (functions called from there included) |
| F2803 (relaxed) | size and address of `user_mem` are multiples of 4 KiB; the address lies outside the identity map (0 to 0x8000_0000), below 0x80_0000_0000 and inside one 1 GiB slot |

`fascon explain <file>` prints the root-cause timeline, the fix and the mathematical reason (`data/rules.json`) of every
error; `fascon dict F2616` looks up the dictionary (`data/dictionary.json`; the dictionaries of the VS Code extension say the same).

## 4. The nine invariants and how they are kept

| # | Invariant | Implementation and check |
| --- | --- | --- |
| 1 | the language core does not change | no syntax added; F0515 (second-class references) and generation-tagged Ids unchanged (a `&str` constant is still F0515) |
| 2 | programs without the new features compile to byte-identical C | 234 of 240 compared programs are identical; the other 6 are programs that use `spawn_on` built with `--target x86_64` (x86_64 now emits multi-core C) |
| 3 | `critical` = (1) mask interrupts, (2) take the spinlock; release in reverse | `fascon_plat_critical_enter` / `exit` (runtime default: `fascon_plat_irq_save`, then the lock); waits are F2603 |
| 4 | E(f) and E_env are monotone | on multi-core E_env is the union of every root on other cores; per-core arenas (TLS) are excluded, so the branchless latch and column-scan fast paths stay |
| 5 | task state is per core | `f20_sched_t` inside `f20_cpu_ctx_t` (current task, run queue, switch requests); the timer (PPI 27 / LAPIC timer) is enabled on every core |
| 6 | TOCTOU (double reads) is ruled out by construction | on multi-core the system-call entry copies each window once with `fascon_plat_copy_from_user` into a shadow buffer (AArch64: LDTRB / STTRB with an exception table; x86_64: SMAP STAC / CLAC with an exception table) |
| 7 | a panic stops every core | `fascon_plat_panic_broadcast` (AArch64: SGI 2, x86_64: NMI); cores that notice the panic in a waiting loop stop in `fascon_plat_core_parked` and are counted |
| 8 | no external crates, stable ABI | `Cargo.toml` still has no dependencies; F3501 and F3601 unchanged |
| 9 | real-time and GC regions stay apart | F2622 now covers `critical` and `irq`; the safepoint rendezvous of a shared gc_arena happens only in open sections that hold no lock |

## 5. The abstract platform API (`fascon_plat_*`)

The C the compiler emits contains no CPU-specific instruction and calls only these functions (declared in
`runtime/fascon2_rt.h`). Interrupt controllers (GICv2, APIC, ...), page tables and TLB instructions live in the BSPs only.

| Group | Functions | Meaning |
| --- | --- | --- |
| interrupts and locks | `irq_save` / `irq_restore` / `irq_open` / `wait_irq`, `critical_enter` / `critical_exit` | mask, open and wait for interrupts of this core; a closed section is (1) mask, (2) lock |
| clock | `timer_now` / `timer_freq` / `timer_arm` | a monotonic clock shared by all cores and a one-shot timer of this core |
| tasks | `task_create` / `task_create_user` / `task_enter` / `task_yield` / `task_resume` / `task_exit` / `task_current` | operations on the task table of this core; saving and switching contexts is the job of the BSP |
| multi-core | `spawn_on` / `cpu_count` / `cpu_id` | starting cores and their numbers |
| address spaces | `vm_space` / `vm_map` / `vm_unmap` / `vm_switch` / `vm_flush` / `vm_drop` | 4 KiB pages, per-task spaces, TLB shootdown per ASID (PCID) |
| user space | `user_register` / `user_window` / `copy_from_user` / `copy_to_user` / `syscall_register` / `trap_register` | window checks and copies across the boundary |
| panic | `panic_broadcast` / `core_parked` | stopping every core and counting the stopped ones |
| fascon-gc | `gc_task_tops` | the shadow stacks of suspended tasks |

Implementations today:

| Platform layer | Contents |
| --- | --- |
| `bsp/qemu-virt-aarch64/` (`bsp.c`, `genos.c`, `start.S`, `kernel.ld`) | everything above; GICv2 (SGI 1 = generic-os TLB, 2 = panic, 3 = address-space TLB), PSCI, 16-bit ASIDs, TTBR0 switching |
| `bsp/qemu-x86_64-bios/` (`bios.S`, `bsp.c`, `bios.ld`) | everything above; 256-entry IDT, GDT / TSS / IST, LAPIC, I/O APIC, HPET, INIT-SIPI-SIPI, TLS through FS, CR3 + PCID, SMAP / SMEP / NX, IPI 0xF0, NMI |
| `bsp/generic-aarch64/`, `boards/qemu-virt-riscv64/` | interrupts, clock, core count; no tasks and no user space (creating them returns failure) |
| `bsp/portable-stub/` | the porting template (ARMv7, MIPS64, RISC-V64 and others): replace the places marked with a star by the devices of the board; tasks run one at a time to completion |
| `bsp/host/` | PC execution (a core is a thread) |

A volunteer adds a board by writing the functions of this table in C under `boards/<name>/` and describing it in
`board.toml`; the compiler is not touched. `python3 tools/cross_check.py target/release/fascon` checks that generated C
compiles with `-Werror` for five architectures and links with the reference platform layer.

## 6. AArch64 (QEMU virt)

- **Per-core tasks**: the task table is `bsp_tasks[core][slot]`; the context of a task is the address of its saved frame
  (800 bytes) on its own kernel stack, and a switch returns another frame to the exit path of `start.S`.
- **Interrupts**: GICv2. PPIs (16 to 31; the timer at 27 / 30) are enabled per core when that core opens interrupts; SPIs
  (32 and up) go to CPU0.
- **Address spaces**: a per-task L1 table (copying the L1 of the kernel) with L2 / L3 (4 KiB) below it. A switch writes TTBR0
  with a 16-bit ASID and flushes nothing; flushes happen only when a page is unmapped or a space is dropped, and only on the
  cores that used that ASID (`tlbi vae1` / `aside1` locally, a mailbox and SGI 3 elsewhere).
- **Demand paging**: an EL0 translation fault (FSC 4 to 7) inside the user memory of the task maps 4 KiB and retries.
- **Boundary copies**: `LDTRB` / `STTRB` (accesses with EL0 permissions) and an exception table; a page not mapped yet is
  mapped and the access retried, anything else is EFAULT.
- **Shootdown mailboxes**: one sender at a time (a global lock taken with interrupts masked), because a mailbox holds a single
  operand and two simultaneous requests to the same core would overwrite each other; a waiting core keeps serving its own
  mailbox, so cores asking each other never deadlock (the SGI 1 path of `genos.c` works the same way). A boot-time self-test
  sends SGI 3 to every core.

## 7. x86_64 (QEMU pc, the Fascon virtual BIOS)

- **Entry**: all 256 vectors build the same saved frame (FXSAVE 512 bytes + 15 general registers + vector and error code +
  the hardware frame, 688 bytes), and `bsp_x86_trap` returns the frame to resume (the same shape as AArch64).
- **GDT, TSS, IST**: ring 0 / ring 3 segments and one TSS per core; NMI on IST 1, #DF and #MC on IST 2.
- **Clock**: `timer_now` / `timer_freq` are the HPET main counter (100 MHz on QEMU; machines without one use the TSC
  measured against the PIT). `timer_arm` is a one-shot of the LAPIC timer of the core, converted with a ratio measured at
  boot. The LAPIC timer is **irq 27** (or 30 when only 30 has a handler) and input n of the I/O APIC is **irq 32 + n**, so the
  same Fascon source runs on both architectures.
- **Multi-core**: core 0 copies the trampoline to 0x8000 and starts the others with INIT, SIPI, SIPI (the core count comes from
  the fw_cfg of QEMU). Every core points its FS base at the end of its own TLS block (x86_64 TLS variant II; `%fs:0` holds its
  own address).
- **Ring 3**: `int $0x80` (a DPL 3 gate); the number in rax, arguments in rdi, rsi, rdx, r10, r8, r9, r12, r13, the result in
  rax. On entry from ring 3 the FS base of the core, stored at the top of the stack, is put back into FS, so the kernel finds
  its own instances even when user code changed FS.
- **Address spaces**: a per-task PML4 and PDPT (the identity map of 0 to 4 GiB stays shared as global kernel pages) with PD /
  PT below them. With PCID, CR3 is written with the no-flush bit, and PCIDs of other cores are removed with INVPCID or a
  flush-on-next-load mark. Switching to a kernel task keeps the space that is loaded (the kernel half is identical in every
  space and SMEP / SMAP guard the user half).
- **Boundary copies**: with SMAP, ring 3 pages are touched only between STAC and CLAC, and an exception table returns EFAULT;
  the entry path executes `clac`, so SMAP stays effective even when user code sets AC.
- **trap**: ring 3 exceptions reach the trap handler with the classes of the AArch64 ESR (#UD → 0x00, #PF → 0x24 / 0x20 with
  a DFSC, #AC → 0x24 / 0x21, int3 → 0x3C, #DE / #MF / #XM → 0x2C, anything else 0x00 with ISS = the vector); `far` is CR2 and
  `pc` is RIP.
- **Panic**: an NMI to every core but the current one. An NMI also reaches masked sections, so a core may stop while holding
  the console lock; the panicking core takes over a console that stays busy and reports (runtime).
- **generic-os**: the parts that are the same on every architecture run (64-bit Ids, per-core magazines, RCU, shared
  gc_arena); guard pages, VA-reserved arenas, the high device mirror and PAN / PAC / BTI are features of the AArch64 BSP and are
  absent on x86_64.

## 8. fascon-gc

- **Temporaries**: while an expression that may collect (an allocation, or a call of a function that reaches `gc_collect`
  directly or indirectly) is evaluated, the gc Ids evaluated before it (or structs and Options holding them) are moved by the
  compiler into temporary locals registered on the shadow stack with `F20_GC_ROOT`. The registration ends with the statement
  (roots are not kept alive to the end of the block). Expressions that cannot collect get nothing, so existing programs keep
  their C.
- **Local owns (2026-11)**: Ids behind `own T` / `Option<own T>` in locals, parameters, temporaries and return slots are
  roots too, with every move, drop and take clearing the C variable first (SPEC-1.0-UNIVERSAL section 5.3.1).
- **Channels**: `gc_collect` reads, as roots, the published cells (`seq == position + 1`, from head to tail; inside `own`
  through the type descriptors) of every channel whose elements may hold Ids of a shared gc_arena (`F20_CHAN_GC_WALK`).

## 9. Verification (actually run in this environment)

- `python3 tests/run_tests.py target/release/fascon --qemu --fuzz 2`: **all 317 tests pass** (5 of them check the dictionary: `tools/check_dictionary.py`, the sync with the VS Code extension, every entry and alias through `fascon dict` in both languages, and `fascon explain` on every error test).
- Generated C: 234 of 240 programs are byte-identical to the original 1.0.0 (the other 6 as described by invariant 2).
- QEMU virt (AArch64) `-smp 4`: `tests/ok/smp_preempt_user.fsc` (8 EL0 processes plus a hostile one, demand paging, TOCTOU
  check 8/8, E2BIG, `&mut` write-back, timer switches on every core) passes repeatedly; the SGI 3 self-test answers 3 of 3.
- QEMU pc (x86_64) `-smp 4`: `tests/qemu/smp_preempt_user_x86.fsc` (the same scenario in ring 3) passes 10 of 10 runs; 10
  kernel tests (`kernel_preempt`, `critical`, `smp_basic`, `smp_irq_tasks`, `smp_dma_ring`, `gc_basic`, `gc_own_promote`,
  `gc_temp_roots`, `gc_chan_roots`, `gc_shared`) print the same output as on AArch64 (only the value of `timer_freq` is the
  clock of the machine).
- Panic stop on every core: `tests/qemu/smp_panic_broadcast.fsc` and `tests/qemu/smp_panic_console.fsc` (cores stopped while
  printing) report "3 other core(s) stopped" with exit code 70 on both architectures (before the fix, the latter hung on x86_64).
- Hostile user program: a window at a kernel address and a window longer than the user memory both return -14 (EFAULT); an
  undefined instruction reaches the trap.
- Generated C compiled and linked: 9 programs × 5 architectures (ARMv7, MIPS64, RISC-V64, AArch64, x86_64) = 45 builds with
  `-Werror`, no warnings.
- RISC-V64 board package: 4 harts boot on `qemu-system-riscv64` virt with `FASCON-BOARD-OK`.
- New error tests: F2616, F2601 (SMP) × 2, F2603 (SMP), F2613 (DMA), F2622 (critical / irq), F2803 (4 KiB).

## 10. Limits and what was not verified (honestly)

- **QEMU TCG emulates neither PCID nor INVPCID** (and this environment has no KVM). The PCID path of x86_64 is implemented but
  was not executed here; under TCG every CR3 write drops the non-global TLB entries (global kernel pages survive, and switches to
  kernel tasks do not write CR3).
- Tasks never move between cores, and ASIDs / PCIDs are per (core, slot), so ordinary space flushes stay on the owning core;
  shootdowns by IPI / SGI are exercised by the boot-time self-tests and by the generic-os CoW path.
- Boundary copies go one byte at a time (shadow buffers hold up to 0xFFF0 bytes); their speed has not been tuned.
- The ring 3 clock of x86_64 is RDTSC; its rate comes from CPUID 15H / 16H, and CPUs without them (QEMU TCG among them) are
  assumed to run at 1 GHz.
- The `f20_aspace` of generic-os (ownership tables and CoW of processes) is not yet connected to real page tables
  (`fascon_plat_pte_set` is the empty default); the address spaces of EL0 / ring 3 tasks are real page tables.
- The AArch64 virtual BIOS (`--target aarch64`) stays single-core because it has no PSCI (`--target x86_64` is multi-core).
- `bsp/portable-stub/` is a template and does not switch task contexts; ARMv7 and MIPS64 were compiled and linked, not run.
- Nothing was checked on real hardware (boards or PCs). The macOS and Windows executables are cross-built: the macOS one was not run, the Windows one only under Wine.
