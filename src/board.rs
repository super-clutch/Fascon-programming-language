//! 宣言的なボード定義（board.toml）: メモリマップ・割り込みコントローラ・タイマー・UART・装置の窓を、コンパイラを変えずに
//! 有志が足せるようにする。`--board boards/qemu-virt-aarch64.toml` で読み、生成 C の横に fascon_board.h を書く
//! declarative board definitions (board.toml): memory map, interrupt controller, timer, UART and device windows, addable by
//! contributors without touching the compiler. Read with `--board boards/qemu-virt-aarch64.toml`; fascon_board.h is written
//! next to the generated C
use crate::target::{Profile, Target};
use crate::toml::Value;

#[derive(Clone, Debug, Default)]
pub struct Board {
    pub name: String,
    pub arch: Option<Target>,
    pub profile: Option<Profile>,
    pub cpus: u32,
    pub ram_base: u64,
    pub ram_size: u64,
    pub uart_kind: String,
    pub uart_base: u64,
    pub irq_kind: String,
    pub gicd: u64,
    pub gicc: u64,
    pub gicr: u64,
    pub timer_intid: u32,
    pub timer_hz: u64,
    pub devices: Vec<(String, u64, u64)>,
    pub user_str_max: Option<u64>,
    pub dma_pool_pages: Option<u64>,
    pub mmio_mirror: Option<u64>,
    pub arena_va_base: Option<u64>,
    /// 性能コアの番号（big.LITTLE の負荷分散で使う） / the performance-core numbers (used by big.LITTLE balancing)
    pub big_cores: Vec<u32>,
    /// [boot]: 実機の像を作る（ロード番地・コアごとのスタック・2 番目以降のコアの起こし方・終わり方）
    /// [boot]: build a real-hardware image (load address, per-core stack, how secondaries wake, how it ends)
    pub boot: Option<Boot>,
    /// [uart] shift: ns16550 のレジスタの間隔（2^shift バイト） / register spacing of an ns16550 (2^shift bytes)
    pub uart_shift: u32,
    /// [dma]: coherent = false ならキャッシュの掃き出し・無効化、bus_offset は装置から見た番地のずれ
    /// [dma]: coherent = false → cache clean / invalidate; bus_offset is the address offset seen by devices
    pub dma_coherent: bool,
    pub cache_line: u32,
    pub bus_offset: u64,
    /// [qemu]: この板を QEMU で試すとき（machine と追加の引数） / trying the board under QEMU (machine and extra args)
    pub qemu_machine: Option<String>,
    pub qemu_args: Vec<String>,
    pub qemu_binary: Option<String>,
    /// board.arch の文字列（riscv64 など、Target に無いものも） / the board.arch string (also ones Target does not know)
    pub arch_name: String,
    /// [toolchain]: cc・ld・cflags（AArch64 以外のボードのパッケージ） / cc, ld, cflags (board packages other than AArch64)
    pub toolchain: Option<(String, String, Vec<String>)>,
}

#[derive(Clone, Debug, Default)]
pub struct Boot {
    pub load_addr: u64,
    pub stack_size: u64,
    pub heap_size: u64,
    pub frame_size: u64,
    /// "none" | "spin-table" | "psci-hvc" | "psci-smc" | "ipi"
    pub smp: String,
    pub release_addrs: Vec<u64>,
    /// "semihosting" | "psci" | "loop"
    pub exit: String,
    /// 起動時の特権レベル（情報。start.S は EL3 / EL2 / EL1 のどれからでも EL1 へ降りる） / the boot privilege level
    /// (informational; start.S drops to EL1 from EL3 / EL2 / EL1)
    pub entry_el: String,
}

fn int(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(|x| x.as_int()).map(|x| x as u64)
}
fn st(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string())
}

