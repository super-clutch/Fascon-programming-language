# Fascon 1.0 — float True-SoA columns, masked column RMW, GPU offload and the HPC profile

The version stays **Fascon 1.0 (1.0.0)**. This document describes four additions for HPC nodes, supercomputer compute nodes and multi-channel-memory workstations, and how each keeps the invariants of Fascon 1.0. The Japanese version is `SPEC-1.0-HPC.ja.md`.

| Addition | In the source | Section |
| --- | --- | --- |
| f32 / f64 and float True-SoA columns | `let x: f32 = 1.5;`, `#[fast_math]`, `--fast-math` | §1 |
| Masked column RMW | `for id in @a { if cond { @a[id].f += x; } }` | §2 |
| GPU / accelerator offload | `gpu_arena @g: T[N];`, `GpuBuf<@g>`, `GpuFence<@g>` | §3 |
| HPC profile | `--target hpc`, `--topology`, `#[numa_node(n)]`, `#[no_smt]`, `#[allow(hpc_io)]` | §4 |

## 0. Invariants (quality gates)

| Gate | How it is checked |
| --- | --- |
| Existing programs compile to the same C, byte for byte (comments and whitespace aside) | `python3 tools/compare_c.py`: every program of `examples/` and `tests/` in the default, `--columns mirrored` and `--columns off` modes against `tests/golden_c.json` (recorded from the build before these additions) |
| No lifetimes, generics, raw pointers, `unsafe` or 0.1-only syntax; new keywords are contextual | `gpu_arena`, `GpuBuf`, `GpuFence`, `f32`, `f64` are contextual (a struct or a function of the same name still works); attributes are ordinary `#[..]` |
| Zero crates | `Cargo.toml` has no dependencies |
| New codes in the dictionary, the rules and the fixes | F2705-F2707 and F3901-F3903 in `data/dictionary.json` (ja / en), `data/rules.json` and `data/fixes.json`; `python3 tools/sync_dictionary.py && python3 tools/check_dictionary.py` |
| The generated C calls platform hooks only | no CUDA, Vulkan, libnuma, pthread or inline assembly in the generated C: offload and pinning go through `fascon_plat_*` (§3.3, §4.3); `tests/run_tests.py` greps the generated C |
| Package safety labels | `src/pkg.rs` `scan_text` labels `gpu_arena`, `GpuBuf`, `GpuFence` and `.dispatch(..)` as "GPU / Accelerator offload" |
| Strict round trip | `--emit-strict-fascon` keeps `gpu_arena`, `#[fast_math]`, `#[numa_node(n)]`, `#[no_smt]`, `#[allow(hpc_io)]`, `// fascon-target: hpc` and `// fascon-topology:`; compiling the strict file gives the same C |
| Opt-outs | `--columns off` (FASCON_COLUMNS=off), `--gpu=off` (FASCON_GPU=off), `FASCON_HPC_NO_PIN=1` |

## 1. f32 / f64 and float True-SoA columns

### 1.1 Floats in the language

`f32` and `f64` are IEEE-754 single and double precision. Literals are `1.5`, `2.0f32`, `1e-3` and `3f64`; an unsuffixed literal takes its type from inference (f64 when nothing decides). Arithmetic is exactly IEEE-754 in written order: the generated C turns contraction into FMA off (`#pragma STDC FP_CONTRACT OFF`, and the GCC / clang equivalents), so `a * b + c` rounds twice on every compiler. `x as i32` saturates (NaN gives 0, as in Rust); `x as f64` widens exactly. `put(x)` prints the shortest decimal that reads back to the same bits, in the shape of Rust's `{:?}` (`1.0`, `0.1`, `1e-7`, `inf`, `NaN`), ties to even (checked against 400,000 random bit patterns).

Methods: `to_bits()`, `is_nan()`, `is_finite()`, `abs()`; `f32::from_bits(u)`, `f64::from_bits(u)`; constants `f64::INFINITY`, `f64::NEG_INFINITY`, `f64::NAN`, `f64::EPSILON`, `f64::MIN`, `f64::MAX` (and the f32 ones).

