# Fascon 1.0 extensions — type inference, names without `@`, two-pass analysis, bump_arena, port_io, own_alloc, automatic reframing, the strict form, the virtual BIOS, own fields of any type, callbacks

日本語: [SPEC-1.0-EXT.ja.md](SPEC-1.0-EXT.ja.md) · the 1.0 base specification: [SPEC-1.0.md](SPEC-1.0.md) · usage: [USAGE-1.0.md](USAGE-1.0.md)

This document specifies the extensions to Fascon 1.0 (the version name stays 1.0); section 10 adds own fields of any type and function-pointer callbacks (stack_only, F0520). All of them keep Fascon's parsing rules
(no generics, no lifetime annotations, second-class references) while making code easier to write and extending what can be
written: bare metal, operating systems (with a GUI) and general applications on x86_64 and AArch64.

**What does not change**

- Safety by types (`Id<a>`) and generation tags: an arena's Ids are coloured nominal types and Ids of another arena cannot be
  passed (F2101); Ids of freed and reused slots no longer match the generation and yield None (lookups) or a safe stop (`a[id]`).
- Scan speed: for the existing programs (`examples/*.fsc`, `tests/**/*.fsc`) the generated C is identical to 1.0.0 apart from
  comments (`tools/compare_c.py` compares two compilers). Writing names without `@` gives the same C as well, because name
  resolution maps them back to the same expressions.

## 0. Pipeline

```text
lexing ─→ (automatic reframing: Rust look → Fascon forms)
       ─→ pass 1: pre-registration of names (structs, arenas, bump arenas, MMIO, ports, fns, statics, aliases)   src/prescan.rs
       ─→ pass 2: parsing (the bodies)                                                                           src/parser.rs
       ─→ name resolution (bare names → regions, desugaring of strict forms)                                     src/resolve.rs
       ─→ semantic analysis (type inference: up to two passes per function)                                      src/sema.rs
       ─→ flow analysis (moves, borrows, effects, liveness proofs, ranges)                                       src/flow.rs
       ─→ C generation (bump arenas, call regions, I/O ports, own_alloc)                                         src/codegen.rs
       ─→ (--emit-strict-fascon: the strict Fascon text)                                                          src/strict.rs
```

## 1. Fully automatic type inference (F0282 eradicated)

`let` needs no type annotation.

```rust
let task = create_process(100);        // Id<pcb_table> (from the return type)
let mut best = None;                   // Option<Id<tasks>> (from a later `best = Some(id)`)
let mut acc;                           // i32 (from a later `acc = 0`)
let ids = [a, b, c];                   // [Id<tasks>; 3]
```

- Type variables (`Ty::Var`) are solved with union-find from assignments, comparisons, call arguments, return values, arena
  indexing and method receivers.
- Each function is type-checked twice when needed: the let types found by the first pass seed the second, so a variable whose
  type is only settled further down may already be used as the base of a field, a method or an index (`tasks[best.unwrap()]`).
- Integers and `None` that nothing uses default to i32 (no error).
- F0282 remains only when a type is still unknown after two passes and is used as the base of a field, a method or an index.
  `--emit-strict-fascon` shows the inferred type of every `let`.

## 2. `@` abolished (optional) and bump_arena

### 2.1 Names without `@`

```rust
pcb_table[id].id = 100;                 // the same as the generation-verified .get_mut(id)
for t in tasks { .. }                   // iterate an arena
let x = pcb_table.alloc(Pcb { .. });
type ProcId<a: Pcb> = Id<a>;            // no `@` in alias parameters either
```

- A bare name denotes a region (arena, bump arena, MMIO, port) when no local of that name is in scope and the pass-1 table
  says it is a region; it becomes `ExprKind::ArenaRef` and then takes exactly the path of the old `@pcb_table`, so the static
  elision of generation checks, the latch and the column scans (reduction specializations) are unchanged.
- The old `@name` remains an explicit marker that always denotes the region (even under a local of the same name).
- Region names share the namespace of functions, statics and types (a clash is F0428).

### 2.2 What `pcb_table[id]` means

`R[i]` (R an arena or a bump arena) is **the same expression** as the strict forms `R.get_mut(i).unwrap()` (writes, `&mut`) and
`R.get(i).unwrap()` (read-only; writing through it is F0596). Name resolution maps the strict forms back to `R[i]`, so either
spelling gives the same C. Stale Ids are handled as in 1.0 (unproven accesses use the sentinel slot + the latch and stop safely
at the next commit point).