/// board.toml を読む（失敗は (行, ja, en)） / read a board.toml (failures are (line, ja, en))
pub fn parse(text: &str) -> Result<Board, (usize, String, String)> {
    let v = crate::toml::parse(text).map_err(|e| (e.line, format!("board.toml が読めません: {}", e.msg), format!("cannot read board.toml: {}", e.msg)))?;
    let mut b = Board { name: st(&v, "board.name").unwrap_or_else(|| "custom".into()), cpus: int(&v, "board.cpus").unwrap_or(1) as u32, ..Default::default() };
    if let Some(a) = st(&v, "board.arch") {
        b.arch_name = a.clone();
        // [toolchain] を持つボードのパッケージは、どのアーキテクチャでもよい（riscv64・armv7m など。start.S / bsp.c は
        // パッケージが持つ）/ a board package with [toolchain] may be any architecture (riscv64, armv7m, ...; the package
        // carries start.S / bsp.c)
        b.arch = Target::parse(&a);
        if b.arch.is_none() && v.get("toolchain").is_none() {
            return Err((0, format!("board.arch `{}` は不明です（aarch64 / x86_64 / portable。ほかは [toolchain] を書く）", a), format!("unknown board.arch `{}` (aarch64 / x86_64 / portable; others need [toolchain])", a)));
        }
    }
    if let Some(p) = st(&v, "board.profile") {
        match crate::target::parse_spec(&p) {
            Some((_, Some(pr))) => b.profile = Some(pr),
            _ => return Err((0, format!("board.profile `{}` は不明です（rtos / generic-os）", p), format!("unknown board.profile `{}` (rtos / generic-os)", p))),
        }
    }
    b.ram_base = int(&v, "memory.ram_base").ok_or((0, "memory.ram_base が要ります".to_string(), "memory.ram_base is required".to_string()))?;
    b.ram_size = int(&v, "memory.ram_size").ok_or((0, "memory.ram_size が要ります".to_string(), "memory.ram_size is required".to_string()))?;
    b.uart_kind = st(&v, "uart.kind").unwrap_or_else(|| "pl011".into());
    b.uart_base = int(&v, "uart.base").unwrap_or(0);
    b.irq_kind = st(&v, "irq.kind").unwrap_or_else(|| "gicv2".into());
    b.gicd = int(&v, "irq.gicd").unwrap_or(0);
    b.gicc = int(&v, "irq.gicc").unwrap_or(0);
    b.gicr = int(&v, "irq.gicr").unwrap_or(0);
    b.timer_intid = int(&v, "timer.intid").unwrap_or(27) as u32;
    b.timer_hz = int(&v, "timer.hz").unwrap_or(0);
    if let Some(ds) = v.get("device").and_then(|x| x.as_array()) {
        for d in ds {
            let n = st(d, "name").unwrap_or_else(|| "device".into());
            let base = int(d, "base").ok_or((0, format!("装置 `{}` に base が要ります", n), format!("device `{}` needs a base", n)))?;
            let size = int(d, "size").ok_or((0, format!("装置 `{}` に size が要ります", n), format!("device `{}` needs a size", n)))?;
            b.devices.push((n, base, size));
        }
    }
    b.user_str_max = int(&v, "limits.user_str_max");
    b.dma_pool_pages = int(&v, "limits.dma_pool_pages");
    b.mmio_mirror = int(&v, "vm.mmio_mirror");
    b.arena_va_base = int(&v, "vm.arena_va_base");
    if let Some(a) = v.get("cpu.big").and_then(|x| x.as_array()) {
        b.big_cores = a.iter().filter_map(|x| x.as_int()).map(|x| x as u32).collect();
    }
    if b.uart_base == 0 {
        return Err((0, "uart.base が要ります".into(), "uart.base is required".into()));
    }
    b.uart_shift = int(&v, "uart.shift").unwrap_or(0) as u32;
    b.dma_coherent = v.get("dma.coherent").and_then(|x| x.as_bool()).unwrap_or(true);
    b.cache_line = int(&v, "dma.cache_line").unwrap_or(64) as u32;
    b.bus_offset = int(&v, "dma.bus_offset").unwrap_or(0);
    b.qemu_machine = st(&v, "qemu.machine");
    b.qemu_binary = st(&v, "qemu.binary");
    if v.get("toolchain").is_some() {
        let cflags: Vec<String> = v.get("toolchain.cflags").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.to_string()).collect()).unwrap_or_default();
        b.toolchain = Some((st(&v, "toolchain.cc").unwrap_or_else(|| "clang".into()), st(&v, "toolchain.ld").unwrap_or_else(|| "ld.lld".into()), cflags));
    }
    if let Some(a) = v.get("qemu.args").and_then(|x| x.as_array()) {
        b.qemu_args = a.iter().filter_map(|x| x.as_str()).map(|x| x.to_string()).collect();
    }
    if v.get("boot").is_some() {
        let smp = st(&v, "boot.smp").unwrap_or_else(|| "none".into());
        if !matches!(smp.as_str(), "none" | "spin-table" | "psci-hvc" | "psci-smc" | "ipi") {
            return Err((0, format!("boot.smp `{}` は不明です（none / spin-table / psci-hvc / psci-smc / ipi）", smp), format!("unknown boot.smp `{}` (none / spin-table / psci-hvc / psci-smc / ipi)", smp)));
        }
        let exit = st(&v, "boot.exit").unwrap_or_else(|| "loop".into());
        if !matches!(exit.as_str(), "semihosting" | "psci" | "loop") {
            return Err((0, format!("boot.exit `{}` は不明です（semihosting / psci / loop）", exit), format!("unknown boot.exit `{}` (semihosting / psci / loop)", exit)));
        }
        let release_addrs: Vec<u64> = v.get("boot.release_addrs").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|x| x.as_int()).map(|x| x as u64).collect()).unwrap_or_default();
        if smp == "spin-table" && release_addrs.len() + 1 < b.cpus as usize {
            return Err((0, "boot.smp = \"spin-table\" にはコア 1〜N の boot.release_addrs が要ります".into(), "boot.smp = \"spin-table\" needs boot.release_addrs for cores 1..N".into()));
        }
        let load_addr = int(&v, "boot.load_addr").ok_or((0, "boot.load_addr が要ります".to_string(), "boot.load_addr is required".to_string()))?;
        b.boot = Some(Boot {
            load_addr,
            stack_size: int(&v, "boot.stack_size").unwrap_or(64 << 10),
            heap_size: int(&v, "boot.heap_size").unwrap_or(8 << 20),
            frame_size: int(&v, "boot.frame_size").unwrap_or(1 << 20),
            smp,
            release_addrs,
            exit,
            entry_el: st(&v, "boot.entry_el").unwrap_or_else(|| "any".into()),
        });
    }
    Ok(b)
}

