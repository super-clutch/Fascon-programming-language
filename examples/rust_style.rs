#![reframing(rust)]
// rust_style.rs — Fascon 1.0 の自動リフレーミング: ふつうの Rust の構文のまま書いたカーネルを、Fascon の安全規則
//                 （世代タグ付き Id・境界検査・own の所有）でコンパイルする
//                 Fascon 1.0 automatic reframing: a kernel written in ordinary Rust syntax, compiled under Fascon's
//                 safety rules (generation-tagged Ids, bounds checks, own ownership)
//
//   fascon run rust_style.rs                         # PC 上 / on the PC
//   fascon qemu rust_style.rs --target x86_64        # x86_64 の仮想 BIOS / the x86_64 virtual BIOS
//   fascon qemu rust_style.rs --target aarch64       # AArch64 の仮想 BIOS / the AArch64 virtual BIOS
//   fascon build rust_style.rs --emit-strict-fascon  # 書き換えた結果を厳格な Fascon で見る / see the strict Fascon result
//
// 先頭の `#![reframing(rust)]`（または --reframing=rust）でオン。`#![reframing(off)]` / --reframing=off でオフ。
// turned on by `#![reframing(rust)]` at the top (or --reframing=rust); off with `#![reframing(off)]` / --reframing=off.
#![no_std]
#![no_main]

use core::panic::PanicInfo;

// `#[link_section]` 付きの `static mut` はデバイスになる: ".uart" は移植できるシリアル（x86_64: COM1 の I/O ポート、
// AArch64: コンパイラが PL011 へ写す）。初期値は捨てられる（レジスタはプログラムが初期化するものではない）
// a `static mut` with `#[link_section]` becomes a device: ".uart" is the portable serial port (x86_64: the COM1 I/O
// ports, AArch64: remapped to the PL011 by the compiler); the initializer is dropped (registers are not program data)
#[link_section = ".uart"]
static mut UART: [u8; 8] = [0; 8];

const MAX_TASKS: usize = 16;

#[derive(Clone, Copy)]
pub struct Pcb {
    pub pid: u32,
    pub state: u8,
    pub name: &'static str,
}

pub struct Node {
    pub v: i64,
    pub next: Option<Id<scratch>>,
}

pub struct Block {
    pub len: u32,
    pub sum: i64,
}

// マクロの形の宣言 / declarations in macro form
arena!(pcb_table: Pcb[MAX_TASKS]);
bump_arena!(scratch: Node[64]);
port_io!(debug_exit: u8, 0xf4);

fn putc(c: u8) {
    unsafe {
        while UART[5] & 0x20 == 0 {}
        UART[0] = c;
    }
}

fn puts(s: &str) {
    let mut i = 0;
    while i < s.len() {
        putc(s.byte(i));
        i += 1;
    }
}

pub fn create_process(pid: u32, name: &'static str) -> Id<pcb_table> {
    pcb_table.alloc(Pcb { pid: pid, state: 0, name: name }).unwrap()
}

/// 呼び出しリージョン: ここで確保した scratch のノードは、戻るときに 1 回の格納でまとめて解放される
/// a call region: the scratch nodes allocated here are released together, by one store, when this returns
fn sum_list(n: i64) -> i64 {
    let mut head = None;
    for i in 0..n {
        head = scratch.alloc(Node { v: i, next: head });
    }
    let mut s = 0;
    let mut cur = head;
    while let Some(id) = cur {
        s += scratch[id].v;
        cur = scratch[id].next;
    }
    s
}

#[no_mangle]
pub extern "C" fn kernel_main() -> ! {
    let task = create_process(100, "init");
    pcb_table[task].state = 1;                 // 世代タグで検証する get_mut(task) と同じ / a verified get_mut(task)
    puts("reframing: pure Rust syntax on Fascon\n");
    putc(b'O');
    putc(b'K');
    putc(b'\n');
    // 書式の `{}` は put（整数・Str・bool を型で選ぶ）の列になる / `{}` in a format becomes put (type-directed) calls
    println!("task {} pid={} state={}", pcb_table[task].name, pcb_table[task].pid, pcb_table[task].state);
    let total = sum_list(10);
    let left = scratch.len();                  // 0: sum_list のリージョンは戻った / 0: sum_list's region has returned
    println!("sum_list(10) = {total}, scratch slots left = {left}");
    let mut data = own_alloc!(Block);          // 0 で埋めた own Block（スコープの終わりで自動解放） / a zero-filled own Block
    data.len = 3;
    data.sum = total;
    println!("own_alloc!(Block): len={} sum={} raw id={:#x}", data.len, data.sum, task.raw());
    println!("FASCON-RUST-OK");
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