### 2.3 bump_arena

```rust
bump_arena scratch: Node[256];          // or bump_arena @scratch: Node[256];

fn sum_list(n: i64) -> i64 {            // this function is a "call region" of scratch
    let mut head = None;
    for i in 0..n {
        head = scratch.alloc(Node { v: i, next: head });   // just advances the top
    }
    ..
}                                       // at every exit one store resets the top to the entry mark = O(1) bulk release
```

| Item | Meaning |
| --- | --- |
| Table | `.bss.fascon_arena` (2 MiB aligned): `st` (top, peak, allocs; a 128 B block) \| `gen[2^K]` (64-bit) \| `slot[2^K + 1]` (with the sentinel) |
| Id | 64 bits `[generation 64-K \| index K]` (`Option<Id<b>>` uses 0 = None and stays 8 bytes) |
| Allocation | `i = top; if i >= CAP → None; g = gen[i] + 1; gen[i] = g; top = i + 1; slot[i] = v` |
| Check | `i < top && gen[i] == g && g != 0` (the branch-free forms feed the latch; writes divert to the sentinel) |
| Release | at **every exit** (the end and each `return`) of the call region (the function): `top = entry mark` (one store) |
| Scan | `for id in b` is a straight `0..top` loop; `b.len()` is the current top |
| Promotion | allocations of functions that return B's Ids or pass them out through `&mut` parameters are promoted to the caller's region |
| Generations | up to 2^(64-K) - 1 per slot; an exhausted slot is never handed out again (generations never wrap = no ABA) |