### 1.2 Columns and kernels (`#[fast_math]` only)

Float additions are not associative, so a float reduction keeps the phase-1 scan in written order unless the loop or its function is marked `#[fast_math]` (or the program is compiled with `--fast-math`). Then:

| Reduction (under `#[fast_math]`) | Column | Dead slots | Kernel |
| --- | --- | --- | --- |
| sum `s += @a[id].f`, filtered sum | True-SoA `float` (4 B) / `double` (8 B) | `0.0` | `f20_col_sum_f32` / `_f64` (four independent vector accumulators; `_m` with a filter bit column) |
| min / max `if @a[id].f < best { .. }` | key column (`int64_t`) | `F20_KEY_NONE` | `f20_col_pick_key` (the integer one) |

**IEEE-754 order-preserving keys** (`F20_KEY_F32` / `F20_KEY_F64`): a value with sign bit 0 flips only the sign bit, one with sign bit 1 flips every bit; the result orders like the floats as a signed integer. NaN becomes `F20_KEY_NONE` (never picked, like a dead slot) and `-0.0` is folded to `+0.0` first. When the accumulator starts as NaN the pick does nothing, as the written-order loop would.

Bulk overwrites of float True-SoA columns of 8 MiB or more use `F20_NT_STORE_F32` / `_F64` with `F20_SFENCE()` at the exit, like integer columns. `#[no_column]` and the other True-SoA exclusions apply unchanged.

## 2. Masked column RMW

`for id in @a { if cond { @a[id].f += x; } }` and `.. { @a[id].f = x; }` become a straight vector-width update when

- `f` is a single-step True-SoA field of type u32, u64, i32 or i64, or f32 / f64 under `#[fast_math]`;
- `cond` can live in a filter bit column (`fbN`, Mirrored-SoA);
- `x` does not change inside the loop and cannot fail (a literal, a negated literal, a local other than the loop variable, a static or a constant, possibly with `as`);
- the arena is a table arena nobody else shares (not shared, not touched by interrupts or tasks, not dma / gpu / gc / bump / task).

The kernels `f20_col_masked_add_{u32,u64,i32,i64,f32,f64}` and `f20_col_masked_set_*` read the filter bit column one 64-bit word (64 slots) at a time:

| Word | Action |
| --- | --- |
| `0` | skip: the column lines of those 64 slots are neither loaded nor stored |
| `~0` | an unmasked vector add / assign; with columns of 8 MiB or more and `--nontemporal auto|on`, `F20_NT_STORE` (checked for overflow first, then streamed) |
| anything else | the word is spread over the lanes (broadcast, AND with 1 << lane, compare with 0; 64-bit lanes are built from 32-bit lane compares because SSE2 has no 64-bit compare) and a predicated operation updates in place; no non-temporal stores, so the cache is not polluted |

Integer adds keep Fascon's checked arithmetic: every lane computes its carry (unsigned: `(v & x) | ((v | x) & ~r)`) or signed overflow (`(v ^ r) & (x ^ r)`) in the top bit. A word with an overflowing lane is restored exactly (the values written are wrapped modulo 2^n, so subtracting `x` gives the originals back) and the kernel returns its first index; the caller continues from that word with the sequential loop, which panics (`FASCON PANIC 0x000b add overflow`) at the same slot with the same output as `--columns off`. Float adds put `v + x` only into the chosen lanes; the other lanes keep their bits (`-0.0` and NaN payloads included). `fascon regions` lists each loop with its column, bit column, kernel and store kind.

## 3. `gpu_arena`: zero-copy offload to accelerators

### 3.1 Syntax and types