/// fascon_board.h（BSP と生成 C の両方が読む） / fascon_board.h (read by both the BSP and the generated C)
pub fn header(b: &Board) -> String {
    let mut o = String::new();
    o.push_str(&format!("/* fascon_board.h — generated from board.toml ({}) by fascon */\n#ifndef FASCON_BOARD_H\n#define FASCON_BOARD_H\n#ifndef __ASSEMBLER__\n#include <stdint.h>\n#endif\n", b.name));
    o.push_str(&format!("#define FASCON_BOARD_NAME \"{}\"\n", b.name));
    o.push_str(&format!("#define FASCON_BOARD_CPUS {}u\n", b.cpus.max(1)));
    o.push_str(&format!("#define FASCON_BOARD_RAM_BASE {:#x}ull\n#define FASCON_BOARD_RAM_SIZE {:#x}ull\n", b.ram_base, b.ram_size));
    o.push_str(&format!("#define FASCON_BOARD_UART_{} 1\n#define FASCON_BOARD_UART_BASE {:#x}ul\n", b.uart_kind.to_ascii_uppercase().replace('-', "_"), b.uart_base));
    o.push_str(&format!("#define FASCON_BOARD_IRQ_{} 1\n", b.irq_kind.to_ascii_uppercase().replace('-', "_")));
    if b.gicd != 0 {
        o.push_str(&format!("#define FASCON_BOARD_GICD {:#x}ul\n", b.gicd));
    }
    if b.gicc != 0 {
        o.push_str(&format!("#define FASCON_BOARD_GICC {:#x}ul\n", b.gicc));
    }
    if b.gicr != 0 {
        o.push_str(&format!("#define FASCON_BOARD_GICR {:#x}ul\n", b.gicr));
    }
    o.push_str(&format!("#define FASCON_BOARD_TIMER_INTID {}u\n", b.timer_intid));
    if b.timer_hz != 0 {
        o.push_str(&format!("#define FASCON_BOARD_TIMER_HZ {}ull\n", b.timer_hz));
    }
    if let Some(p) = b.dma_pool_pages {
        o.push_str(&format!("#define FASCON2_DMA_POOL_PAGES {}u\n", p));
    }
    if let Some(m) = b.mmio_mirror {
        o.push_str(&format!("#define FASCON2_MMIO_MIRROR UINT64_C({:#x})\n", m));
    }
    if let Some(m) = b.arena_va_base {
        o.push_str(&format!("#define FASCON2_ARENA_VA_BASE UINT64_C({:#x})\n", m));
    }
    let big: u64 = b.big_cores.iter().fold(0u64, |m, c| m | (1u64 << (c & 63)));
    o.push_str(&format!("#define FASCON_BOARD_BIG_MASK UINT64_C({:#x})\n", big));
    // 装置の窓（bind_mmio が受け付ける範囲）を X マクロで。ランタイムの fascon_board_init が main の最初に登録する
    // device windows (the ranges bind_mmio accepts) as an X macro; the runtime's fascon_board_init registers them first in main
    o.push_str("#define FASCON_BOARD_DEVICES(X) \\\n");
    for (n, base, size) in &b.devices {
        o.push_str(&format!("    X({:#x}ull, {:#x}ull) /* {} */ \\\n", base, size, n));
    }
    o.push_str("    /* end */\n");
    o.push_str(&format!("#define FASCON_BOARD_UART_SHIFT {}u\n", b.uart_shift));
    // 非一貫の DMA: submit_dma / complete_dma の前後でキャッシュラインごとに掃き出し・無効化（fascon2_rt.h）
    // non-coherent DMA: clean / invalidate by cache line around submit_dma / complete_dma (fascon2_rt.h)
    if !b.dma_coherent {
        o.push_str(&format!("#define FASCON2_DMA_NONCOHERENT 1\n#define FASCON2_CACHE_LINE {}u\n", b.cache_line.max(16)));
    }
    if b.bus_offset != 0 {
        o.push_str(&format!("#define FASCON2_DMA_BUS_OFFSET UINT64_C({:#x})\n", b.bus_offset));
    }
    if let Some(bt) = &b.boot {
        o.push_str(&format!("#define FASCON_BOARD_LOAD_ADDR {:#x}\n#define FASCON_BOARD_STACK_SIZE {:#x}\n", bt.load_addr, bt.stack_size));
        let smp = bt.smp.to_ascii_uppercase().replace('-', "_");
        o.push_str(&format!("#define FASCON_BOARD_SMP_{} 1\n#define FASCON_BOARD_SMP_NAME \"{}\"\n", smp, bt.smp));
        if !bt.release_addrs.is_empty() {
            let r: Vec<String> = bt.release_addrs.iter().map(|x| format!("{:#x}ull", x)).collect();
            o.push_str(&format!("#define FASCON_BOARD_RELEASE_ADDRS {}\n", r.join(", ")));
        }
        o.push_str(&format!("#define FASCON_BOARD_EXIT_{} 1\n", bt.exit.to_ascii_uppercase()));
    }
    o.push_str("#endif\n");
    o
}

