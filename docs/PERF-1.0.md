# Fascon 1.0 — branch-free traversal, cache-line layout, huge pages and columns (November 2026 update)

2026-11-06, columns added 2026-11-07, True SoA added 2026-11-21, masked column RMW and float columns added 2026-11-25 · Fascon 1.0.0 (version unchanged) · 日本語: [PERF-1.0.ja.md](PERF-1.0.ja.md)

This note describes the changes made to the 1.0.0 compiler, runtime and BSP to speed up table traversal (`for id in @a`). For each of the three suspected causes (branches, false sharing, TLB misses) it gives what the 1.0.0 code actually did, what changed, the generated C and assembly, and the measurements. Phase 4 then removes what was left: the bytes each scan reads.

## Summary

| Suspected cause | What the 1.0.0 code actually did | What changed |
| --- | --- | --- |
| Dynamic liveness branches (cbnz / jnz) | The 0.9 ns scan (the sum specialization) was already branch-free apart from its back edge. The branches were in the general `for id in @a` (one `link[]` test per slot) and in the commit point at the top of every loop iteration of latch-using functions (`cbnz`) | A live bitmap + ctz (TZCNT / RBIT+CLZ); filtered counts, sums and min / max as branch-free masked scans; commit points moved out of fixed-trip loops; the latch folded into the exit test of while / loop |
| False sharing between cores | The bookkeeping `st`, written by every allocation, shared a line with the start of `gen[]` | `st` gets a 128-byte block of its own and every array starts on a 128-byte boundary; `#[repr(align(N))]`; a per-core unit (512 slots) is defined |
| TLB misses (4 KiB pages) | The BSP already identity-maps RAM with 2 MiB blocks; there are no 4 KiB pages. The MMU is set up once at boot; no code rewrites page tables per call | Without a framebuffer RAM is one 1 GiB block (one level fewer to walk); with one, 2 MiB blocks with the contiguous hint (32 MiB). Arenas live in a 2 MiB-aligned section. PC runs use `madvise(MADV_HUGEPAGE)` |
| MMU work for "one call = one region" | As above, there is none. `own` used the heap free lists (12 classes) | A `let x = own T { .. }` that escape analysis shows is never moved goes to a frame region (the same motion as a stack) |
| Bytes read per task (what remained after phases 1–3) | Every scan read the whole 32-byte slot, however few of its bytes the reduction used | Columns (phase 4): each reduction scans a column of exactly the values it reads (8 or 4 bytes, or 1 bit per task); the compiler keeps the columns in step at every write |
| Writes to a field a column reads took about 2× (the dual write of phase 4) | Mirrored columns wrote both the slot and the column, and the working set exceeded L2 | True SoA (phase 5): eligible fields leave the slot and their column is the only copy (one write); bulk overwrites of large columns use non-temporal stores; write-heavy fields that cannot be True SoA get no mirrored columns |
| A conditional write (`if cond { @a[id].f += x }`) still walked the live bitmap slot by slot (phase 5: 0.97–1.90× C) | The general loop: ctz, a filter test and a checked add per live slot | Masked column RMW (phase 6): the filter bit column is read one 64-slot word at a time; zero words are skipped, all-one words are a plain vector add, mixed words a predicated vector add with per-lane overflow checks; float columns under `#[fast_math]` |

Measured on x86-64 ([bench/RESULTS.md](../bench/RESULTS.md), ns per task): filtered counts and pick_next went from 1.10–1.31 ns to 0.69–0.73 ns, sparse tables from 0.48–0.51 ns to 0.07 ns, and the plain sum scan from 0.71–0.74 ns to 0.57–0.63 ns, which is 1.2–1.3 cycles at 2.10 GHz and inside the target (0.48–0.70 ns). The plain sum had no branch to remove: its gain comes from 2 MiB pages, and what was left was the bandwidth of reading 32 bytes per task.

With the columns of phase 4 (a later session on another host, core clock 3.85 GHz, [bench/RESULTS.md](../bench/RESULTS.md)): the plain sum went from 0.45–0.54 to 0.10–0.21 ns per task, the filtered sum from 0.48–0.82 to 0.05–0.10 ns, pick_next from 0.49–0.63 to 0.07–0.21 ns and the filtered count from 0.28–0.60 to 0.00–0.02 ns. That is under one cycle per 32-byte task in every build (GCC and clang, SSE2 and AVX-512): 0.19–0.81 cycles. Loops that write a field a column reads pay for it (about 2× in a loop that writes `vruntime` in every runnable task); loops that write other fields do not.

After the True SoA of phase 5 (this host, core clock 2.87–3.27 GHz, hand-written C (AoS), before and after run alternately 9 times): with 60,000 tasks, reads stay under one cycle per task in every build (0.01–0.75 cycles), bulk updates and overwrites of every task take 0.44–0.86× the C time (3.1–3.5× before), and the write to runnable tasks only takes 0.97–1.03× C with GCC and 1.47–1.90× with clang (1.9–3.6× before). With 4 million tasks, reads take 0.01–1.03 cycles (C: 5.6–10.5), bulk updates and overwrites 0.10–0.24× C, and the write to runnable tasks 1.11–1.27× (5-7).

## Phase 1 — no branches in scans

### 1-1 The live bitmap and ctz (general `for id in @a`)

Every arena table now has a liveness bitmap `live[]` with one bit per slot (64 slots = one `uint64_t`). `alloc` sets the bit; `free` / `remove` clear it.

```c
/* runtime/fascon2_rt.h (inside acquire / release) */
live[i >> 6] |= UINT64_C(1) << (i & 63u);       /* acquire */
live[i >> 6] &= ~(UINT64_C(1) << (i & 63u));    /* release */

static inline uint32_t f20_ctz64(uint64_t m) { return (uint32_t)__builtin_ctzll(m); }   /* TZCNT / RBIT+CLZ */
static inline uint64_t f20_blsr64(uint64_t m) { return m & (m - 1u); }                 /* BLSR / SUB+AND */
```

A general `for id in @a` reads one word and finds the next live slot with ctz. The 1.0.0 per-slot "test `link[_i] != F20_LIVE` and continue" branch is gone.

```c
/* 1.0.0 */
for (uint32_t _i = 0; _i < arena_process.st.next_unused; _i++) {
    F20_COMMIT(_flt, site);                                   /* cbnz every iteration */
    if (arena_process.link[_i] != F20_LIVE) continue;         /* a branch per slot */
    ...
}

/* now (src/codegen.rs, loop_stmt, ForIter::Arena) */
for (uint32_t _w1 = 0u; _w1 < F20_LIVE_WORDS_USED(arena_process.st.next_unused); _w1++) {
    for (uint64_t _lm2 = arena_process.live[_w1]; _lm2 != 0u; _lm2 = f20_blsr64(_lm2)) {
        const uint32_t _i3 = (_w1 << 6) | f20_ctz64(_lm2);
        const fsc_Id_process id = F20_ID_MAKE(arena_process.gen[_i3], _i3, ARENA_process_K);
        { /* body */ }
    }
}
F20_COMMIT(_flt, site);                                       /* once, at the exit */
```

Only when the body allocates, frees or removes in the same arena (callees included: `Effects` gained `allocs`, closed transitively) is the word reloaded, keeping the live slots above the current index: `arena.live[w] & f20_above_lowest64(m)`. Freed slots are skipped and slots allocated further on are visited, the same meaning as 1.0.0. `break` jumps to a label after both loops.

For the 64-slot `@process` GCC removes the word loop altogether and loads the bitmap as a single word (x86-64, `-mbmi`):

```asm
fsc_tick_all:                              // for id in @process { @process[id].ticks = @process[id].ticks + 1; }
    movl    arena_process(%rip), %eax      // st.next_unused
    addl    $63, %eax
    shrl    $6, %eax
    je      .L142
    movq    128+arena_process(%rip), %rcx  // live[0]: the liveness of all 64 slots in one load
    testq   %rcx, %rcx
    je      .L142
    leaq    arena_process(%rip), %rsi
.L148:
    xorl    %eax, %eax
    tzcntq  %rcx, %rax                     // the next live slot (one instruction)
    andl    $63, %eax
    addq    $24, %rax
    salq    $5, %rax
    addq    %rsi, %rax
    movq    8(%rax), %rdx
    addq    $1, %rdx
    jc      .L160                          // overflow of the checked add (never taken)
    movq    %rdx, 8(%rax)
    blsr    %rcx, %rcx                     // clear the lowest set bit
    jne     .L148
.L142:
    ret
```

On AArch64 (clang 18, `-mcpu=cortex-a72`) it is `rbit` + `clz`. ARM's `BRK` is the breakpoint instruction and cannot scan bits; with FEAT_CSSC (ARMv8.9 / v9.4, `-march=armv8.9-a`) it becomes a single `ctz`.

```asm
.LBB28_5:                                  // once per live slot
    rbit    x12, x11
    clz     x12, x12                       // index of the next live slot
    add     x12, x10, x12, lsl #5
    ldr     x13, [x12, #8]!
    adds    x13, x13, #1
    b.hs    .LBB28_7                       // overflow (never taken)
    str     x13, [x12]
    sub     x12, x11, #1
    ands    x11, x12, x11                  // blsr
    b.ne    .LBB28_5
```

### 1-2 Masked reductions (filtered sums, counts, min / max)

The 1.0.0 sum specialization (loops that only add slot fields) now also covers loops that filter, as branch-free scan functions (`reduction_plan` / `sum_action` / `minmax_plan` / `cond_ok` / `cond_c` / `emit_reduction` in `src/codegen.rs`).

| Body | Scan | How dead slots are excluded |
| --- | --- | --- |
| `if cond { acc += field + .. }` | the table from the start, 4 lanes | fields of dead slots are zero, so ANDing with the filter mask `m = 0 - cond` is enough |
| `if cond { acc += constant }` | per bitmap word, 64 slots × 4 lanes | add live bit AND cond; without a filter it is `st.live × constant` (no scan) |
| `if field < acc { acc = field; pick = Some(id); }` (< <= > >=; more filters with `&&`) | per word, 2 lanes | k = live bit AND cond AND compare |

A filter may only compare slot fields, integer / bool constants, statics and outer locals (which become parameters of the scan function), with `&&` `||` `!` and bitwise operators. None of them has a side effect, so `&&` becomes `&` and everything is evaluated; evaluating them on a dead, all-zero slot is harmless. A word that is all zero (64 dead slots) skips those 64 slots without reading them.

The generated C of a filtered count and its inner loop (AArch64):

```c
for (uint32_t j = 0u; j < 64u; j += 4u) {
    c0 += ((uint32_t)(lw >> (j)) & 1u) & (uint32_t)((p[j].v.state == fsc_RUNNING));
    c1 += ((uint32_t)(lw >> (j + 1u)) & 1u) & (uint32_t)((p[j + 1u].v.state == fsc_RUNNING));
    ...
}
```

```asm
.LBB29_6:                                  // 30 instructions for 4 slots; the only branch is the back edge
    lsr     x0, x16, x17                   // live bit
    and     x0, x0, #0x1
    ldur    w1, [x18, #-48]
    cmp     w1, #1                         // filter
    csel    x0, x0, xzr, eq                // live bit AND filter (CSEL)
    ...
    add     x12, x0, x12
    cmp     x17, #60
    add     x17, x17, #4
    b.lo    .LBB29_6
```