```fascon
struct Particle { x: f32, v: f32, steps: u32, tag: u32 }
gpu_arena @parts: Particle[64];            // elements are plain old data (F2705)

let h: GpuBuf<@parts> = @parts.alloc(Particle { x: 0.0, v: 0.5, steps: 0, tag: 1 }).unwrap();
@parts[h].v = 0.75;                        // the CPU may touch the slot while it holds h
let f: GpuFence<@parts> = @parts.dispatch(STEP, grid, block, h);
// h is moved: reading, writing or freeing it now is F0382
if let Some(back) = @parts.reclaim(f) {    // does not wait; None until the work is done
    put(@parts[back].x);
    @parts.free(back);
}
let w = @parts.reclaim_wait(f);            // waits; the Sleep effect
```

- `alloc(v) -> Option<GpuBuf<@g>>`, `free(h) -> bool` (consumes h), `dispatch(kernel: u32, grid: u64, block: u64, h) -> GpuFence<@g>` (consumes h), `reclaim(f)` / `reclaim_wait(f) -> Option<GpuBuf<@g>>`, `len()`, `capacity()`, `h.index()`.
- `GpuBuf` is move-only (like `Dma`); `GpuFence` is a copyable ticket `[number 32 | index 32]`. A ticket gives the handle back once; a second reclaim with the same ticket, or a stale one, gives None, so there is never more than one handle.
- The table lives in `.bss.fascon_gpu` on a 2 MiB boundary (4 KiB on PE / Mach-O), a single instance for every core, never in columns.
- `reclaim_wait` has the Sleep effect: inside `critical`, a parallel scan or code reachable from an interrupt handler it is F2603; inside `rcu_read` / `atomic` it is F3203.

### 3.2 Errors

| Code | When |
| --- | --- |
| F2705 | the element holds own, Id, Str, references, function pointers, bool or Option (integers, floats and arrays / structs of them only) |
| F2706 | a handle or ticket of another gpu_arena, an integer or a handle where a ticket is expected, `GpuBuf<@x>` for an arena that is not a gpu_arena, `for id in @g`, or an Id operation (get, contains, validate, ...) |
| F2707 (warning) | the program has a gpu_arena and is compiled with `--gpu=off` |
| F0382 | the handle is used after `dispatch` or `free` |

### 3.3 Platform API

The generated C calls only these hooks (through the runtime wrappers `f20_gpu_*`):

```c
int  fascon_plat_gpu_register(void *base, size_t bytes, uint32_t arena_id);   /* 0 = success; pin / map the slot array */
int  fascon_plat_gpu_dispatch(uint32_t arena_id, uint32_t slot_idx, uint32_t kernel_id,
                              uint64_t grid, uint64_t block, uint64_t *out_fence);  /* 0 = queued */
int  fascon_plat_gpu_poll(uint64_t fence);                                       /* nonzero when done */
void fascon_plat_gpu_wait(uint64_t fence);
```

The generated C also emits the descriptor table `fascon2_gpu_arenas[]` (`name`, `base` = slot 0, `bytes`, `stride`, `elem_size`): the element of slot i is at `base + i * stride`. A board package (`boards/<name>/`) or another backend replaces the hooks without touching the compiler; on bare metal without one (`FASCON2_BOARD_GPU` not defined) dispatches run in place and complete at once.

### 3.4 The PC emulation and kernels

`bsp/host/fascon2_plat_host.c` queues dispatches in a FIFO and one worker thread runs them in order. Kernels are C functions in `<name>.gpu.c` next to the source, which `fascon run` builds along:

```c
#include "fascon2_rt.h"
typedef struct { float x, v; uint32_t steps, tag; } Particle;   /* the C shape of the Fascon struct */
FASCON_GPU_KERNEL(1) {
    Particle *p = (Particle *)elem;
    p->x += p->v * (float)(grid * block);
}
```

`FASCON_GPU_KERNEL(id)` defines the function and registers it before `main` (the registry lives in the generated C: `fascon_gpu_kernel_register` / `fascon_gpu_kernel_lookup`). A fence is a number counting from 1; the worker stores the finished count with release and `poll` loads it with acquire, so the reclaiming CPU sees what the kernel wrote. At exit (normal or panic) the queued work is finished first. `tests/run_tests.py` runs `tests/ok/gpu_offload.fsc` under ASan + UBSan and TSan: no memory errors, no data races, the same output as `--gpu=off`.