/// 汎用 BSP のリンカスクリプト（ロード番地・コアごとのスタック・ヒープ・フレームを board.toml から）
/// the linker script of the generic BSP (load address, per-core stacks, heap and frame from board.toml)
pub fn linker_script(b: &Board) -> String {
    let bt = b.boot.clone().unwrap_or_default();
    let cpus = b.cpus.max(1);
    format!(
        "/* {name}.ld — generated from board.toml by fascon (generic BSP) */\nENTRY(_start)\nSECTIONS\n{{\n    . = {load:#x};\n    __kernel_start = .;\n    .text : {{ KEEP(*(.text.boot)) *(.text .text.*) }}\n    .rodata : ALIGN(16) {{ *(.rodata .rodata.*) }}\n    .data : ALIGN(16) {{ *(.data .data.*) }}\n    .tdata : {{ *(.tdata .tdata.*) }}\n    .tbss (NOLOAD) : ALIGN(128) {{ *(.tbss .tbss.*) *(.tcommon) }}\n    ASSERT(SIZEOF(.tdata) == 0, \"Fascon: initialized per-core data (.tdata) is not supported by the generic BSP\")\n    __tls_size = SIZEOF(.tbss);\n    __tls_align = ALIGNOF(.tbss);\n    .bss (NOLOAD) : ALIGN(16) {{ __bss_start = .; *(.bss .bss.*) *(COMMON) . = ALIGN(16); __bss_end = .; }}\n    .percpu (NOLOAD) : ALIGN(4096) {{ __percpu_start = .; . += {cpus} * ALIGN(SIZEOF(.tbss) + 4096, 4096); __percpu_end = .; }}\n    .stack (NOLOAD) : ALIGN(4096) {{ . += {cpus} * {stack:#x}; __stack_top = .; }}\n    .heap (NOLOAD) : ALIGN(4096) {{ __heap_start = .; . += {heap:#x}; __heap_end = .; }}\n    .frame (NOLOAD) : ALIGN(4096) {{ __frame_start = .; . += {frame:#x}; __frame_end = .; }}\n    __kernel_end = .;\n    ASSERT(__kernel_end <= {ramend:#x}, \"Fascon: the image does not fit in [memory] of board.toml\")\n    /DISCARD/ : {{ *(.comment) *(.note*) *(.eh_frame*) }}\n}}\n",
        name = b.name,
        load = bt.load_addr,
        cpus = cpus,
        stack = bt.stack_size,
        heap = bt.heap_size,
        frame = bt.frame_size,
        ramend = b.ram_base + b.ram_size
    )
}