For min / max (CFS pick_next) k = live bit AND filter AND compare is always built without a branch. The update that follows, `F20_PICK`, has two forms:

```c
/* runtime/fascon2_rt.h */
#if defined(FASCON2_PICK_BRANCHLESS)
#define F20_PICK(k, T, bv, v, iv, idx) do { (bv) = (T)F20_SEL64((k), (v), (bv)); (iv) = F20_SEL32((k), (idx), (iv)); } while (0)
#else
#define F20_PICK(k, T, bv, v, iv, idx) do { if (FASCON_UNLIKELY(k)) { (bv) = (v); (iv) = (idx); } } while (0)
#endif
```

- **Default**: the update is one rarely-taken branch. New minima get rarer as the scan proceeds (about ln n times for values in random order), so it almost never mispredicts, and it is faster than re-selecting two values on every slot (pick_next 0.69–0.73 ns). The hard-to-predict filter (`state == RUNNING` and the like) is inside k and never becomes a branch.
- **`FASCON2_PICK_BRANCHLESS`**: the update becomes a conditional select too, so no branch depends on the data at all (for code whose timing must not depend on the data). With clang, `?:` with `__builtin_unpredictable` becomes `CSEL` on AArch64 and `CMOV` on x86-64. GCC sometimes turns `?:` back into a branch when it guesses the branch is predictable, so for GCC the select is written as an all-ones / all-zeros mask (AND / BIC / ORR). With all four compilers (clang / GCC × AArch64 / x86-64) the only branch in the inner loop is the back edge. This form is slower than the default: pick_next 1.5 ns (clang) / 2.6 ns (GCC).

The inner loop with `FASCON2_PICK_BRANCHLESS` (clang 18):

```asm
// AArch64: 28 instructions for 2 slots, CSEL / CSINC, only the back edge branches
.LBB31_6:
    ldur    x1, [x0, #-16]                 // v = vruntime
    cmp     x1, x10                        // v < best ?
    ldur    w2, [x0, #-20]
    ccmp    w2, #1, #0, lo                 // && state == RUNNING
    cset    w2, ne
    lsr     x3, x17, x18
    tst     x3, #0x1                       // live bit
    csinc   w2, w2, wzr, ne
    cmp     w2, #0
    csel    x10, x10, x1, ne               // best = k ? v : best
    add     w1, w11, w18
    csel    w8, w8, w1, ne                 // idx  = k ? i : idx
    ...
    b.lo    .LBB31_6

// x86-64: CMOV
.LBB27_4:
    cmpl    $1, -20(%r15)
    movq    -16(%r15), %r13
    setne   %bpl
    cmpq    %rdx, %r13
    setae   %al
    orb     %bpl, %al
    btq     %r12, %rbx                     // live bit
    setae   %bpl
    orb     %al, %bpl
    leal    -1(%r14), %eax
    testb   %bpl, %bpl
    cmoveq  %r13, %rdx                     // best
    cmovel  %eax, %ecx                     // idx
    ...
    jb      .LBB27_4
```

The lanes are merged once at the end: on a tie `<` / `>` keep the smaller index and `<=` / `>=` the larger, so the result equals the sequential loop (checked against plain C over 30,000 random alloc / free steps in `tests/latch`).

### 1-3 Commit points out of fixed-trip loops

In 1.0.0 every loop of a latch-using function had a commit point (`cbnz`) at the top of each iteration. Loops with a fixed trip count (`for i in a..b`, `for id in @a`) now check once at the exit instead. Every external effect (call, output, MMIO, return) already has its own commit point and the loop always ends, so one check at the exit gives the same guarantee; the body has one jump fewer and becomes something the C compiler can vectorize.

Loops without a fixed trip count (`while`, `loop`) might never end, so they still check every iteration, but the latch is folded into the exit test and shares the exit's branch:

```c
while ((arena_process_at_r(id, &_flt)->ticks < v) && (k < UINT64_C(1000))) && F20_LATCH_OK(_flt)) {
    k = fascon_add_u64(k, UINT64_C(1));
}
F20_COMMIT(_flt, site);
```

```asm
.LBB27_1:                                  // clang AArch64: three conditions, one branch
    cmp     x9, x1
    add     x0, x0, #1
    ccmp    x0, x10, #2, lo
    csinc   w11, w8, wzr, ls
    cmp     w11, #1
    b.ne    .LBB27_1
```

In a loop that follows a chain of Ids from outside, such as `loop { match cur { Some(c) => .., None => break } }`, the latch check stays as a well-predicted branch (the compiler threads it to the loop exit). Choosing the read address from the compare would remove it, but would lengthen the latency of every step of the chain, so that was not done.

## Phase 2 — cache lines and false sharing

### The table layout (redefined)

```c
#define F20_CACHE_LINE 64u
#define F20_DESTRUCTIVE_SIZE 128u   /* Intel's adjacent-line prefetcher, Apple M-series 128-byte lines */

#define F20_DEFINE_TABLE(tag, K, GenT)                                           \
    typedef struct F20_ALIGN128 {                                                \
        F20_ALIGN128 f20_arena_state_t st;                                       \
        uint8_t st_pad_[F20_DESTRUCTIVE_SIZE - sizeof(f20_arena_state_t)];       \
        F20_ALIGN128 uint64_t live[F20_LIVE_WORDS(K)];                           \
        F20_ALIGN128 GenT gen[F20_SLOTS(K)];                                     \
        F20_ALIGN128 uint32_t link[F20_SLOTS(K)];                                \
        F20_ALIGN128 f20_slot_##tag##_t slot[F20_SLOTS(K) + 1u];                 \
    } f20_arena_##tag##_t
```

- **Metadata does not share lines with data (SoA).** A live mark inside the slot would make scans read the lines of dead slots and make every slot bigger. The live bitmap is 8 KiB even for 65,536 slots and stays in L1.
- **`st`, written by every allocation, has a 128-byte block to itself.** In 1.0.0 `gen[]` started right after the 32-byte `st`, so an allocation invalidated the line holding `gen[0..]` that other cores read for generation checks. Not any more.
- **The per-core unit is 512 slots (`F20_SHARD_SLOTS`).** For 512 slots `live[]` is 64 bytes and `gen[]`, `link[]` and `slot[]` are multiples of 128 bytes. Every array starts on a 128-byte boundary, so the boundaries between cores fall on 128-byte boundaries in every array and each line is written by one core only. The 64-slot `@process` fits in one unit; to split it between cores, give each core its own arena.
- **Slots stay 16-byte aligned.** Scan speed is set by the bytes per slot: making every slot 64 bytes would read four times as much for a 16-byte Task. Use `#[repr(align(N))]` only on tables that need one slot per line (per-CPU run queues and the like).

The small, hot bookkeeping (`st`, `live`, `gen`) comes first and the big slot array last. A table starts on a 2 MiB boundary, so the bookkeeping and most of the slots share the first 2 MiB page. Putting `slot[]` first was tried as well: scattered chains got 20–30 % slower with GCC (`gen[]` then sits 2 MiB away from the slots), so it was not kept. That comparison was made before huge pages were confirmed to be in effect.

### `#[repr(align(N))]`

```rust
#[repr(align(64))]
struct PerCpu { ticks: u64, runnable: u32 }

arena @cpu: PerCpu[4];          // slots on a 64-byte boundary (one slot per line)
```

```c
struct __attribute__((aligned(64))) fsc_PerCpu { uint64_t ticks; uint32_t runnable; };
_Static_assert(_Alignof(struct fsc_PerCpu) >= 64u, "#[repr(align(64))]");
```

N is a power of two from 1 to 4096 (anything else is **F2031**). Own regions are 16-byte aligned, so putting a type aligned to more than 16 bytes (or a type that contains one by value) into `own` is **F2032**.

## Phase 3 — the MMU and huge pages

### Page tables (`bsp/qemu-virt-aarch64/bsp.c`)

With a 39-bit VA (T0SZ = 25) and the 4 KiB granule a table walk starts at level 1 (1 GiB per entry). 1.0.0 walked RAM through level 1 → level-2 2 MiB blocks (two levels).

```c
#define PTE_CONT (1ull << 52)  /* Contiguous */

if (!fb) {
    /* no framebuffer: RAM (1-2 GiB) is one 1 GiB block; the walk ends at level 1 (one level) */
    l1_table[1] = RAM_BASE | PTE_VALID | PTE_ATTR(1) | PTE_AF | PTE_SH_INNER;
} else {
    /* framebuffer: 2 MiB blocks; every 32 MiB-aligned group of 16 with the same attributes gets the contiguous bit */
    l1_table[1] = (uint64_t)(uintptr_t)l2_ram | PTE_TABLE;
    ...
    for (uint32_t g = 0; g < 512u / 16u; g++)
        if (/* all 16 share the attributes */) for (uint32_t k = 0; k < 16u; k++) l2_ram[g * 16u + k] |= PTE_CONT;
}
```

The tables are written once at boot and never rewritten while running, and there are no 4 KiB (level-3) pages anywhere.

```text
[bsp] MMU + caches on (identity map, no 4 KiB pages: RAM = one 1 GiB block, 1-level walk)
[bsp] arenas: 3 tables, 0x0000000040200000..0x0000000040205a80 (22 KiB, 2 MiB aligned) -> inside the 1 GiB RAM block (1 TLB entry for all RAM)

[bsp] MMU + caches on (identity map, no 4 KiB pages: RAM in 2 MiB blocks, 31 of 32 groups contiguous = 32 MiB TLB entries; framebuffer pages are Normal non-cacheable)
```

Whether a CPU holds a 1 GiB block or a contiguous range in one TLB entry is implementation-defined; CPUs that do not still run correctly and still get the shorter walk. QEMU's TCG does not model a TLB, so no speed difference shows on QEMU.

### Where arenas live

The generated C places arenas in `.bss.fascon_arena`, and tables of 1 MiB or more get a 2 MiB boundary. The BSP linker script gathers them into a 2 MiB-aligned `.fascon_arena`, which `start.S` zero-fills.

```c
#define F20_ARENA_STORAGE(T) __attribute__((section(".bss.fascon_arena"), \
        aligned(sizeof(T) >= F20_HUGE_MIN ? F20_HUGE_PAGE : F20_DESTRUCTIVE_SIZE)))
static f20_arena_t(task) arena_task F20_ARENA_STORAGE(f20_arena_t(task));
```

```ld
.fascon_arena (NOLOAD) : ALIGN(0x200000) {
    __arena_start = .;
    *(.bss.fascon_arena .bss.fascon_arena.*)
    . = ALIGN(16);
    __arena_end = .;
}
```

PC runs (`fascon run` on Linux) apply `madvise(MADV_HUGEPAGE)` to arenas of 1 MiB or more before their first touch; `FASCON_NO_HUGEPAGE=1` turns it off. With a huge page in effect, the plain sum got 13–17 % faster and scattered Id chains 16–31 % faster on an x86-64 PC (see Measurements). Only whole 2 MiB-aligned ranges can become huge pages, so the 2.4 MiB table of the benchmark gets one 2 MiB page and keeps 4 KiB pages for its last 0.4 MiB. Whether Linux grants the page depends on the system (THP mode `madvise` or `always`, free 2 MiB pages); `AnonHugePages` in `/proc/<pid>/smaps` shows it. macOS and Windows executables cannot carry 2 MiB alignment, so they get 128-byte alignment only.