### 3.5 `--gpu=off`

`--gpu=off` (FASCON_GPU=off) defines `FASCON2_GPU_OFF`: dispatch runs the registered kernel in place on the calling core (nothing when none is registered) and returns a completed ticket; no `fascon_plat_gpu_*` hook is called, and the program needs no thread library. The compiler warns once per gpu_arena (F2707).

## 4. `--target hpc`

### 4.1 The profile

`--target hpc` or `// fascon-target: hpc` (in the first 20 lines). Compared with the default profile:

- Ids are 64 bits `[generation 32 | index 32]` (`FASCON2_ID64`, as on generic-os); `fascon regions` describes them that way.
- Every table is on a 2 MiB boundary in `.bss.fascon_arena`, whatever its size, and the PC layer applies `MADV_HUGEPAGE` to all of them.
- `FASCON2_MAX_CPUS` is the logical core count of the node shape; each core context `fascon__cpu[core]` and each default own heap sits on pages of its own.
- At main and at every spawn_on entry, once per core, the generated C calls `f20_hpc_core_enter()`: `fascon_plat_pin_core(core)`, then binds the per-core arenas (the core TLS instances), `fascon__cpu[core]` and the default own heap of the core to `fascon_plat_numa_node(core)` with `fascon_plat_numa_bind` (MPOL_BIND). Shared tables marked `#[numa_node(n)]` are bound to node n by core 0 at main entry.

### 4.2 The node shape

`--topology cores=N,smt=K,numa=M[,layout=adjacent|interleave]`, or `// fascon-topology: cores=N smt=K numa=M` in the file; the default is `cores=128 smt=2 numa=8`. The physical core of logical core c is `c mod (cores / smt)` (interleaved, the Linux / x86 numbering: siblings are c and c + physical cores) or `c / smt` (adjacent); NUMA nodes split the physical cores evenly in order. The strict form writes the topology directive, so it compiles to the same C.

### 4.3 Platform API

```c
typedef struct { uint32_t cpus, cores, smt, numa_nodes; } FasconTopology;
void     fascon_plat_topology(FasconTopology *t);
void     fascon_plat_pin_core(uint32_t core);
uint32_t fascon_plat_numa_node(uint32_t core);
int      fascon_plat_numa_bind(void *base, size_t bytes, uint32_t node);   /* 0 = success */
```

The PC layer on Linux groups the usable logical CPUs (`sched_getaffinity`, so taskset and cgroups are honoured) into physical cores by the `/sys` topology; core k takes the first thread of the k-th physical core while k is below the physical core count (no SMT sibling is used), then the sibling threads. The NUMA node comes from `/sys/devices/system/cpu/cpuN/nodeM`; binding is the `mbind` system call (MPOL_BIND, MPOL_MF_MOVE) without libnuma. All of it is advisory: a refusal (containers, kernels without NUMA, a node the machine does not have) never stops the program; `FASCON_HPC_NO_PIN=1` turns it off. Windows pins only; macOS does nothing. Bare-metal builds without `FASCON2_BOARD_HPC` get do-nothing versions.

### 4.4 Checks: the management core and the compute cores

Core 0 runs `main` and manages; functions started with `spawn_on(k, ..)` with k > 0 (or a core known only at run time) are compute roots. Reachability follows the static call graph.