/// ELF から生の像（PT_LOAD の中身を物理番地の順に並べたもの = objcopy -O binary と同じ）
/// a raw image from an ELF (the PT_LOAD contents laid out by physical address = what objcopy -O binary does)
pub fn elf_to_bin(elf: &[u8]) -> Result<(u64, Vec<u8>), String> {
    let rd16 = |o: usize| -> u64 { u16::from_le_bytes([elf[o], elf[o + 1]]) as u64 };
    let rd32 = |o: usize| -> u64 { u32::from_le_bytes([elf[o], elf[o + 1], elf[o + 2], elf[o + 3]]) as u64 };
    let rd64 = |o: usize| -> u64 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&elf[o..o + 8]);
        u64::from_le_bytes(b)
    };
    if elf.len() < 64 || &elf[0..4] != b"\x7fELF" || elf[4] != 2 || elf[5] != 1 {
        return Err("not a 64-bit little-endian ELF".into());
    }
    let phoff = rd64(0x20) as usize;
    let phentsize = rd16(0x36) as usize;
    let phnum = rd16(0x38) as usize;
    let mut segs: Vec<(u64, usize, usize)> = Vec::new();
    for i in 0..phnum {
        let o = phoff + i * phentsize;
        if o + 56 > elf.len() {
            return Err("truncated program headers".into());
        }
        if rd32(o) != 1 {
            continue; /* PT_LOAD だけ / PT_LOAD only */
        }
        let off = rd64(o + 8) as usize;
        let paddr = rd64(o + 24);
        let filesz = rd64(o + 32) as usize;
        if filesz == 0 {
            continue;
        }
        if off + filesz > elf.len() {
            return Err("segment outside the file".into());
        }
        segs.push((paddr, off, filesz));
    }
    if segs.is_empty() {
        return Err("no loadable segment".into());
    }
    let base = segs.iter().map(|s| s.0).min().unwrap_or(0);
    let end = segs.iter().map(|s| s.0 + s.2 as u64).max().unwrap_or(base);
    if end - base > (1u64 << 30) {
        return Err("segments span more than 1 GiB".into());
    }
    let mut out = vec![0u8; (end - base) as usize];
    for (pa, off, n) in segs {
        let d = (pa - base) as usize;
        out[d..d + n].copy_from_slice(&elf[off..off + n]);
    }
    Ok((base, out))
}