### "One call = one region" as a frame region

A `let x = own T { .. }` that escape analysis (`FlowResults::moved_locals`) shows is never moved is taken from a pre-reserved frame region instead of the heap free lists:

```c
uint8_t *_fm2 = fascon_frame_mark();
fsc_Big *_o3 = (fsc_Big *)fascon_frame_alloc(FASCON_OWN_SPAN(fsc_Big), "frame.fsc:19:13");
*_o3 = (fsc_Big){ ... };
fsc_Big *x = _o3;
...
fascon_frame_release(_fm2);    /* the end of x's scope (also on break, continue and return) */
```

- Allocation is one inlined compare and one add; release resets the top pointer (the same motion as a stack). Neither touches the MMU or page tables.
- An `x` inside a loop is released every iteration, so the frame does not grow with the trip count. A callee releases its frame before it returns, so the order stays last-in, first-out.
- An immutable `x` cannot have its root replaced, so even the root's free (the header check) is skipped and only its children are dropped.
- An own that is returned, stored in a struct or arena, passed by value or dropped stays on the heap. When the frame is full, allocation falls back to the heap (same result).
- The BSP registers the `.frame` section of its linker script (1 MiB) with `fascon_frame_init`; PC runs default to 64 KiB.
- `FASCON2_CPU_LOCAL` is empty in single-core builds and `__thread` when `FASCON2_SMP` is defined, so each core gets its own frame region ([SPEC-1.0-SMP.ja.md](SPEC-1.0-SMP.ja.md)). `FasconFrame` is 128-byte aligned, so per-core frames never share a line.

## Phase 4 — columns: a reduction reads only what it needs

After phases 1–3 the plain sum had no branch left and still took 1.2–1.3 cycles per task, because it read the whole 32-byte slot to add 8 bytes of vruntime. Phase 4 removes those bytes. For every `for id in @a` reduction in the program the compiler adds a column to the table holding exactly the values that reduction reads, and the scan reads that column instead of the slots. The source does not change, and `@a.get(id)` still returns a pointer into the slot: the slot stays authoritative and the columns are mirrors that the compiler keeps in step (with phase 5, the column becomes the only copy of eligible fields). `--columns off` (or `FASCON_COLUMNS=off`, or the VS Code setting `fascon.columns`) gives back the 1.0.0 tables and scans.

### 4-1 Which column for which reduction

| Reduction (as `reduction_plan` recognizes it) | Column | Per slot | Scan |
| --- | --- | ---: | --- |
| sum `s += @a[id].f` (several terms: one column each) | aggregate column `agg[i] = f` | 8 B (u64) / 4 B (32 bits or fewer) | `f20_col_sum_u64` / `f20_col_sum_u32` |
| filtered sum `if c { s += @a[id].f }` | aggregate column `agg[i] = c ? f : 0` | 8 B / 4 B | the same |
| filtered count `if c { n += k }` | bit column: bit i = live && c | 1 bit | `f20_col_count_bits` (one popcount per 64 slots) |
| min / max pick `if @a[id].f < best (&& c) { best = ..; pick = Some(id); }` | key column `key[i] = live && c ? enc(f) : NONE` (max: `~enc(f)`) | 8 B | `f20_col_pick_key` (signed minimum) |

enc(v) is v ^ 2^63 for u64 (it maps the unsigned order onto the signed order) and a plain widening for every other integer type; NONE is INT64_MAX. Dead slots are 0 in aggregate columns, 0 in bit columns and NONE in key columns, so the scans need neither the filter nor a live mask: both are already in the column. Reductions that need the same column (same kind, value and filter) share it. An unfiltered count needs no column (it is `st.live`).

Two kinds of reductions keep the phase-1 scan: a filter that reads a local variable (a parameter, say; its value is not known when the element is written), and a 64-bit `<=` / `>=` pick (exactly one value has the key NONE, UINT64_MAX for a min, and with `<=` that value can be the answer; with `<` it never can). `fascon regions` lists every column, its size and the lines that read it:

```text
  reductions that read columns (they scan a column of just the values they read, never the slots): 4 (the columns add 1,843,236 B to the tables)
    task.agg0_vruntime           aggregate column (u64)     8 B/slot          524,296 B  (line 93)
    task.agg1_prio               aggregate column (u32)     4 B/slot          262,148 B  (line 93)
    task.fb2                     filter bit column          1 bit/slot          8,200 B  (line 106)
    task.min3_vruntime           pick-key column (min)      8 B/slot          524,296 B  (line 119)
    task.agg4_vruntime           aggregate column (u64)     8 B/slot          524,296 B  (line 131)
  places that rebuild column mirrors (writes by Id, ends of &mut borrows, calls given &mut): 1
```

The columns sit right after `live[]` (`F20_DEFINE_TABLE_COLS`), each on a 128-byte boundary, so everything the scans read is at the front of the table:

```c
typedef struct F20_ALIGN128 {
    F20_ALIGN128 uint64_t agg0_vruntime[F20_COL_LEN(ARENA_task_K)]; /* aggregate column: (*v).vruntime (the sum on line 93) */
    F20_ALIGN128 uint32_t agg1_prio[F20_COL_LEN(ARENA_task_K)];     /* aggregate column: (*v).prio (the sum on line 93) */
    F20_ALIGN128 uint64_t fb2[F20_COL_WORDS(ARENA_task_K)];         /* bit column: live && ((*v).state == fsc_RUNNING) */
    F20_ALIGN128 int64_t min3_vruntime[F20_COL_LEN(ARENA_task_K)];  /* key column: live && (state == RUNNING) ? enc(vruntime) : NONE */
    F20_ALIGN128 uint64_t agg4_vruntime[F20_COL_LEN(ARENA_task_K)]; /* aggregate column: (state == RUNNING) ? vruntime : 0 */
} f20_cols_task_t;
F20_DEFINE_TABLE_COLS(task, ARENA_task_K, f20_gen16_t, f20_cols_task_t); /* st | live | col | gen | link | slot */
```

### 4-2 Keeping the columns in step with the slots

Columns are written only by code the compiler emits where an element can change:

| Where | What is emitted |
| --- | --- |
| `@a.alloc(v)` | after the value is stored, `arena_<n>_mirror(&slot[i].v, i)` (every column) |
| `@a.free(id)` / `@a.remove(id)` | `arena_<n>_unmirror(i)`: aggregates 0, bits 0, keys NONE |
| a write by Id (`@a[id].f = x`, `+=`, a nested field, the whole element) | the element pointer is kept in a temporary inside the place expression; after the store, the columns this write can reach are rebuilt (`arena_<n>_mirror_m<mask>`); a write to a field no column reads emits nothing |
| `if let Some(p) = @a.get_mut(id)` / `match` | at every exit of the scope (its end, `break`, `continue`, `return`), the columns the writes through `p` can reach (assignments, `&mut p.f` arguments, `p` passed as `&mut`) |
| `match @a[id].f { q => .. }`, `match @a[id] { t => .. }`, `if let Some(q) = @a[id].o` (a `&mut` binding of a place inside the slot) | at every exit of the arm (its end, `break`, `continue`, `return`), the columns the writes through the binding can reach (the path of the place followed by the paths written through the binding); the element pointer is kept inside the place expression, as for writes by Id |
| a call given `&mut @a[id]` or `&mut @a[id].f` | right after the call, the columns below that path |

A mirror can be stale during a borrow without anyone seeing it: while a `&mut` reference into `@a` is live, 1.0 rejects every read of `@a` that overlaps the borrowed place (all of `@a` for a `get_mut` binding or a whole element), every call that reads `@a` and every overlapping write by Id (F2502). A reduction reads every field its column depends on, so a reduction whose column the borrow can reach cannot run before the borrow ends, and the borrow ends exactly where the mirror is rebuilt. `tests/err/f2502_*` pin this rule down.

Which columns a write reaches: every column records the element field paths it reads (its value and every field of its filter). A write to the path P reaches a column when P is a prefix of one of those paths: `@a[id] = ..` (P = []) reaches all of them, `@a[id].s = ..` reaches those that read `s.state`, `@a[id].files += 1` reaches none. Places behind `own` and inside arrays never reach a column, because columns read neither. The index comes from the pointer difference; a write through a stale Id has gone to the slot sentinel (phase 1), and its mirror goes to the column sentinel `F20_COL_SENTINEL(K) = max(2^K, 64)`, which no scan reads. Key columns are filled with NONE one 64-slot word at a time, by the allocation that first uses that word.

```c
static inline FASCON_MAYBE_UNUSED fsc_Id_task arena_task_alloc(const fsc_Task *v) {
    const f20_id_t id = f20_arena_acquire(&arena_task.st, arena_task.gen, arena_task.link, arena_task.live, ARENA_task_CAP, ARENA_task_K);
    if (id != 0u) {
        const uint32_t i = F20_ID_INDEX(id, ARENA_task_K);
        arena_task.slot[i].v = *v;
        if ((i & 63u) == 0u && arena_task.st.next_unused == i + 1u) { /* a fresh 64-slot word: NONE into every key column */
            f20_col_key_fresh(arena_task.col.min3_vruntime, i);
        }
        arena_task_mirror(&arena_task.slot[i].v, i);
    }
    return id;
}
static inline FASCON_MAYBE_UNUSED void arena_task_mirror(const fsc_Task *F20_RESTRICT v, uint32_t i) {
    arena_task.col.agg0_vruntime[i] = (uint64_t)(*v).vruntime;
    arena_task.col.agg1_prio[i] = (uint32_t)(*v).prio;
    F20_BIT_PUT(arena_task.col.fb2, i, ((*v).state == fsc_RUNNING));
    arena_task.col.min3_vruntime[i] = (((*v).state == fsc_RUNNING)) ? F20_KEY_U64((*v).vruntime) : F20_KEY_NONE;
    arena_task.col.agg4_vruntime[i] = (((*v).state == fsc_RUNNING)) ? (uint64_t)(*v).vruntime : 0u;
}
```

### 4-3 The scans

The reduction functions keep their 1.0.0 names and signatures, so the call sites do not change:

```c
/* line 93: for id in @task { if let Some(t) = @task.get(id) { s = s + t.vruntime + (t.prio as u64); } } */
static FASCON_MAYBE_UNUSED uint64_t fsc_reduce_task_1(const f20_arena_t(task) *__restrict__ arena, uint32_t *__restrict__ flt, uint64_t acc) {
    const uint32_t n = F20_COL_SCAN_END(arena->st.next_unused);       /* next_unused rounded up to 64 */
    const uint64_t s0 = f20_col_sum_u64(arena->col.agg0_vruntime, arena->live, n, flt);
    const uint64_t s1 = f20_col_sum_u32(arena->col.agg1_prio, arena->live, n);
    uint64_t r = acc;
    uint32_t ov = 0u;
    ov |= (uint32_t)__builtin_add_overflow(r, s0, &r);
    ov |= (uint32_t)__builtin_add_overflow(r, s1, &r);
    *flt |= ov << 1; /* F20_FAULT_ARITH */
    return r;
}
/* line 119: pick_next */
static FASCON_MAYBE_UNUSED void fsc_reduce_task_3(const f20_arena_t(task) *__restrict__ arena, uint64_t *__restrict__ acc, fsc_Id_task *__restrict__ pick) {
    int64_t best = F20_KEY_U64(*acc);
    const uint32_t i = f20_col_pick_key(arena->col.min3_vruntime, arena->live, F20_COL_SCAN_END(arena->st.next_unused), &best, 0);
    if (i != 0xFFFFFFFFu) {
        *acc = ((uint64_t)F20_UNKEY_U64(best));
        *pick = F20_ID_MAKE(arena->gen[i], i, ARENA_task_K);
    }
}
```