**Safety**: after a rewind a stale Id is "index ≥ top" or "wrong generation", so it yields None (lookups) or the sentinel + latch
(`b[id]`) and never reaches data placed in the same slot later. A callee only rewinds to its own entry mark (≥ the caller's top),
so the caller's Ids stay valid across calls.

**Static rules**

| Code | Meaning |
| --- | --- |
| F2401 | a bump_arena has no `free` / `remove` (no per-slot release) |
| F2402 | B's Ids cannot be stored where they outlive their region (elements of plain arenas or other bump arenas, statics) |
| F2103 | bump_arena elements cannot hold own (the bulk release runs no drop glue) |

**Generated C (`depth` in `tests/ok/bump.fsc`)**

```c
static int64_t fsc_depth(int64_t n) {
    const uint32_t _bm_scratch = arena_scratch.st.top; /* call region: the entry mark */
    ...
    fsc_Id_scratch _t2 = arena_scratch_alloc(&(fsc_Node){ .v = n });
    ...
    _rv1 = fascon_add_i64(arena_scratch_at_r(id, &_flt)->v, r);
    F20_COMMIT(_flt, "bump.fsc:48:1");
    arena_scratch.st.top = _bm_scratch; /* bulk release (one store) */
    return _rv1;
}
```

The return value is computed before the rewind (the return expression may read the bump arena).

## 3. I/O ports: `port_io` (x86_64 in / out, separate from AArch64 MMIO)

```rust
port_io com1 at_port 0x3f8;                 // u8 when the type is omitted
port_io com1_lsr at_port 0x3fd;
port_io regs: [u8; 7] at_port 0x3f9;        // consecutive registers (bounds-checked index)
#[mmio_map(0x0900_0000)] port_io dev: u32 at_port 0x5000;   // pick the AArch64 target explicitly

com1 = c;                                    // write
let s = com1_lsr;                            // read
regs[2] = 0x03;
```

| Target | `com1 = c;` | `let s = lsr;` |
| --- | --- | --- |
| `--target x86_64` | `mov dx, 0x3f8; out dx, al` (`fascon_outb`, inline asm with the "Nd" constraint) | `in al, dx` |
| `--target aarch64` | remapped to MMIO by the compiler: COM1 → the PL011 DR (`0x0900_0000`) | LSR built from the PL011 FR |
| PC run (`fascon run`) | the platform layer: COM1 transmit goes to stdout | LSR = transmit ready |
| default (portable) | the three above selected by `#if` | |

AArch64 mapping (`src/target.rs`):

| x86 port | AArch64 |
| --- | --- |
| COM1 (0x3f8-0x3ff) | a 16550 emulated on the PL011 (`f20_com1_in` / `f20_com1_out`): THR / RBR → DR, LSR ← FR (data ready = !RXFE, transmit ready = !TXFF), 0x3f8 while DLAB is set is only remembered as the divisor, IIR = 1, MSR = 0xB0 |
| COM2-4 | not connected (LSR = 0x60, writes dropped) |
| isa-debug-exit (0xf4 / 0x501) | exit (`(value << 1) \| 1`, the same exit code as QEMU x86) |
| anything else | MMIO in the PCI I/O window of QEMU virt (`0x3eff_0000 + port`) |
| with `#[mmio_map(address)]` | MMIO at that address |

Rules: elements u8 / u16 / u32 and ports 0 to 0xFFFF (F2205); overlapping ports are F2203; no references to ports (F2204).
Port reads and writes are external effects: a commit point precedes them and they are evaluated in the written order.

**One source, BIOS images for both architectures**: `fascon bios app.fsc` generates the C for `--target x86_64` and
`--target aarch64` and writes `build/app.x86_64.bios.bin` and `build/app.aarch64.bios.bin` (§7).

## 4. own_alloc(T): dynamic allocation without generics

```rust
let data = own_alloc(Block);       // a zero-filled own Block, freed automatically at the end of its scope
```

- The argument is a struct **type name** (not an expression); no `<T>` generics (the parsing rules stay as they are).
- Own children (non-Option own fields, also inside arrays and inline structs) are created zero-filled and **packed into the
  same own region** (one allocation, one free); `Option<own T>` fields start as None.
- A `let` that is never moved takes the frame region (a pointer bump); a moved value takes a heap own region.
- Rules: non-structs are F2301; types whose own children never end (`A { b: own B }`, `B { a: own A }`) are F2304 (make the way
  back `Option<own T>`); alignment above 16 bytes is F2032.

```c
const size_t _sz2 = FASCON_OWN_SPAN(fsc_Tree) + FASCON_OWN_CHILD_SPAN(fsc_Pair) + 5u * FASCON_OWN_CHILD_SPAN(fsc_Leaf) + ...;
char *_blk3 = (char *)fascon_own_alloc(_sz2, "own_alloc.fsc:21:17");   /* Tree and its 8 own children in one own region */
__builtin_memset(_blk3, 0, _sz2);
(void)fsc_zfill_Tree((fsc_Tree *)_blk3, _blk3 + FASCON_OWN_SPAN(fsc_Tree));
```

## 5. Two-pass analysis: free declaration order (F2104 eradicated)

Pass 1 (`src/prescan.rs`) walks the token stream and registers the names of all structs, arenas, bump arenas, MMIO regions,
ports, functions, statics and type aliases. Parsing and name resolution in pass 2 consult this table, so `main` may come first
and structs or arenas may be declared at the very end of the file. F2104 appears only when a name is declared nowhere.

## 6. Automatic reframing (ordinary Rust syntax, on / off)

On with `#![reframing(rust)]` at the top of the file or `--reframing=rust` (environment `FASCON_REFRAMING=rust`, VS Code
`fascon.reframing`); off with `#![reframing(off)]` / `--reframing=off`. Right after lexing the token stream is rewritten into
Fascon's own forms (`src/reframe.rs`); all meaning is then checked by Fascon's rules (generation tags, bounds checks, own).

| Rust look | Fascon form |
| --- | --- |
| `#[link_section = ".uart"] static mut UART: [u8; 8] = [0; 8];` | `port_io UART: [u8; 8] at_port 0x3f8;` (x86: COM1, AArch64: PL011) |
| `#[link_section = ".mmio.0x0900_0000"] static mut U: [u32; 7] = ..;` | `mmio U: [u32; 7] at 0x0900_0000;` |
| `#[link_section = ".port.0x3f8"] static mut COM1: u8 = 0;` | `port_io COM1: u8 at_port 0x3f8;` |
| `#[link_section = ".fb.0x4800_0000"] static mut FB: [[u32; 800]; 600] = ..;` | `#[framebuffer] mmio FB: .. at 0x4800_0000;` |
| `arena!(pcb: Pcb[64]);` / `bump_arena!(tmp: Node[256]);` | `arena pcb: Pcb[64];` / `bump_arena tmp: Node[256];` |
| `port_io!(com1, 0x3f8);` / `mmio!(uart: [u32; 18], 0x0900_0000);` | `port_io com1 at_port 0x3f8;` / `mmio uart: .. at ..;` |
| `let data = own_alloc!(Block);` | `let data = own_alloc(Block);` |
| `println!("pid {} = {:#x}", a, b)` / `{name}` | `{ put_str("pid "); put(a); put_str(" = "); put_hex(b); print_str(""); }` |
| `pcb_table[task].state = 1;` | the generation-verified `.get_mut(task)` (§2.2) |
| `unsafe { .. }` / `pub` / `use ..;` / `extern "C"` / `#[no_mangle]` / `#![no_std]` | removed |
| `'A'` / `b'A'` / `&'a T` / `&'static str` / `char` | `65` / `65` / `&T` / `Str` / `u32` |
| `while let P = e { .. }` | `loop { if let P = e { .. } else { break; } }` |
| `for b in s.bytes() { .. }` / `for &b in s.as_bytes() { .. }` / `for b in v.iter()` | `{ let rf_bytes1 = (s); for rf_i1 in 0..rf_bytes1.len() { let b = rf_bytes1.byte(rf_i1); .. } }` |
| `s.as_bytes()[i]` / `s.as_bytes().len()` / `let v = s.as_bytes(); v[i]` | `s.byte(i)` / `s.len()` / `v.byte(i)` (bounds-checked) |
| `&[u8]` / `b"..."` | `Str` (a read-only byte string; Fascon has no slices) |
| `-> !` / `const fn` / `struct S;` / `#[panic_handler] fn ..` | no return type / `fn` / `struct S {}` / removed |
| no `fn main` | `kernel_main` / `kmain` / `_start` / `rust_main` becomes main |

impl blocks, traits, generics and closures are not rewritten (errors under Fascon's rules). Example: `examples/rust_style.rs`.

### 6.1 The strict form (`--emit-strict-fascon`)

`fascon build app.rs --emit-strict-fascon` writes the Fascon "functional-safety review form" to `build/app.strict.fsc`:

- every `let` with its type, every integer literal with its type suffix (`100u32`)
- arena indexing as `R.get_mut(i).unwrap()` (writes) / `R.get(i).unwrap()` (reads)
- ports as `p.read()` / `p.write(v)` / `p.read_at(i)` / `p.write_at(i, v)`
- type aliases expanded, array lengths as numbers; `@` only where a local hides a region of the same name
- call-region notes and the record of what reframing rewrote (with line numbers)

**Round-trip guarantee**: compiling the strict form again gives the same C as the original source (comments and line numbers
aside); `tests/run_tests.py` checks this for every file in tests/ok and examples.

## 7. The virtual BIOS (x86_64 and AArch64)

No SeaBIOS, UEFI or U-Boot: Fascon's own boot code is the image QEMU loads with `-bios`.

**x86_64** (`bsp/qemu-x86_64-bios/`): QEMU pc maps the image right below 4 GiB and the CPU starts in real mode at 0xFFFF_FFF0.

1. 16-bit: load the GDT, enter protected mode
2. 32-bit: page tables at 0x1000-0x6FFF (identity map of 0-4 GiB, 2 MiB pages only, 3-4 GiB uncached) → PAE, EFER.LME and
   paging enter long mode; CR0.CD / NW cleared (caches on), CR4.OSFXSR set (SSE on)
3. 64-bit: copy the kernel image (.text / .rodata / .data) from the ROM to RAM at 1 MiB, zero .bss and the arenas, call `bsp_main`
4. `bsp_main`: COM1 at 115200 8N1, an IDT for the 32 exceptions (each prints the vector, RIP and CR2 and exits), the MMIO
   table check, ramfb through fw_cfg (I/O ports 0x510 / 0x511, DMA 0x514 / 0x518) when there is a `#[framebuffer]`, the 8 MiB
   own heap and the 1 MiB frame, then `fascon2_main()`
5. exit through isa-debug-exit (port 0xf4; QEMU ends with `(code << 1) | 1`, and `fascon qemu` returns the original code)

The ROM size follows the kernel image (the first link measures it, the second places the image right before the boot code; a
multiple of 64 KiB).

```text
qemu-system-x86_64 -M pc -m 2G -bios app.x86_64.bios.bin -nic none -no-reboot \
  -device isa-debug-exit,iobase=0xf4,iosize=0x04 -serial stdio [-vga none -device ramfb]
```

**AArch64** (`bsp/qemu-virt-aarch64/firmware.S` / `firmware.ld`): QEMU virt maps the image in flash (address 0) and CPU0 starts
at 0. The kernel image is copied from flash to RAM at 0x4008_0000 and control enters the same `_start` (start.S) as with `-kernel`.

```text
qemu-system-aarch64 -M virt -cpu cortex-a72 -m 256M -nic none -semihosting -bios app.aarch64.bios.bin -nographic
```

The framebuffer (`0x4800_0000`) lies in RAM on both architectures, so one GUI source is enough (`examples/gui_dual.fsc`, key
input through port_io COM1).

## 8. Changed sources

| File | What changed |
| --- | --- |
| `src/parser.rs` | pass-2 parsing: bare names interpreted with the pass-1 table; syntax of `bump_arena` / `port_io ... at_port` / `own_alloc(T)`; alias and `Id<name>` arguments classified by the table; `for id in name`; `while let` expansion under reframing |
| `src/sema.rs` | type inference (type variables, union-find, two passes per function, seeds, defaults; F0282 only in pass 2); bump_arena rules (F2401 / F2402 / F2103) and call regions (with promotion); port declarations, overlaps, reads and writes (F2205 / F2203 / F2204); own_alloc (F2301 / F2304 / F2032); the `put` builtin; F0428 |
| `src/codegen.rs` | bump-arena tables and alloc / get / at / at_r / validate; call-region marks and rewinds (every exit); straight `for id in b` loops; port functions (hosted / x86 in-out / AArch64 MMIO under `#if`, one form with `--target`); port reads and writes with commit points; own_alloc zero-fill and packed children (`fsc_zfill_T`); `put` |
| `src/lexer.rs` | the `bump_arena` / `port_io` keywords; char literals and lifetimes for reframing |
| `src/prescan.rs` (new) | pass 1: pre-registration of names |
| `src/resolve.rs` (new) | bare names → regions (scope-aware); desugaring of the strict `R.get(_mut)(i).unwrap()` and of port `read` / `write` |
| `src/reframe.rs` (new) | automatic reframing |
| `src/strict.rs` (new) | the strict-form printer |
| `src/target.rs` (new) | `--target` and the AArch64 mapping of I/O ports |
| `src/flow.rs` | ports (no references, writes), own_alloc, the read-only strict form (F0596) |
| `src/types.rs` / `src/ast.rs` | type variables, bump-arena and port tables, syntax-tree items |
| `src/driver.rs` / `src/main.rs` | the pipeline, `--target` / `--reframing` / `--emit-strict-fascon` / `fascon bios`, region reports |
| `src/qemu.rs` / `src/assets.rs` | building the x86_64 / AArch64 virtual BIOS (ELF → ROM image) and booting QEMU; `fascon doctor` |
| `src/lsp.rs` / `src/dict.rs` | hover on names without `@`, completions, dictionary entries |
| `runtime/fascon2_rt.h` | `f20_bid_t`, `F20_DEFINE_BUMP`, `f20_bump_validate`, `fascon_inb/outb/..`, `f20_com1_in/out` (AArch64) |
| `bsp/qemu-x86_64-bios/` (new) | `bios.S`, `bsp.c`, `bios.ld` |
| `bsp/qemu-virt-aarch64/firmware.S` / `firmware.ld` (new) | the AArch64 virtual BIOS |
| `bsp/host/fascon2_plat_host.c` | emulated I/O ports (COM1, isa-debug-exit, a 64 KiB port space) |

## 9. Tests

`python3 tests/run_tests.py target/release/fascon --qemu` checks the 52 earlier cases and additionally (113 in all):

- error detection: F2401, F2402 (tables, statics), F2103, F2304, F2205, F2204, F0596 (writing through `.get().unwrap()`), F2301
- run results: type inference, names without `@` and declaration order, bump_arena (promotion, recursion, stale-Id detection,
  full arena), port_io (PC emulation), own_alloc (packed children, nothing leaked), reframing (including str-as-bytes idioms)
- the strict-form round trip (21 files give the same C) and its contents
- `--target x86_64` C uses `out dx, al`, `--target aarch64` C maps to the PL011
- `fascon bios` (two images from one source, multiples of 64 KiB)
- virtual-BIOS boots with the same output on x86_64 and AArch64: bump, own_alloc, inference, names without `@`, frame,
  reframing, the vruntime scan, bios_dual, rust_style, kernel_core
- the GUI (gui_dual.fsc) on x86_64 and AArch64: three ramfb screens and key input through port_io COM1

## 10. own fields of any type and callbacks (same version 1.0)

### 10.1 own fields: `own i32`, `own [u8; N]`, `own Str`

```rust
struct MyContext {
    buffer: own i32,          // an owning pointer (int32_t * in C), freed together with the struct
    size: i32,
}

fn grow(c: &mut MyContext) { *c.buffer += c.size; }       // `*` reaches the contents of a non-struct own
fn consume(c: MyContext) -> i32 { *c.buffer * 10 + c.size } // taken by value: c.buffer is freed when it returns

fn main() {
    let mut ctx = MyContext { buffer: own 38, size: 4 };
    grow(&mut ctx);                // *ctx.buffer == 42
    ctx.buffer = own 7;            // replace the whole own: the old one is freed on the spot
    let moved = ctx;               // a move of the whole struct: ctx is cleared (F0382 if used again)
    print_int(consume(moved) as i64);
    let buf = own_alloc([u8; 4096]); // a zero-filled heap array
}
```

- `own τ` takes any value type: integers, `bool`, `Str`, Ids, arrays, Options and structs. References, `()`, `own own T` and
  function pointers cannot be owned (F0515 / F2301). `own_alloc(T)` takes the same types (`own_alloc(i32)`,
  `own_alloc([u8; 4096])`) and creates every own child zero-filled — also `own i32` fields, which therefore are never NULL.
- `*e` on an own (or a reference to an own) denotes its contents: `*ctx.buffer = 7;`, `*ctx.buffer += 1;`,
  `let v = *ctx.buffer;`, `&mut *ctx.buffer`. On references `*` still does nothing (they are auto-dereferenced), so existing
  programs are unaffected. Methods and `match` / `if let` on a non-struct own act on its contents (`b.take()`, `buf.len()`).
- A struct with own fields is move-only. A move clears the source in C (`__builtin_memset(&c, 0, sizeof c)`), so its drop
  does nothing; scope ends, overwrites, `drop(x)` and arena frees run the drop glue, which frees every own inside
  (`fsc_drop_MyContext` → `fsc_own_drop_T_i32(p->buffer)` → `fascon_own_free`). Deep copies use `.clone()`
  (`fsc_own_clone_T_i32`). Packing into one block (own_alloc, `own S { .. }` with struct children) and the frame region work
  as before.
- F0382 covers whole-struct moves followed by a field read, a field write or a write through `*`, on every path (branches,
  loops, `break`, `return`, function pointers). F0507 rejects moving an own field out (`let b = c.buffer;`) and moving a
  non-copyable value out of `*b`. Together they make a double free unwritable.

### 10.2 Function pointers, higher-order functions and stack_only callbacks

```rust
fn for_each(a: &mut [i32; 4], f: fn(&mut i32)) {
    for i in 0..4 { f(&mut a[i]); }          // lends a reference to a[i] for one call
}
fn add_one(x: &mut i32) { *x += 1; }

struct Handler { name: Str, on_click: fn(&mut Button) }
static OPS: [fn(&mut i32); 2] = [add_one, square];

for_each(&mut a, add_one);                  // a function name used as a value is a function pointer
(h.on_click)(&mut b);                       // a pointer in a field is called in parentheses
OPS[1](&mut a[0]);
```

- Types: `fn(T1, &mut T2) -> R`. Parameters may be references (they are parameter positions); the result may not (F0515);
  arrays are not passed or returned by value (F2040). Function pointers are copyable values: locals, parameters, struct
  fields, `Option<fn(..)>` (NULL = None in C) and static tables. In C each type is a typedef (`typedef void (*fsc_fn1)(int32_t *);`).
- Calls: a local, parameter or static of function-pointer type is called by name (`f(x)`, a local hides a function of the
  same name only when it is a function pointer); any other expression is called as `(expr)(args)` / `ops[i](x)`. The
  callee is evaluated first, then the arguments, then the commit point, then the call; `&mut @a[id]` arguments rebuild
  column mirrors right after the call, as for direct calls.
- Candidates (`src/callback.rs`): a function-pointer value only comes from a function name, so the functions an indirect
  call can reach are among the functions of that type used as values; for a parameter `f: fn(..)` the set is narrowed to
  the functions actually passed at direct call sites (a least fixpoint). Effects, F2502 and bump-arena call regions use these
  candidates. When a callback may touch an arena that the higher-order function has lent a reference into, F2502 names it:
  `the callback f (which may be reap) may free slots of tasks ...`.
- stack_only and F0520 (`src/escape.rs`): every function with reference parameters that is used as a value is a callback and
  must consume its references inside its own frame. Per function and reference parameter, an escape summary is computed as a
  least fixpoint over the call graph, following references made from references (`&mut x.f`, bindings of places behind them),
  wrapping values (`Some(x)`, struct / array literals, `own x`), locals bound to them, and paths through callees. Flows into a
  return value, an arena, a static, MMIO, a port, the place behind another reference, an own region or a by-value
  parameter (of a function or a function pointer) are reported as F0520 with the whole flow as the root-cause timeline;
  F0515 / F0308 at the same places are folded into it. Ownership taken out from behind a reference (`x.next.take()`) is not
  a reference and may be stored.

```text
error[F0520]: reference escape constraint violated: the callback `stash` lets its reference `h: &mut Holder` escape — stored into the arena `keep` (its slot lives until freed, longer than the call)
  --> f0520_callback_arena.fsc:8:5
   = 17:30: `stash` is passed to the higher-order function `for_each_holder` as its callback `f` (`fn(&mut Holder)`)
   = 7:10: `h: &mut Holder` is a second-class reference that the higher-order function lends for one call only (stack_only: read, write and consume it inside this frame)
   = 8:16: put into the field `p` of `Saved { .. }`
```

- Because references are second-class, a program that type-checks is stack_only already; F0520 is an independent check that
  points out code departing from it in the callback's context. New errors: F0520, F0618 (calling a non-function); F2301 now
  means "an own of something an own cannot hold"; F0515 also covers `own x` of a reference and references in function
  pointer results.
- Existing programs generate the same C (`tools/compare_c.py`, comments and whitespace aside).

| File | What changed |
| --- | --- |
| `src/ast.rs` / `src/parser.rs` | `TypeExpr::Fn` (`fn(..) -> R`), `ExprKind::CallIndirect` (`(e)(args)`), the `*` marks (`Program::derefs`) |
| `src/types.rs` | `Ty::Fn`, `own` of any value type (`Tables::ownable`), `holds_ref` |
| `src/sema.rs` | own of any value type, `*` on own, function values (`Res::Fn`), calls through function pointers (`CallKind::Indirect`), own_alloc of non-struct types, function signatures before statics |
| `src/callback.rs` (new) | candidates of indirect calls and of function-pointer parameters |
| `src/escape.rs` (new) | the stack_only escape analysis and F0520 with its timeline |
| `src/flow.rs` | indirect calls (moves, borrows, effects, F2502 naming the callback), F0382 / F0507 for `*b` |
| `src/codegen.rs` | function-pointer typedefs, indirect calls, static tables, non-struct own allocation / drop / clone / zero-fill, `*` |
| `src/strict.rs` / `src/dict.rs` / `src/driver.rs` | strict form of the new syntax, dictionary entries (`fn(T) -> R`, `stack_only`, F0520, F0618), the F0520 explanation |

Tests (136 with `--qemu`): `tests/ok/own_fields.fsc` (own i32 / arrays / Str / Option fields, moves, clones, own_alloc, arena
frees, nothing leaked), `tests/ok/callbacks.fsc` (higher-order functions, fields, static tables, Option<fn>, arena callbacks,
an own child detached by a callback), `examples/own_callbacks.fsc` (PC, BIOS-less AArch64 and the x86_64 virtual BIOS), and
16 error cases: six F0520 shapes, three F0382, two F0507, F2502 through a callback, F0618, F0308, F0515 and F2301. The two new
programs were also run under AddressSanitizer / UBSan / LeakSanitizer with `-DFASCON2_HEAP_MALLOC` (no error, no leak).