| Code | Severity | When |
| --- | --- | --- |
| F3901 | warning | MMIO, I/O ports, dma regions and arenas, `bind_mmio`, `submit_dma` / `complete_dma`, or gpu `dispatch` / `reclaim` reachable from a compute root. `#[allow(hpc_io)]` on the function or on the root lifts it |
| F3902 | error | a compute-reachable path allocates dynamically (`own`, `own_alloc`, gc_arena allocations, `promote_to_gc`, `gc_collect`, `dma_alloc`), or a `for id in @a` loop on such a path writes a field read by a Mirrored-SoA column (a filter bit column, a key column), rebuilding the mirror table-wide (writes made by functions the body calls count too: each call rebuilds an element's mirror, which adds up to the whole table). The second part is checked after code generation, because only it knows which columns are mirrors |
| F3903 | error | a constant spawn_on core at or above `cores`, `#[numa_node(n)]` at or above `numa`, or, with `#[no_smt]` on `fn main`, two cores (core 0 included) on sibling threads of one physical core |

Slot allocation in fixed tables (`@a.alloc(..)`) is not dynamic allocation: the tables are static and do not grow under this profile.

### 4.5 `fascon regions`

```text
  --target hpc: compute node cores=8 smt=2 numa=2 (// fascon-topology:), #[no_smt]
    core 0   management: main — physical core 0, NUMA node 0 (pinned at main's entry)
    core 1   compute root: `worker` (line 72) — physical core 1, NUMA node 0 (pinned at the spawn_on entry; ...)
    core 2   compute root: `worker` (line 73) — physical core 2, NUMA node 1 (...)
    #[numa_node(1)] totals → NUMA node 1 (bound by core 0 at main's entry)
    functions reachable from compute cores: worker, beacon
    F3901 I/O reachable from compute cores: the I/O port `post` — `beacon` (line 66) (lifted by #[allow(hpc_io)])
```

## 5. Quick fixes (`data/fixes.json`)

| Code | Fix |
| --- | --- |
| F2705, F2707 | make the table an ordinary `arena` (keep it on the CPU) |
| F2706 | `GpuBuf<a>` → `Id<a>` for the handle of an ordinary table |
| F3901 | insert `#[allow(hpc_io)]` before the enclosing function (new fix kind `insert_before_enclosing_fn`) |
| F3902 | run the work on the management core: `spawn_on(c, f, a)` → `f(a)` at the root (new fix kind `replace_in_note`) |
| F3903 | `spawn_on(c, ..)` → `spawn_on(1, ..)`; `#[numa_node(n)]` → `#[numa_node(0)]` |

Fixes are verified by recompiling; the language server also offers the fixes of the warnings F3901 and F2707.

## 6. Tests

- `tests/ok/tsoa_float.fsc`, `tsoa_float_nt.fsc`, `tsoa_rmw.fsc`: every `--columns` × `--nontemporal` combination prints the same; C shape and `fascon regions` checks.
- `tests/cols/rmw_kernels.c`: the masked kernels against a scalar model (zero, all-one and mixed words, tails, -0.0, NaN, overflowing words) at -O2 and under ASan / UBSan; `tests/cols/rmw_overflow.fsc`: the overflow panic is the same with `--columns on` and `off`.
- `tests/float/fmtfuzz.c` + `fmtcheck.py`: the float printer against Python on 400,000 bit patterns.
- `tests/cols/colfuzz.py --float` (NaN, -0.0, inf; `#[fast_math]` against written order) and `--rmw` (masked += / = on six column types with allocations and frees).
- `tests/ok/gpu_offload.fsc` + `gpu_offload.gpu.c`: worker-thread emulation, `--gpu=off`, ASan + UBSan, TSan; `tests/err/f0382_gpu_use_after_dispatch.fsc`, `f2705_*`, `f2706_*`, `f2603_gpu_wait_in_critical.fsc`, `f2603_gpu_wait_in_irq.fsc`, `f3203_gpu_wait_in_rcu.fsc`.
- `tests/ok/hpc_compute.fsc` (the C of `--target hpc`, the regions report); `tests/hpc/io_noise.fsc` (F3901); `tests/err/f3902_hpc_alloc.fsc`, `f3902_hpc_mirror.fsc`, `f3902_hpc_mirror_callee.fsc`, `f3903_hpc_core.fsc`, `f3903_hpc_numa.fsc`, `f3903_hpc_smt.fsc`.
- `bench/fsimd_bench.sh`: float sums and masked updates against C (AoS) at `-O2` and `-ffast-math` (bench/RESULTS.md).