The kernels are in `runtime/fascon2_rt.h` and are written with the GCC / clang vector extension only (no intrinsics), at the target's own width: 64 bytes with AVX-512VL, 32 with AVX2, 16 otherwise (SSE2, NEON, or plain scalar code where there is no SIMD). Wider-than-hardware vectors are avoided on purpose: GCC splits them into per-lane scalar code.

- **Sums** add each 64-slot block with four independent vector accumulators. A block whose live-bitmap word is 0 is skipped (one predicted branch per 64 slots). Overflow leaves the loop: the u64 kernel also ORs the values, and when bit length(OR) + bit length(n) ≤ 64 no partial sum can reach 2^64, so the plain sum is exact; only otherwise does an exact pass (low and high halves summed separately) run, and it latches F20_FAULT_ARITH when the total reaches 2^64. That is the verdict of sequential checked additions, because the values are unsigned. The u32 kernel reads the column as u64 lanes and splits them with a mask and a shift; 2^24 slots of values below 2^32 cannot wrap a 64-bit lane.
- **Counts** are one popcount per 64 slots (x86-64: POPCNT, AArch64: CNT + ADDV).
- **Picks** take the vector signed minimum of each 64-slot block (four chains; AVX-512VL: VPMINSQ, AVX2: VPCMPGTQ + VPBLENDVB, AArch64: CMGT + BSL) and re-select the best block with CMOV / CSEL (`<`: an earlier block wins a tie, `<=`: a later one). Then, in the winning block only, one bit per "key == minimum" and the first set bit (TZCNT / RBIT+CLZ) or the last (LZCNT / CLZ). The result is the one of the sequential loop: for `<` the smallest index among the minima, for `<=` the largest; `*best` comes in as the key of the value before the loop, so only strictly better (or, for `<=`, equal) values are picked. Where a vector 64-bit compare is not one instruction (plain SSE2, ARMv7, no SIMD) the block minimum is four scalar CMOV / CSEL chains (`F20_VEC_I64_CMP=0`).
- **GCC on x86** folded each vector load into the memory operands of two instructions and so read every 16 bytes twice; on SSE2 the sum was load-bound (0.29 ns per task). An empty `asm` keeps the loaded vector in a register (`F20_KEEP`, GCC on SSE2 targets only), which brought it to 0.20 ns.
- `fascon run` now compiles the generated C with `-march=native` (x86-64) or `-mcpu=native` (AArch64) so the widest vectors are used (`--cpu baseline` for a portable binary).

### 4-4 What it costs

- **Memory:** 4 to 8 bytes × 2^K per column (a bit column: 2^K bits). In `bench/sched_bench.fsc` the five columns add 1.84 MB to a 2.50 MB table. Columns live in `.bss` like the table, so untouched pages cost nothing on a PC; the QEMU BSP clears `.bss` at boot as before.
- **Writes that reach a column:** a few stores per write. In a loop that writes such a field in every runnable task (`charge_running`, `vruntime += 1`), the loop took about twice as long, because the column lines it now touches push the working set past the 2 MiB L2 (the extra instructions are a shift, an XOR and three stores; even one extra store per write accounted for most of the difference). Writes to fields no column reads are unchanged (`tick_running`, `files += 1`).
- Programs that mostly write and rarely scan can use `--columns off`.
- With phase 5 this dual write is gone for fields that can be True SoA, and write-heavy fields that cannot get no mirrored columns (`--columns mirrored` keeps this section's behaviour).

### 4-5 How it was checked

- `tests/cols/cols.fsc` covers every reduction shape (sums of one and several terms, u8 to u64, filtered sums, counts with filters that are true on a zeroed slot, keyed min / max with `<`, `>`, `<=`, the `if let Some(t) = @a.get(id)` form, the two kinds that keep the 1.0.0 scan, a 4-slot arena whose column sentinel is 64, values near 2^62 that need the exact pass) after every write path (alloc, free, remove, plain, compound, nested and whole-element writes by Id, `get_mut` scopes left by their end, `break`, `continue` and `return`, `&mut` arguments, `take()`, writes in bitmap loops, array fields), and in a second phase after writes through `match` / `if let` bindings of places inside the slot (a field, the whole element, nested, as a value, left by `break`, `continue` and `return`, in a bitmap loop). Its output is identical with `--columns on` and `--columns off`, on the PC and on BIOS-less QEMU (NEON, `-mstrict-align`); the first phase also prints the same as the released 1.0.0 compiler, which cannot build the second (generating C for those bindings is the bug fixed in that update; see the changelog). Removing the new mirror calls from the generated C changes 41 lines of the second phase's output.
- `tests/cols/colfuzz.py` writes random programs (random element layouts, filters, reductions and write paths, stale-Id writes that latch, sums that overflow). About 1,400 of them (the last 300 on the final code) were run with `--columns on` and `off` and compared, with identical output every time: GCC and clang, SSE2, SSE4.2, AVX2 and AVX-512, GCC's AddressSanitizer + UndefinedBehaviorSanitizer, and AArch64 under qemu-user. `run_tests.py --fuzz N` runs N of them (8 by default).
- The generated C also compiles for ARMv7 (NEON and soft-float), i386 (with and without SSE2), RISC-V 32 / 64, AArch64 with `-mstrict-align`, and x86-64 without SSE2.

## Phase 5 — True SoA, non-temporal stores and the exclusion of write-heavy fields

The columns of phase 4 are mirrors (Mirrored-SoA): the slot stays authoritative, so a write to a field a column reads writes both the slot and the column (a dual write). In a loop that writes such a field across the table, that cost about twice the time and pushed the working set past L2 (4-4). Phase 5 takes eligible fields out of the slot and makes their column the only copy (True SoA: a write is one store to the column), uses non-temporal stores for bulk overwrites of large columns, and gives write-heavy fields that cannot be True SoA no mirrored columns. The source does not change, and the version stays 1.0.

`--columns` now has three settings: `on` (the default: True SoA + mirrored columns + the write-heavy exclusion), `mirrored` (phase 4 as it was: no True SoA, no exclusion) and `off` (the 1.0.0 tables). On the 59 programs in `tests/ok`, `tests/cols`, `tests/latch`, `bench` and `examples`, the C generated with `--columns mirrored` and with `--columns off` is byte-for-byte the output of the compiler before this change.

### 5-1 When a field becomes True SoA

Candidates are top-level 32- / 64-bit integer fields (u32, i32, u64, i64) that reductions read as values. A field becomes True SoA when all of the following hold; otherwise it stays on mirrored columns (Mirrored-SoA) or the 1.0.0 scan (AoS), and `fascon regions` prints why.

| Condition | The reason printed otherwise (`fascon regions`) |
| --- | --- |
| a table arena (not shared, task, dma, bump, gc or a process table) | `a shared arena (one instance for every core): never True SoA`, etc. |
| the element is not `#[stable_abi]`, `#[wire]`, `#[secret]` or `#[repr(align(N))]` and holds no own | `the element ... is #[stable_abi] (fixed layout)`, etc. |
| neither the element type nor an `Id` of this table appears in a parameter or the return value of an `extern "C"` function (in 1.0 only integers, bool, Id, Option<Id> and FaultError cross that boundary, so a table reaches it through its Ids) | `the element ... or an Id of @a crosses the extern "C" boundary of ...` |
| the function of `#[on_drop(f)]` does not touch the same arena | `#[on_drop(f)] touches the same arena` |
| no reference to an element escapes: bindings of `@a.get(id)` / `get_mut(id)` only read and write fields; no `&@a[id]`, no method call on an element, no `match` / `if let` binding an element. Whole-element reads and writes (`@a[id] = T { .. }`, `@a[id].clone()`) are rebuilt | `a reference to the element ...` / `a get / get_mut binding ... escapes`, etc. (with the line) |
| the field's address is never taken (`&@a[id].f`) and the field is not matched on or used as a method receiver | `its address is taken (&@a[id].f, line N)` (only this field stays mirrored) |
| the field appears in no filter, no reduction that keeps the 1.0.0 scan reads it, and it is not atomic or `#[no_column]` | `appears in a filter` / `read by a reduction that keeps the 1.0.0 scan` |
| not usize / isize (pointer-sized) and not narrower than 32 bits | `usize / isize (pointer-sized) stays a mirrored column`, etc. |

The decision looks at the whole program, table by table (`plan_tsoa` in `src/codegen_tsoa.rs`). Fields that appear in filters are mirrored into the filter bit column (live && filter) as before and stay in the slot.

### 5-2 The generated C

The table of the Task in `bench/tsoa_bench.fsc.in` (`pid, prio, vruntime, parent, next, files, state`) once `vruntime` is True SoA. The slot holds the remaining 24 bytes and is not raised to a 16-byte boundary (a True-SoA table never crosses an FFI boundary; raising it to 32 bytes slowed the loop that reads `state` and writes `vruntime` from 1.03× to 1.34× C with 60,000 tasks and from 1.14× to 1.32× with 4 million, GCC baseline, median of 3 rounds; the other rows did not change).

```c
#define ARENA_task_TRUE_SOA 1 /* a True-SoA table (field f is arena_<name>.col.ts_f[i]) */
#define ARENA_task_TS_vruntime 1
typedef struct {
    uint32_t pid;
    uint32_t prio;
    fsc_Id_task parent;
    fsc_Id_task next;
    uint32_t files;
    uint32_t state;
} f20_part_task_t;                    /* the part left in the slot */
typedef struct {
    f20_part_task_t v;
} f20_slot_task_t;                    /* natural alignment (24 B) */
typedef struct F20_ALIGN128 {
    F20_ALIGN128 uint64_t fb0[F20_COL_WORDS(ARENA_task_K)];        /* mirror: live && state == RUNNING */
    F20_ALIGN128 uint64_t ts_vruntime[F20_COL_LEN(ARENA_task_K)];  /* the only copy (dead slots 0) */
} f20_cols_task_t;
```

Columns still have `F20_COL_LEN(K) = F20_COL_SENTINEL(K) + 1` elements with the sentinel `F20_COL_SENTINEL(K) = max(2^K, 64)` at the end (`col[2^K]` for K ≥ 6); no scan reads it.

| Where | The True-SoA C |
| --- | --- |
| a proven `@a[id].f` | `arena_task.col.ts_f[F20_COL_OF_SLOT(F20_ID_INDEX(id, K), K)]`; for the loop variable of `for id in @a`, the loop's slot index itself (no `gen[]` read) |
| a read of an unproven `@a[id].f` | `arena_task_ix_r(id, &_flt)`: the generation compare only feeds the latch (as `_at_r`); a stale Id reads a value of the same type in the column, or 0 |
| a write to an unproven `@a[id].f` | `arena_task_ix(id, &_flt)`: a wrong generation is diverted, without a branch, to the sentinel index and latched (`(i & (bad - 1)) \| (F20_SENTINEL(K) & -bad)`, mapped to the column sentinel by `F20_COL_OF_SLOT`); no real element is touched. Elements with drop glue (`#[on_drop]`) stop on the spot with `arena_task_ix_strict` (as `_at_strict`) |
| `t.f` in `if let Some(t) = @a.get(id)` | `t` points to the part in the slot; `t.f` is `ts_f[arena_task_ix_p(t)]` (the pointer difference is the slot index) |
| `alloc` | `arena_task_put(i, v)`: the remaining part goes to the slot, the True-SoA values to the columns (initial values are written to the columns), and the mirrored columns are rebuilt |
| `free` / `remove` | the slot is zero-filled and `arena_task_ts_zero(i)` zeroes the True-SoA values; `remove` returns `arena_task_load(i)` (rebuilt from the slot and the columns) |
| `#[on_drop]` | `free` runs the drop glue on a temporary rebuilt by `arena_X_load(i)`, so the `on_drop` function sees the column values; when alloc fails, the value passed in is dropped |
| `@a[id] = T { .. }` | the place (index) → the new value → the old value's drop → `arena_task_put` (the same evaluation order as before) |
| `@a[id].clone()` | a copy of the temporary rebuilt by `arena_task_load` |

Writes to True-SoA fields need no mirror rebuild (in `bench/tsoa_bench.fsc`, "places that rebuild column mirrors" went from 3 to 0).

### 5-3 Scans (the mask is applied at scan time)

Filtered reductions over True-SoA values use the filter bit column (or the live bitmap) as a mask at scan time; no mirrored aggregate or key column is kept.

| Reduction | Function (`runtime/fascon2_rt.h`) |
| --- | --- |
| sum (no filter) | `f20_col_sum_u64` / `f20_col_sum_u32` (dead slots are 0 in the column; the phase-4 functions) |
| filtered sum | `f20_col_sum_u64_m` / `f20_col_sum_u32_m`: per 64-slot block, a zero mask word skips the block; inside, branch-free vector adds of the values ANDed with per-lane masks (an AND and a subtraction from 0). On x86 SSE2 / SSE4.2, which have no per-lane variable shift, 4 mask bits index a 512 B table instead (no instructions build the masks). Overflow uses the phase-4 OR bound |
| min / max pick | `f20_col_pick64_m` / `f20_col_pick32_m`: keys are made on the fly (`key = (int64)(value ^ x)`, x a constant for the type and direction). Stage 1 folds the lanes once per 256 slots; stage 2 finds the first (`<` / `>`) or last (`<=` / `>=`) match in the winning 256 slots. On plain SSE2, an all-ones word runs four CMOV chains and other words walk only their set bits. Same result as the sequential loop |
| count | `f20_col_count_bits` (unchanged) |

These functions were checked against a plain reference with random columns, masks, types, directions and `<` / `<=` (20,000 rounds per type) under GCC and clang with SSE2, SSE4.2, AVX2 and AVX-512.

### 5-4 Non-temporal stores and prefetch

`runtime/fascon2_rt.h` gains `F20_NT_STORE_U64` / `_U32` / `_I64` / `_I32` (clang: `__builtin_nontemporal_store`; GCC on x86-64: a `MOVNTI` asm; plain stores elsewhere), `F20_SFENCE()` (x86: `SFENCE`; elsewhere a release fence) and `F20_PREFETCH_R(p)`.

The compiler treats a `for id in @a` whose body is only `@a[id].f = value;` statements (f a True-SoA field, the value a pure expression that does not read f) as a bulk overwrite and decides with `--nontemporal`; every loop that gets non-temporal stores gets `F20_SFENCE()` at its exit.

| `--nontemporal` | Loops that get non-temporal stores |
| --- | --- |
| `auto` (default) | columns of 8 MiB or more, in tables not shared with interrupt handlers or tasks (`E_env`), when the following statements (up to the next loop) do not read the same column |
| `on` | every bulk-overwrite loop (tables shared with interrupts too; `F20_SFENCE` at the exit); scans of large tables' columns also get prefetch (`F20_PREFETCH_DIST 1024`) |
| `off` | none |

Shared arenas and dma_arenas are never True SoA, so they never get non-temporal stores. `#[no_column]` on a field keeps every column (mirrored or True SoA) off it.

The 8 MiB lower bound was measured, not taken from the assignment's example (4096 elements / 64 KiB). On this host, for the loop writing `vruntime = x` in every task: with 60,000 tasks (a 480 KB column) non-temporal stores were 32 % slower under GCC (0.445 ns against 0.337 ns) and about the same under clang (0.664 ns against 0.680 ns); with 4 million tasks (a 32 MiB column, which fits in this host's 260 MiB L3) they were 14–16 % slower (0.445–0.450 ns against 0.384–0.394 ns); with four tables of 2^24 tasks (512 MiB of columns, beyond the L3) they were 2.3× faster (0.49 ns against 1.13–1.14 ns). Non-temporal stores pay off when the column exceeds the last-level cache; 8 MiB is where ordinary machines' L3 starts to be exceeded. On machines with a large L3 like this one, columns between 8 MiB and the L3 size lose about 14 %.

`auto` adds no prefetch: software prefetch 256–2048 B ahead in the column scans of 4 million tasks gained nothing on this host (the plain sum stayed at 0.273 ns; some cases got slower), because hardware prefetch already reaches the bandwidth limit. On boards with weak hardware prefetch, `--nontemporal=on` or `-DF20_PREFETCH_DIST=1024` adds it.

### 5-5 Excluding write-heavy fields

For fields that cannot be True SoA and would stay on mirrored columns, the compiler compares, over the whole program, a scan weight with a bulk-write weight. A site at loop depth d weighs 4^d (one sweep of the table = 1): a reduction 4^d, a write inside `for id in @a` 4^(d−1), a single write outside such a loop 4^d / capacity. Reductions that read a field whose write weight exceeds its scan weight get no mirrored columns and fall back to the 1.0.0 scan (the slots, AoS). In `tests/cols/cols.fsc`, `task` (which has a match binding an element, so it cannot be True SoA) has two such fields, `vruntime` (bulk writes 8.04 > scans 5.00) and `ticks` (4.06 > 3.00), and `fascon regions` prints:

```text
  reductions kept on the phase-1 scan (live bitmap + slots) without columns:
    task line 28: `vruntime` is write-heavy (bulk-write weight 8.04 > scan weight 5.00; the mirror's dual write would cost more)
```

The exclusion works with `--columns on` only (`mirrored` makes the phase-4 columns).

### 5-6 `fascon regions` (`bench/tsoa_bench.fsc.in`, 4,194,304 tasks)

After the change:

```text
  arena task        Task[4194304]  slot = 24 B × (2^22 + 1 sentinel) + live/gen/link 25690112 B → 126353664 B (True SoA: vruntime in columns, sized below)
  reductions that read columns (they scan a column of just the values they read, never the slots): 4 (the columns add 34,078,736 B to the tables)
    task.ts_vruntime             True-SoA (single-write)    value column (u64)         8 B/slot       33,554,440 B  (line 41, 49, 70)
    task.fb0                     Mirrored-SoA (dual-write)  filter bit column          1 bit/slot        524,296 B  (line 49, 59, 70)
  places that rebuild column mirrors (writes by Id, ends of &mut borrows, calls given &mut): 0
  bulk-overwrite loops over True-SoA columns:
    task.vruntime line 101: non-temporal stores (F20_NT_STORE) — the column of 33,554,432 B ≥ 8,388,608 B (F20_SFENCE at the loop exit)
```

Before (phase 4): a 32 B slot, four mirrored columns (`agg0_vruntime`, `agg1_vruntime`, `fb2`, `min3_vruntime`, 101,187,616 B together) and 3 places that rebuild mirrors.

Fields kept off True SoA are listed with their reason (from `tests/ok/tsoa_fallback.fsc`):

```text
  fields kept off True SoA (they stay in the slot; any column they have is Mirrored-SoA (dual-write)):
    wire.x: the element `Wire` is #[wire] (a byte layout)
    ffi.v: the element `Ffi` or an Id of @ffi crosses the extern "C" boundary of `ffi_weight`
    esc.v: a reference to the element `&@a[id]` (line 99)
    addr.v: its address is taken (`&@a[id].v`, line 100)
```

### 5-7 Measurements

`bench/tsoa_bench.sh` (with `bench/tsoa_bench.fsc.in` and `bench/tsoa_harness.c`) compares three things: hand-written C (AoS: an array of the same 32-byte struct, read and written by plain loops, with no overflow checks and whatever `-O2` auto-vectorizes), Fascon before the change (the phase-4 mirrored columns) and Fascon after it (the defaults, `--columns on --nontemporal auto`). Machine: Intel Xeon (KVM, 2 vCPUs, 2 MiB L2 per core, 260 MiB L3), GCC 13.3 and clang 18.1 at `-O2 -std=c11`, baseline (SSE2) and `-march=native` (AVX-512), 2 MiB pages (`madvise`) for both the Fascon table and the C array. The 16 binaries (2 sizes × 4 builds × before / after) were run alternately 9 times; every number is the median of the 9 rounds (each round is the harness's median: 20 × 15 timings for 60,000 tasks, 3 × 9 for 4 million). Cycles use each round's own clock, measured at its start with a chain of dependent IMULs (2.87–3.27 GHz, median 3.06 GHz). On this shared host some rounds are about 1.6× slower for everything, C included.

Cycles per task. Each cell is "C (AoS) / before / after"; the last column is the range of after ÷ C over the four builds.

**60,000 tasks (fits in cache)**

| Operation | GCC baseline | GCC native | clang baseline | clang native | after ÷ C |
| --- | --- | --- | --- | --- | --- |
| read: sum of vruntime (vruntime_scan) | 1.62 / 0.41 / 0.40 | 1.56 / 0.18 / 0.18 | 1.54 / 0.35 / 0.34 | 1.60 / 0.23 / 0.24 | 0.11–0.25 |
| read: vruntime sum of runnable tasks | 3.04 / 0.41 / 0.69 | 3.09 / 0.18 / 0.45 | 1.99 / 0.35 / 0.67 | 1.81 / 0.24 / 0.40 | 0.15–0.34 |
| read: count of runnable tasks | 1.93 / 0.08 / 0.08 | 1.97 / 0.02 / 0.02 | 1.60 / 0.04 / 0.04 | 1.55 / 0.01 / 0.01 | 0.01–0.04 |
| read: pick_next | 2.08 / 0.76 / 0.75 | 2.52 / 0.36 / 0.58 | 1.97 / 0.71 / 0.52 | 1.81 / 0.28 / 0.62 | 0.23–0.36 |
| write: vruntime += 1, runnable tasks | 2.89 / 5.48 / 2.98 | 3.09 / 5.68 / 2.99 | 1.86 / 5.53 / 2.73 | 1.52 / 5.51 / 2.89 | 0.97–1.90 |
| write: vruntime += 1, every task | 2.47 / 8.11 / 2.10 | 2.38 / 8.35 / 2.05 | 2.44 / 8.12 / 2.04 | 2.35 / 8.20 / 1.85 | 0.79–0.86 |
| write: vruntime = x, every task | 2.61 / 8.48 / 1.14 | 2.60 / 8.44 / 1.17 | 2.55 / 7.85 / 1.98 | 2.56 / 7.92 / 1.22 | 0.44–0.78 |

**4,194,304 tasks (a 126 MB slot table + 34 MB of columns; the C array is 128 MB)**

| Operation | GCC baseline | GCC native | clang baseline | clang native | after ÷ C |
| --- | --- | --- | --- | --- | --- |
| read: sum of vruntime (vruntime_scan) | 8.15 / 0.89 / 0.91 | 8.25 / 0.91 / 0.87 | 8.05 / 0.85 / 0.83 | 5.64 / 0.90 / 0.85 | 0.10–0.15 |
| read: vruntime sum of runnable tasks | 9.91 / 0.90 / 0.97 | 10.06 / 0.89 / 0.85 | 8.63 / 0.84 / 0.99 | 7.67 / 0.88 / 0.86 | 0.08–0.11 |
| read: count of runnable tasks | 10.49 / 0.10 / 0.10 | 10.45 / 0.02 / 0.02 | 8.58 / 0.04 / 0.04 | 5.83 / 0.01 / 0.01 | 0.00–0.01 |
| read: pick_next | 10.10 / 1.08 / 1.03 | 10.27 / 0.87 / 0.86 | 8.95 / 1.03 / 0.93 | 8.28 / 0.88 / 0.84 | 0.08–0.10 |
| write: vruntime += 1, runnable tasks | 9.94 / 16.61 / 11.00 | 9.96 / 17.04 / 11.01 | 8.93 / 15.57 / 10.92 | 8.53 / 15.39 / 10.81 | 1.11–1.27 |
| write: vruntime += 1, every task | 8.79 / 20.98 / 2.12 | 9.39 / 19.68 / 2.16 | 9.00 / 19.12 / 1.74 | 9.06 / 18.74 / 1.61 | 0.18–0.24 |
| write: vruntime = x, every task | 13.30 / 19.60 / 1.36 | 12.96 / 19.10 / 1.42 | 13.33 / 18.99 / 1.38 | 13.61 / 19.70 / 1.38 | 0.10–0.11 |

- **Reads stay under one cycle per task with 60,000 tasks in every build (0.01–0.75).** With 4 million tasks they take 0.01–1.03 cycles: the u64 column streams at the memory bandwidth limit (8 B per 0.27–0.30 ns here, about 27–30 GB/s). The same reads in C (AoS) read the 32-byte structs and take 5.6–10.5 cycles.
- **Some reads got slower than before.** The filtered sum (0.18 → 0.45 cycles with GCC native) and pick_next (0.28 → 0.62 with clang native) now apply the filter as a mask at scan time. All stay under one cycle; the plain sum and the count are unchanged.
- **Bulk updates (`+= 1` in every task) and bulk overwrites (`= x`) are now faster than C (AoS) at both sizes in every build** (0.44–0.86× with 60,000 tasks, 0.10–0.24× with 4 million). Before the change they took 3.1–3.5× (60,000) and 1.4–2.3× (4 million) the C time. The overwrite with 4 million tasks uses non-temporal stores (as 5-4 says, plain stores are 14 % faster at this size on this host, but it is still 0.10× C).
- **The conditional write (`+= 1` in runnable tasks only) is on par with C under GCC (0.97–1.03× with 60,000 tasks, 1.11× with 4 million) and still slower than C under clang** (1.47–1.90× with 60,000, 1.22–1.27× with 4 million), down from 1.9–3.6× (60,000) and 1.7–1.8× (4 million) before. clang unrolls the C loop 2–6 times, and its C runs in 0.5–0.6× the GCC time; on the Fascon side the general loop walks the live bitmap (ctz + blsr) and executes more instructions per task than the C loop, which is the remaining gap (5-9).

### 5-8 How it was checked

- Every existing test passes (`python3 tests/run_tests.py --qemu`: 338 checks, 0 failures). `tests/cols/cols.fsc` prints `cols.expected` with `--columns on`, `mirrored` and `off`; the checks of the mirrored-column C run on the `mirrored` output (its `task` table is not eligible, so it is mirrored under `on` too) and the True-SoA checks on the `on` output. The 113 checks of `tests/latch/harness.c` (stale writes go to the sentinel, live slots stay untouched, ...) pass in all three modes; only its slot-alignment check was split, so that a True-SoA table checks "slot = the part left in it, naturally aligned".
- `tests/ok/tsoa_life.fsc`: alloc initial values, plain and compound writes by Id, writes through get_mut bindings, whole-element writes and `clone()`, free, `remove` (the moved-out value carries the column values), sum, filtered sum, count, u64 min, i32 max and i32 `<=` picks, `#[on_drop]` (on free, on the old value of a whole-element write, and on a value whose alloc failed in a full table), and ABA (after the freed slot is reused with a new generation, a write and a read through the stale Id are caught as ESTALE and the new element stays intact).
- `tests/ok/tsoa_fallback.fsc`: `#[stable_abi]` (back to AoS), `#[wire]`, an Id crossing `extern "C"`, an escaping element reference, and an address-taken field (only that field stays mirrored; another field of the same table is True SoA).
- `tests/ok/tsoa_nt.fsc`: a bulk overwrite of an 8 MiB column (`auto`: non-temporal stores + `F20_SFENCE`), an overwrite whose next loop reads the same column (plain stores), a table shared with an interrupt handler (`auto`: plain stores; `on`: non-temporal stores + `F20_SFENCE`), and a shared arena (never True SoA).
- These three print the same in all nine combinations of `--columns on` / `mirrored` / `off` and `--nontemporal auto` / `on` / `off`, and pass the `--emit-strict-fascon` round trip (compiling the strict form again gives the same C). Their C compiles without warnings under `-Wall -Wextra -Werror` with GCC and clang.
- `tests/cols/colfuzz.py --tsoa`: 500 random programs without the uses that rule True SoA out (address-taken fields, place bindings, whole-element reads and writes, stale Ids and bulk overwrites remain; 300 on an intermediate version, the last 200 on the final code) were run with `on`, `on --nontemporal on`, `mirrored` and `off`; every output matched (57 of them had True-SoA tables). Their `on` C compiled without warnings under `-Wall -Wextra -Werror` with GCC and clang (except `-Wtype-limits` from comparisons against random constants, which the C from before the change has too). `run_tests.py` runs 8 of each by default.
- `tools/cross_check.py` (compile with `-Werror` and link for ARMv7, MIPS64, RISC-V64, AArch64 and x86_64) passes, including `tests/ok/tsoa_life.fsc`, `tests/ok/tsoa_fallback.fsc` and `bench/sched_bench.fsc`.

### 5-9 Not done, and limits

- **The conditional write under clang** (5-7). A general `for id in @a` walks the live bitmap with ctz. Turning it into a plain index loop for all-ones 64-slot words took clang native with 60,000 tasks from 1.90× to 1.23× C (hand-edited C, one measurement), but it slows sparse tables, so it is not in.
- **The True-SoA conditions are conservative.** Tables whose element references are passed to functions, that have a match binding an element, or whose element type or Ids cross `extern "C"` stay mirrored, as do integer fields narrower than 32 bits and usize / isize fields.
- **The non-temporal lower bound comes from one machine** (5-4). The last-level cache size is not known when the C is generated, so on machines with a large L3, columns between 8 MiB and the L3 size lose a little. `--nontemporal=off` turns it off.
- **No timings on AArch64, RISC-V or MIPS.** The generated C compiles and links with `-Werror` for those targets (`tools/cross_check.py`), no more.

## Phase 6 — masked column RMW and float columns

Phase 5 left one write slower than C: a conditional update such as `for id in @task { if @task[id].state == RUNNING { @task[id].vruntime += 1; } }`. The filter already lives in a bit column (`fb0`, kept in step with the slot) and the field is a True-SoA column, but the general loop still visited the live slots one by one. Phase 6 turns that shape into a straight scan of the bit column (docs/SPEC-1.0-HPC.md §2), and gives f32 / f64 the columns and kernels of the integers under `#[fast_math]` (§1).

### 6-1 The kernels

`f20_col_masked_add_{u32,u64,i32,i64,f32,f64}` and `f20_col_masked_set_*` in `runtime/fascon2_rt.h` are written once with GCC / clang vector extensions (`F20_VECTOR_BYTES` = 16 / 32 / 64 bytes by target), so SSE2, AVX2, AVX-512 and NEON all get the same code:

```c
for (uint32_t base = 0u; base < n; base += 64u) {
    const uint64_t w = m[base >> 6];
    if (w == 0u) continue;                                /* no load, no store of those 64 slots */
    T *const q = c + base;
    UVT of = {0};
    if (w == ~UINT64_C(0)) {                              /* every slot selected */
        if (nt) { /* check every lane for overflow first, then stream with F20_NT_STORE */ }
        for (j = 0; j < 64; j += L) { v = load(q + j); r = v + xv; of |= OVF(v, xv, r); store(q + j, r); }
    } else {                                              /* mixed: predicated, in place, no NT stores */
        for (j = 0; j < 64; j += L) { v = load(q + j); xm = xv & MASK(w, j, sel); r = v + xm; of |= OVF(v, xm, r); store(q + j, r); }
    }
    if (FASCON_UNLIKELY(top_bit_set(of))) { /* subtract xm again: the word is back as it was */ return base; }
}
return F20_RMW_NONE;
```

- `MASK(w, j, sel)` broadcasts `(uint32_t)(w >> j)`, ANDs it with `1 << lane` and compares with zero. 64-bit lanes use the same 32-bit compare with both halves of a lane selecting the same bit, because SSE2 has no 64-bit compare; the first version used 64-bit compares and took 2.1–2.4 ns per task on SSE2 (60,000 tasks, one trial run), against 0.64–0.66 ns now (the medians below).
- Overflow is computed without compares (SSE2 has neither unsigned nor 64-bit ones): the carry `(v & x) | ((v | x) & ~r)` for unsigned lanes and `(v ^ r) & (x ^ r)` for signed ones, both in the top bit. Masked-out lanes add zero and never overflow.
- The store is speculative: on an overflowing word the kernel subtracts the same `xm` again (modular arithmetic gives the originals back exactly) and returns the word's first index; the generated C continues there with the sequential loop, which panics at the same slot with the same output as `--columns off`. The first version checked each word in a second pass before storing; single pass with undo is what the numbers below measure.
- Float adds and sets select with a bit blend, so unselected lanes keep their exact bits (`-0.0`, NaN payloads).

### 6-2 Measurements

Same host type as phase 5 (Intel Xeon @ 2.10 GHz, KVM, 2 vCPUs, 2 MiB L2 per core, 260 MiB L3), GCC 13.3 and clang 18.1 at `-O2`, SSE2 and `-march=native` (AVX-512), 9 alternating rounds, medians, cycles per task at each round's own clock (2.66–3.85 GHz). Full tables: [bench/RESULTS.md](../bench/RESULTS.md).

**The conditional write `vruntime += 1` on runnable tasks** (about a quarter of the tasks, so nearly every word is mixed). Each cell is "C (AoS) / phase 1 / now":

| Tasks | GCC baseline | GCC native | clang baseline | clang native | now ÷ C |
| --- | --- | --- | --- | --- | --- |
| 60,000 | 2.72 / 3.02 / 2.22 | 2.97 / 2.90 / 0.70 | 2.10 / 2.84 / 2.20 | 1.58 / 3.03 / 0.68 | 0.24–1.05 |
| 4,194,304 | 3.98 / 4.02 / 2.27 | 3.98 / 3.70 / 0.95 | 3.89 / 3.81 / 2.16 | 3.85 / 3.92 / 0.96 | 0.24–0.57 |

**Floats (`bench/fsimd_bench.fsc.in`, a 32-byte particle)**, cycles per particle, "C at -O2 / C at -ffast-math / Fascon" for clang native (the other builds are in bench/RESULTS.md):

| Operation | 60,000 particles | 4,194,304 particles |
| --- | --- | --- |
| read: sum of mass (f64) | 2.09 / 1.62 / 0.20 | 4.01 / 3.77 / 0.89 |
| read: sum of x (f32) | 2.08 / 1.41 / 0.11 | 3.91 / 3.69 / 0.44 |
| read: energy sum of HOT (f64, filtered) | 1.53 / 1.86 / 0.35 | 3.77 / 3.85 / 0.89 |
| write: energy += 0.5, HOT (f64 masked add) | 1.59 / 1.54 / 0.53 | 4.00 / 4.04 / 0.98 |
| write: x += 1.0, HOT (f32 masked add) | 1.58 / 1.55 / 0.26 | 4.32 / 3.96 / 0.51 |
| write: energy = e, COLD (f64 masked set) | 1.55 / 1.57 / 0.51 | 4.16 / 4.22 / 0.98 |
| write: mass = m, every particle (f64) | 2.63 / 2.58 / 2.03 | 4.94 / 6.81 / 1.32 |

- **The conditional write is now 0.24–1.05× C** (60,000 tasks) **and 0.24–0.57× C** (4 million), against 0.97–1.91× and 0.93–1.02× for phase 1 in the same run. With AVX-512 a mixed word costs 8 vector operations of 8 lanes; with SSE2, 32 of 2 lanes, which is why clang baseline stays at C (1.05×).
- **Every float operation is faster than both C baselines in every build** (reads 0.04–0.55× plain C, masked writes 0.02–0.82×, bulk overwrites 0.15–0.78×). Plain `-O2` C keeps float sums in written order and cannot vectorize them; `-ffast-math` C may, but the AoS loop still loads 4 or 8 bytes out of every 32.
- The other rows of the task benchmark (reads, unconditional writes) did not change beyond the noise of this shared host; `tools/compare_c.py` shows their C is the same.

### 6-3 How it was checked

- `tests/cols/rmw_kernels.c`: every kernel against a scalar model on random columns with zero, all-one and mixed words, tails, `-0.0` and NaN values and overflowing words, at -O2 (GCC, clang, SSE2 / AVX2 / AVX-512) and under ASan / UBSan.
- `tests/ok/tsoa_rmw.fsc` (u32 / u64 / i32 / i64 / f32 / f64, add and set) and `tests/ok/tsoa_float*.fsc` print the same in all nine `--columns` × `--nontemporal` combinations; `tests/cols/rmw_overflow.fsc` panics at the same slot with the same output with `--columns on` and `off`.
- `tests/cols/colfuzz.py --rmw` and `--float`: random programs with masked `+=` / `=` on six column types mixed with allocations and frees, and float columns with NaN, `-0.0` and infinities, run with `on`, `on --nontemporal on`, `mirrored` and `off` (32 of each in the final run, all equal).
- `tools/compare_c.py`: the C of every existing program in all three column modes is unchanged.

### 6-4 Not done, and limits

- **Mixed words on SSE2 cost 32 vector steps per 64 slots** for 64-bit columns. A filter that is sparse but scattered (a few set bits in most words) would run faster with a per-bit loop; it is not used, so that the cost per word stays fixed and no branch depends on the data.
- **Only loop-invariant values** (`x` a literal, a local, a static or a constant) are taken; `@a[id].f += @a[id].g` keeps the general loop.
- **No AArch64 / RISC-V timings**; the kernels compile there (vector extensions), but were measured on x86-64 only.

## Measurements

All rows are in [bench/RESULTS.md](../bench/RESULTS.md). Intel Xeon @ 2.10 GHz (KVM, 2 vCPUs, Linux 6.18, THP in `madvise` mode), GCC 13.3 and clang 18.1 at `-O2`, 60,000 tasks, ns per task. Two sessions of 9 alternating runs; every number is the median of those 18 runs (each run is itself the median of 15 timings). "4 KiB" is the new code with `FASCON_NO_HUGEPAGE=1`. "2 MiB" is the new code with `madvise(MADV_HUGEPAGE)`; `/proc/<pid>/smaps` showed `AnonHugePages: 2048 kB` on the table. 1.0.0 always ran on 4 KiB pages.

| Traversal | 1.0.0 gcc | now gcc 4 KiB | now gcc 2 MiB | 1.0.0 clang | now clang 4 KiB | now clang 2 MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| plain sum (`for id in @task`, sum specialization) | 0.74 | 0.72 | 0.63 | 0.71 | 0.69 | 0.57 |
| number of runnable tasks (filtered count) | 1.10 | 0.79 | 0.71 | 1.10 | 0.80 | 0.70 |
| vruntime of runnable tasks (filtered sum) | 1.08 | 1.09 | 1.04 | 1.12 | 0.74 | 0.66 |
| pick_next (runnable, least vruntime) | 1.31 | 0.82 | 0.73 | 1.23 | 0.73 | 0.69 |
| general loop (conditional writes) | 1.12 | 1.19 | 1.15 | 1.12 | 1.12 | 1.10 |
| sparse table (9 of 10 blocks of 64 slots freed): count | 0.49 | 0.07 | 0.07 | 0.48 | 0.07 | 0.07 |
| sparse table: pick_next | 0.48 | 0.07 | 0.07 | 0.51 | 0.07 | 0.07 |
| Id chain with `get`, scattered order | 17.43 | 16.95 | 14.23 | 22.91 | 23.21 | 15.93 |
| Id chain with `@task[id]`, scattered order | 18.23 | 18.04 | 13.67 | 18.78 | 18.48 | 14.43 |

- **The code alone (4 KiB columns):** filtered counts are 27–28 % faster and pick_next 37 % (GCC) / 41 % (clang). The filtered sum is 34 % faster with clang and unchanged with GCC. Sparse tables are 7× faster because all-zero words are skipped.
- **Huge pages on top (2 MiB against 4 KiB):** the plain sum is 13 % (GCC) / 17 % (clang) faster, the filtered scans 5–13 %, and scattered Id chains 16–31 %.
- **The plain sum** went from 0.74 to 0.63 ns (GCC) and from 0.71 to 0.57 ns (clang): 1.32 / 1.20 cycles per task at 2.10 GHz, inside the target (0.48–0.70 ns). Phases 1–3 did not reach under one cycle (0.476 ns at this clock): the scan had no branch left and read 32 bytes per task (50–56 GB/s, 24–27 bytes per cycle), so bandwidth was what remained. Phase 4 (below) removes those bytes.
- **The general loop** (conditional writes, every slot live) is unchanged with clang and 3–6 % slower with GCC. In a dense table the per-slot `link[]` test of 1.0.0 was always predicted, while the bitmap adds ctz + blsr per slot; the bitmap pays off in sparse tables. An `if` written in the body is the program's own logic and stays a branch (writing every slot unconditionally would add line traffic between cores).
- **Huge pages vary by system.** In an earlier session the same day, the 4 KiB and 2 MiB columns came out equal; most likely no huge page backed the table then.
- `FASCON2_PICK_BRANCHLESS` makes pick_next 1.5 ns (clang) / 2.6 ns (GCC) (see 1-2).
- Creation-order chains are the same as 1.0.0 within noise.
- AArch64 was not timed on hardware (QEMU timings are meaningless); the instruction counts are in the listings above.

### Phase 4 (columns)

The same harness in a later session on a different host: Intel Xeon (KVM, 2 vCPUs), GCC 13.3 and clang 18.1 at `-O2`, 2 MiB pages, 60,000 tasks. The core clock, measured in the same session with a chain of dependent 64-bit multiplies, was 3.85 GHz (3.80–3.94), so one cycle is about 0.26 ns. Nine alternating rounds of eight builds: `--columns off` / `on` × GCC / clang × baseline (SSE2) / `-march=native` (AVX-512 on this host). Every number is the median of the nine rounds, in ns per task. "off" is everything up to phase 3.

| Traversal | off GCC | on GCC | off clang | on clang | off GCC native | on GCC native | off clang native | on clang native |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| plain sum (vruntime + prio) | 0.54 | 0.21 | 0.45 | 0.19 | 0.51 | 0.10 | 0.45 | 0.11 |
| number of runnable tasks (filtered count) | 0.56 | 0.02 | 0.56 | 0.01 | 0.60 | 0.00 | 0.28 | 0.00 |
| vruntime of runnable tasks (filtered sum) | 0.79 | 0.10 | 0.52 | 0.09 | 0.82 | 0.05 | 0.48 | 0.06 |
| pick_next (runnable, least vruntime) | 0.60 | 0.21 | 0.63 | 0.18 | 0.61 | 0.10 | 0.49 | 0.07 |
| sparse table: sum | 0.56 | 0.03 | 0.45 | 0.02 | 0.51 | 0.02 | 0.43 | 0.02 |
| sparse table: count | 0.05 | 0.02 | 0.05 | 0.01 | 0.06 | 0.00 | 0.03 | 0.00 |
| sparse table: pick_next | 0.06 | 0.03 | 0.07 | 0.02 | 0.06 | 0.02 | 0.05 | 0.01 |
| general loop writing `files` (no column reads it) | 0.87 | 0.89 | 0.84 | 0.77 | 0.94 | 0.90 | 0.76 | 0.75 |
| general loop writing `vruntime` (three columns read it) | 0.94 | 2.13 | 0.81 | 1.79 | 0.93 | 1.78 | 0.78 | 1.82 |

- **Under one cycle per 32-byte task, in every build.** With columns, at 3.85 GHz: the plain sum 0.73–0.81 cycles (baseline) / 0.39–0.42 (native), the filtered sum 0.35–0.39 / 0.19–0.23, pick_next 0.69–0.81 / 0.27–0.39, the filtered count under 0.1. Without columns the same scans take 1.1–3.2 cycles (0.28–0.82 ns) including the filtered count, the basis used in [bench/RESULTS.md](../bench/RESULTS.md); the plain sum, the filtered sum and pick_next alone take 1.7–3.2 cycles (0.45–0.82 ns). The plain sum reads 12 bytes per task (vruntime 8 + prio 4) instead of 32.
- **Writes:** a loop writing a field no column reads is unchanged; a loop writing `vruntime` in every runnable task takes about twice as long (see 4-4).
- **Tables larger than the caches.** At 60,000 tasks the columns one scan reads (at most 0.8 MB) stay in the 2 MiB L2. With 4,194,304 tasks (128 MB of slots, `-march=native`, GCC, median of 9) the 1.0.0-style scans take 1.9–2.9 ns per task and the column scans 0.29 ns (sum, filtered sum, pick_next) and 0.004 ns (count): about 7× faster, but a u64 column then streams at memory bandwidth (about 28 GB/s on this host), which is 1.1 cycles per task. Fewer bytes per task (narrower values) would be the only way further down.
- **AArch64** was not timed on hardware. From the compiled loops (GCC, `-mcpu=cortex-a72`): the u64 sum is one SIMD operation per slot (ADD + ORR), the u32 sum 0.75 (AND, USRA, ADD), the pick one (CMGT + BSL) and the count a few instructions per 64 slots (CNT + ADDV); that is about 0.5 / 0.38 / 0.5 / 0.03 cycles per slot on a core with two 128-bit SIMD pipes, and half of that with four.
- Rows that do not involve columns (Id chains, the plain C pointer lists) are in `bench/RESULTS.md`; in this session they moved by up to ±50 % between rounds on the shared host, in both builds alike.

## Not done, and limits

- **Mirrored columns (Mirrored-SoA) trade write time and memory for scan time.** A write that reaches a mirror costs a few stores and, in loops that write such a field across the table, extra cache lines. For fields that can be True SoA this is gone (phase 5); the conditions and the remaining limits are in 5-1 and 5-9. Filters that read a local variable and 64-bit `<=` / `>=` picks keep the phase-1 scans. `--columns off` / `mirrored` apply to a whole program; there is no per-arena switch yet.
- **Beyond the caches the columns run at memory bandwidth** (about 0.85–1.0 cycles per task for a u64 column with 4 million tasks; 5-7). Narrower values per task would be the next step.
- **Multi-core.** The layout and the unit (512 slots) that avoid false sharing are defined, and multi-core programs are covered by [SPEC-1.0-SMP.ja.md](SPEC-1.0-SMP.ja.md) and [SPEC-1.0-INTEGRATION.md](SPEC-1.0-INTEGRATION.md). The measurements in this note run on one core (`bench/harness.c` starts no threads); false sharing between cores was not measured here.
- **Where a latched fault stops.** After a stale read inside a fixed-trip loop the program stops at the loop exit (every external effect before that is still stopped, so memory stays intact and nothing outside sees the fault). In long loops it takes longer to stop than in 1.0.0.
- **The numbers come from one virtual machine** and drift with the time of day; whether Linux grants a huge page can also differ between sessions.

## Where the source changed

| Place | Change |
| --- | --- |
| `src/codegen.rs` | `loop_stmt` (bitmap + ctz, exit commit points, the latch folded into while / loop), `body_changes_liveness`, `reduction_plan` and the masked reductions, `gen_arena` (placement, `live[]`, the arena table), `emit_struct` (alignment), `stmt` / `emit_scope_drops` / `own_expr` (frame region) |
| `src/parser.rs` · `src/sema.rs` · `src/types.rs` · `src/ast.rs` | `#[repr(align(N))]`, F2031, F2032, `StructInfo::align`, `type_align_attr` |
| `src/flow.rs` | `allocs` in arena effects (transitive closure) |
| `src/driver.rs` · `src/dict.rs` · `src/qemu.rs` | layout arithmetic, new lines in `fascon regions`, dictionary entries, BSP build defaults |
| `runtime/fascon2_rt.h` | `live[]`, bit scans, `F20_SEL64` / `F20_PICK`, the new table layout, `F20_ARENA_STORAGE`, the arena table, the frame region |
| `bsp/qemu-virt-aarch64/` | 1 GiB / 2 MiB + contiguous, `.fascon_arena` and `.frame`, boot messages |
| `bsp/host/fascon2_plat_host.c` | `madvise(MADV_HUGEPAGE)` (`FASCON_NO_HUGEPAGE=1` turns it off) |
| `vscode-extension/` | dictionary entries (live bitmap, `#[repr(align(N))]`, frame region, commit point, F2031, F2032), snippets `repralign` / `forpick` / `forcount`, highlighting of `#[repr(align(N))]` |
| `tests/` | `ok/frame.fsc`, `ok/align.fsc`, `err/f2031`, `err/f2032`, 10 more checks in the latch harness (including 30,000 random steps against plain C), two more QEMU boots; 38 in total |
| `bench/` | rows for filtered scans and sparse tables; the harness asks for huge pages like `fascon run` (`FASCON_NO_HUGEPAGE=1` for the 4 KiB columns) |
| `src/codegen.rs` (phase 4) | `collect_columns` / `plan_columns` / `add_col` (columns from every reduction, shared by kind, value and filter), `gen_arena` (the column struct, `F20_DEFINE_TABLE_COLS`, `_mirror` / `_mirror_p` / `_unmirror`, column-aware alloc / free / remove), `gen_mirror_masks` (partial rebuilds), `emit_col_reduction`, `arena_ptr` / `assign` / `call` (keep the element pointer of a write and rebuild what it reaches), `lower_match` / `emit_scope_drops` / `ref_writes` (get_mut borrows and match / if-let bindings of places inside the slot), `arena_write_root` / `place_root_path` / `col_mask` (which columns a write reaches) |
| `runtime/fascon2_rt.h` (phase 4) | `F20_COL_*`, `F20_KEY_*`, `F20_BIT_PUT`, native-width vector types, `F20_KEEP`, `f20_col_sum_u64` / `_exact`, `f20_col_sum_u32`, `f20_col_count_bits`, `f20_col_pick_key`, `f20_col_key_fresh`, `F20_DEFINE_TABLE_COLS` |
| `src/main.rs` · `src/driver.rs` · `src/qemu.rs` · `src/dict.rs` (phase 4) | `--columns on\|off` and `FASCON_COLUMNS`, `--cpu native\|baseline` (`fascon run` adds `-march=native` / `-mcpu=native` and retries without it), the column lines of `fascon regions`, the dictionary entry "列" (columns) |
| `vscode-extension/` (phase 4) | settings `fascon.columns` and `fascon.cpu`, the dictionary entry |
| `tests/` (phase 4) | `cols/cols.fsc` (+ `.expected`, also on QEMU), `cols/colfuzz.py` (`--fuzz N`), four F2502 cases for the borrow rule the mirrors rely on; 52 in total (with `ok/match_place.fsc` and an F0507 case from the fix of bindings of places) |
| `bench/` (phase 4) | `charge_running` (a loop writing a field the columns read) |
| `src/codegen_tsoa.rs` (phase 5, new) | `plan_tsoa` (the True-SoA decision: table, element, extern "C", #[on_drop], escaping references, addresses, filters, 1.0.0 scans, write weights), `ts_lv` / `arena_ix` (column lvalues and the proven / latched / sentinel / stop-on-the-spot indices), `gen_tsoa_part` / `gen_tsoa_fns` / `gen_tsoa_cols` (the part left in the slot, `_ix` / `_ix_r` / `_ix_p` / `_ix_strict`, `_load` / `_put` / `_ts_zero`, the columns), `ts_red_body` (masked scans), `nt_fields` / `nt_decide` / `nt_scan_block` (bulk overwrites and the non-temporal decision) |
| `src/codegen.rs` (phase 5) | `CgOpts::true_soa` / `nontemporal`, `ColUse::SumT` / `PickT`, `plan_columns` (True-SoA terms, the write-heavy exclusion), `gen_arena` (the slot part type, natural alignment, alloc / free / remove through `_put` / `_load`, dropping a value whose alloc failed), `lv_raw` (True-SoA fields and whole-element reads), `lower_match` (the type of lookup bindings), `assign` (whole-element writes, non-temporal stores), `arena_ptr` (the element pointer type for mirrors, the loop's slot index), `loop_stmt` (`F20_SFENCE`), `generate` (`F20_PREFETCH_DIST` for `--nontemporal=on`) |
| `runtime/fascon2_rt.h` (phase 5) | `f20_col_sum_u64_m` / `_exact`, `f20_col_sum_u32_m`, `f20_col_pick64_m`, `f20_col_pick32_m`, `F20_LANE_BITS` / `F20_LANE_MASK`, the mask tables for x86 SSE2 / SSE4.2, `F20_NT_STORE_*`, `F20_SFENCE`, `F20_PREFETCH_R`, `F20_PF_BLOCK` (only with `F20_PREFETCH_DIST`) |
| `src/main.rs` · `src/driver.rs` (phase 5) | `--columns on\|mirrored\|off` (`FASCON_COLUMNS=mirrored`), `--nontemporal auto\|on\|off` (`FASCON_NONTEMPORAL`), in `fascon regions`: True-SoA (single-write) / Mirrored-SoA (dual-write), why fields were kept off True SoA, bulk-overwrite decisions, the slot size of True-SoA tables |
| `tests/` (phase 5) | `ok/tsoa_life.fsc`, `ok/tsoa_fallback.fsc`, `ok/tsoa_nt.fsc` (with `.expected`), `cols/colfuzz.py --tsoa`, `run_tests.py` (3 `--columns` × 3 `--nontemporal` settings, the shape of the C, `fascon regions`, the latch harness in three modes), `latch/harness.c` (the slot-alignment check for True-SoA tables) |
| `bench/` (phase 5) | `tsoa_bench.fsc.in`, `tsoa_harness.c` (against hand-written C AoS), `tsoa_bench.sh`, `tsoa_summary.py`; `harness.c` (reads the reference values of a True-SoA table through `ARENA_task_TS_vruntime`) |
| `src/lexer.rs` · `src/parser.rs` · `src/sema.rs` · `src/types.rs` · `src/strict.rs` (phase 6) | `f32` / `f64` (literals, inference, casts, `to_bits` / `from_bits` / `is_nan` / `is_finite` / `abs`, constants), `#[fast_math]` on loops and functions, `--fast-math` |
| `src/codegen.rs` · `src/codegen_tsoa.rs` (phase 6) | float True-SoA columns and `ColKind::FKey` (IEEE-754 order keys), `ts_fsum_body`, `rmw_plan` / `plan_rmw` / `emit_rmw` (masked column RMW, the overflow fallback into the sequential loop), `CgReport::rmw_loops` for `fascon regions` |
| `runtime/fascon2_rt.h` (phase 6) | `FP_CONTRACT` off, the float helpers and the shortest round-trip printer, `F20_KEY_F32` / `F20_KEY_F64`, `f20_col_sum_f32` / `_f64` (`_m`), `F20_NT_STORE_F32` / `_F64`, `f20_col_masked_add_*` / `f20_col_masked_set_*` |
| `tests/` · `bench/` (phase 6) | `ok/float_basic.fsc`, `ok/tsoa_float*.fsc`, `ok/tsoa_rmw.fsc`, `cols/rmw_kernels.c`, `cols/rmw_overflow.fsc`, `float/fmtfuzz.c` + `fmtcheck.py`, `cols/colfuzz.py --float` / `--rmw`; `fsimd_bench.fsc.in`, `fsimd_harness.c`, `fsimd_c.c` / `.h`, `fsimd_bench.sh`, `tsoa_summary.py --float` |
