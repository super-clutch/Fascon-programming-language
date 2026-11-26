//! ターゲット（`--target x86_64 | aarch64`）と、I/O ポートの写し方
//! targets (`--target x86_64 | aarch64`) and how I/O ports are mapped
//!
//! x86_64 は CPU に I/O ポート空間があるので、`port_io` の読み書きは `in` / `out` 命令そのもの（`com1 = c;` → `out dx, al`）。
//! AArch64 には I/O ポート空間が無いので、コンパイラが MMIO へ写し直す。よく使う PC のポートは QEMU virt の対応する機器へ
//! 写すので、同じソースが両方のアーキテクチャで動く:
//!   COM1〜COM4（0x3f8 / 0x2f8 / 0x3e8 / 0x2e8）の送受信レジスタ → PL011 UART（0x0900_0000）の DR（MMIO の代入・読み出し）
//!   同じく LSR（+5） → PL011 の FR から作る（送信可・受信あり）、それ以外のレジスタ（初期化）→ 何もしない / 0
//!   isa-debug-exit（0xf4 / 0x501） → セミホスティングの終了（QEMU が (値 << 1) | 1 で終わるのと同じ終了コード）
//!   それ以外 → QEMU virt の PCI の I/O 窓（0x3eff_0000 + ポート番号）への MMIO
//! `#[mmio_map(番地)]` を付けたポートは、その番地の MMIO に写す。
//! x86_64 has an I/O port space, so reads and writes of a `port_io` are the `in` / `out` instructions themselves
//! (`com1 = c;` → `out dx, al`). AArch64 has none, so the compiler remaps them to MMIO; common PC ports go to the matching
//! device of QEMU virt, so one source runs on both architectures:
//!   COM1-COM4 data registers → the DR of the PL011 UART (0x0900_0000) (an MMIO store / load)
//!   their LSR (+5) → built from the PL011 FR (transmit ready, data ready); other registers (setup) → no-op / 0
//!   isa-debug-exit (0xf4 / 0x501) → a semihosting exit (the same exit code as QEMU's (value << 1) | 1)
//!   anything else → MMIO in the PCI I/O window of QEMU virt (0x3eff_0000 + port number)
//! A port with `#[mmio_map(address)]` is remapped to MMIO at that address.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// アーキテクチャを決めない C（`#if` で x86_64 / AArch64 / PC 実行を選ぶ） / architecture-neutral C (`#if` picks x86_64 /
    /// AArch64 / a PC run)
    Portable,
    X86_64,
    Aarch64,
}

impl Target {
    pub fn parse(s: &str) -> Option<Target> {
        match s.trim().to_ascii_lowercase().as_str() {
            "x86_64" | "x86-64" | "x64" | "amd64" | "x86" | "x86_64-bios" | "pc" => Some(Target::X86_64),
            "aarch64" | "arm64" | "armv8" | "aarch64-virt" | "virt" => Some(Target::Aarch64),
            "portable" | "any" | "host" | "auto" => Some(Target::Portable),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Target::Portable => "portable",
            Target::X86_64 => "x86_64",
            Target::Aarch64 => "aarch64",
        }
    }
}

/// OS のプロファイル（`--target rtos` / `--target generic-os`）。アーキテクチャ（Target）とは独立した軸で、
/// `--target generic-os+aarch64` のように組み合わせられる。Classic は今までの 1.0（生成 C は 1 文字も変わらない）
/// the OS profile (`--target rtos` / `--target generic-os`), an axis independent of the architecture (Target); they combine
/// as in `--target generic-os+aarch64`. Classic is the 1.0 behaviour so far (its generated C does not change by one byte)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// 今までの 1.0（プロファイル指定なし） / plain 1.0 (no profile given)
    Classic,
    /// 組込みベアメタル RTOS: 32 ビット Id・固定長 .fascon_arena・再帰禁止とスタック消費の静的証明
    /// bare-metal RTOS: 32-bit Ids, fixed .fascon_arena, no recursion and a static proof of stack use
    Rtos,
    /// 汎用マルチプロセス OS: 64 ビット Id・仮想番地を予約したアリーナ・Per-CPU マガジン・RCU・PAN / PAC / BTI
    /// general multi-process OS: 64-bit Ids, VA-reserved arenas, per-CPU magazines, RCU, PAN / PAC / BTI
    GenericOs,
    /// HPC・計算ノード: 64 ビット Id・2 MiB 境界のアリーナ・計算コアの物理コアへの 1:1 のピン留め（SMT を避ける）・コアごとの
    /// メモリの NUMA ローカルな固定。管理コア（0）と計算コア（1..N）の役割を分ける検査（F3901〜F3903）
    /// HPC / compute nodes: 64-bit Ids, 2 MiB-aligned arenas, compute cores pinned 1:1 to physical cores (SMT avoided),
    /// per-core memory bound NUMA-locally; checks that separate the management core (0) from compute cores (1..N)
    /// (F3901-F3903)
    Hpc,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Profile::Classic => "classic",
            Profile::Rtos => "rtos",
            Profile::GenericOs => "generic-os",
            Profile::Hpc => "hpc",
        }
    }
    /// Id が 64 ビット（上位 32 = 世代・下位 32 = 添字）か / whether Ids are 64 bits (upper 32 = generation, lower 32 = index)
    pub fn id64(self) -> bool {
        matches!(self, Profile::GenericOs | Profile::Hpc)
    }
}

/// `--target hpc` の計算ノードの形（コンパイル時の検査 F3903 と、生成 C の FASCON2_MAX_CPUS に使う）。論理コア c の物理コアは
/// interleave（Linux / x86 の番号の振り方: 兄弟は c と c + 物理コア数）なら c mod 物理コア数、adjacent（兄弟が隣どうし）なら
/// c / smt。NUMA ノードは物理コアを順に等分する
/// the shape of an `--target hpc` compute node (used by the compile-time check F3903 and for FASCON2_MAX_CPUS in the
/// generated C). The physical core of logical core c is c mod (physical cores) when interleaved (the Linux / x86
/// numbering: siblings are c and c + physical cores), c / smt when adjacent (siblings side by side); NUMA nodes split the
/// physical cores evenly in order
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Topology {
    /// 論理コアの数 / logical cores
    pub cores: u32,
    /// 物理コアあたりのスレッド数（SMT） / threads per physical core (SMT)
    pub smt: u32,
    /// NUMA ノードの数 / NUMA nodes
    pub numa: u32,
    /// 兄弟スレッドが隣どうしの番号か / whether sibling threads have adjacent numbers
    pub adjacent: bool,
    /// どこで決まったか（0 = プロファイルの既定、1 = `// fascon-topology:`、2 = `--topology`）/ where it came from (0 = the
    /// profile default, 1 = `// fascon-topology:`, 2 = `--topology`)
    pub source: u8,
}

impl Topology {
    /// `--target hpc` の既定（ソースにも引数にも無いとき） / the `--target hpc` default (when neither the source nor the
    /// command line gives one)
    pub const HPC_DEFAULT: Topology = Topology { cores: 128, smt: 2, numa: 8, adjacent: false, source: 0 };

    /// 論理コア c の物理コア / the physical core of logical core c
    pub fn phys(&self, c: u32) -> u32 {
        let p = (self.cores / self.smt.max(1)).max(1);
        if self.adjacent {
            c / self.smt.max(1)
        } else {
            c % p
        }
    }

    /// 論理コア c の NUMA ノード / the NUMA node of logical core c
    pub fn node(&self, c: u32) -> u32 {
        let p = (self.cores / self.smt.max(1)).max(1);
        let per = (p / self.numa.max(1)).max(1);
        (self.phys(c) / per).min(self.numa.max(1) - 1)
    }

    /// `cores=64 smt=2 numa=4 [layout=adjacent]`（空白・`,`・`;` で区切る） / `cores=64 smt=2 numa=4 [layout=adjacent]`
    /// (separated by spaces, `,` or `;`)
    pub fn parse(spec: &str, source: u8) -> Option<Topology> {
        let mut t = Topology { source, ..Topology::HPC_DEFAULT };
        let mut any = false;
        for part in spec.split(|c: char| c.is_whitespace() || c == ',' || c == ';').filter(|x| !x.is_empty()) {
            let (k, v) = part.split_once('=')?;
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            match k.as_str() {
                "cores" | "cpus" | "threads" => t.cores = v.parse().ok().filter(|&n: &u32| n >= 1 && n <= 4096)?,
                "smt" => t.smt = v.parse().ok().filter(|&n: &u32| n >= 1 && n <= 8)?,
                "numa" | "nodes" => t.numa = v.parse().ok().filter(|&n: &u32| n >= 1 && n <= 1024)?,
                "layout" | "smt_layout" => {
                    t.adjacent = match v.to_ascii_lowercase().as_str() {
                        "adjacent" | "packed" => true,
                        "interleave" | "interleaved" | "linux" => false,
                        _ => return None,
                    }
                }
                _ => return None,
            }
            any = true;
        }
        if !any || t.cores % t.smt != 0 {
            return None;
        }
        Some(t)
    }

    /// `// fascon-topology:` に書く形 / the form written after `// fascon-topology:`
    pub fn spec(&self) -> String {
        format!("cores={} smt={} numa={}{}", self.cores, self.smt, self.numa, if self.adjacent { " layout=adjacent" } else { "" })
    }
}

/// `--target` の値を読む: アーキテクチャ・プロファイル・その組み合わせ（`+` `,` `:` `/` で区切る）
/// read a `--target` value: an architecture, a profile, or both (joined by `+`, `,`, `:` or `/`)
///   x86_64 | aarch64 | portable | rtos | generic-os | generic-os+aarch64 | rtos,x86_64 ...
pub fn parse_spec(s: &str) -> Option<(Option<Target>, Option<Profile>)> {
    let mut arch = None;
    let mut prof = None;
    for part in s.split(|c| c == '+' || c == ',' || c == ':' || c == '/') {
        let p = part.trim().to_ascii_lowercase();
        if p.is_empty() {
            continue;
        }
        match p.as_str() {
            "rtos" | "baremetal-rtos" | "mcu" => prof = Some(Profile::Rtos),
            "generic-os" | "generic_os" | "genericos" | "os" | "general" | "mobile" | "server" | "desktop" => prof = Some(Profile::GenericOs),
            "hpc" | "hpc-node" | "supercomputer" | "numa" | "workstation" => prof = Some(Profile::Hpc),
            "classic" | "1.0" => prof = Some(Profile::Classic),
            other => arch = Some(Target::parse(other)?),
        }
    }
    if arch.is_none() && prof.is_none() {
        return None;
    }
    Some((arch, prof))
}

/// QEMU virt の PCI I/O 窓（x86 のポート番号をこの先頭からの距離として置く） / the PCI I/O window of QEMU virt
pub const VIRT_PIO_WINDOW: u64 = 0x3eff_0000;

/// AArch64 で 1 つのポートのレジスタ（先頭からのバイト位置 off）へのアクセスをどう写すか
/// how an access to one register (byte offset off from the port base) is mapped on AArch64
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum A64Map {
    /// この番地の MMIO（幅はポートの要素の幅） / MMIO at this address (as wide as the port's element)
    Mmio { addr: u64, bits: u32 },
    /// COM1 の 16550 のレジスタ（0〜7）。ランタイムの f20_com1_in / f20_com1_out が PL011 で真似る
    /// (THR / RBR → DR、LSR ← FR、DLAB の間の除数・その他の設定レジスタは覚えておくだけ)
    /// a 16550 register of COM1 (0-7), emulated on the PL011 by the runtime's f20_com1_in / f20_com1_out
    /// (THR / RBR → DR, LSR ← FR; the divisor while DLAB is set and the other setup registers are only remembered)
    Com1(u32),
    /// 読み出しは定数、書き込みは捨てる（つながっていない COM2〜4） / reads give a constant, writes are dropped (COM2-4,
    /// which are not connected)
    Const(u64),
    /// isa-debug-exit: 書き込みで終了 / isa-debug-exit: a write exits
    DebugExit,
}

const COM_BASES: [u64; 4] = [0x3f8, 0x2f8, 0x3e8, 0x2e8];

/// ポート p（x86 のポート番号）を AArch64 でどう扱うか。mmio_map は `#[mmio_map(番地)]` / mmio_map is `#[mmio_map(address)]`
pub fn aarch64_map(p: u64, width_bits: u32, base_port: u64, mmio_map: Option<u64>) -> A64Map {
    if let Some(m) = mmio_map {
        return A64Map::Mmio { addr: m + (p - base_port), bits: width_bits };
    }
    if let Some(com) = COM_BASES.iter().position(|b| p >= *b && p < *b + 8) {
        let off = p - COM_BASES[com];
        // COM1 だけが PL011 に写る（QEMU virt の UART は 1 個） / only COM1 reaches the PL011 (QEMU virt has one UART)
        if com == 0 {
            return A64Map::Com1(off as u32);
        }
        // LSR = 0x60（送信可・受信なし）: 送信待ちのループはすぐ抜ける / LSR = 0x60 (transmit ready, no data): transmit
        // polling loops move on at once
        return A64Map::Const(if off == 5 { 0x60 } else { 0 });
    }
    if p == 0xf4 || p == 0x501 {
        return A64Map::DebugExit;
    }
    A64Map::Mmio { addr: VIRT_PIO_WINDOW + p, bits: width_bits }
}

/// 1 行の説明（生成 C の注釈） / a one-line description (for comments in the generated C)
pub fn describe(m: &A64Map) -> String {
    match m {
        A64Map::Mmio { addr, .. } => format!("MMIO {:#x}", addr),
        A64Map::Com1(0) => "COM1 THR / RBR → PL011 DR".to_string(),
        A64Map::Com1(5) => "COM1 LSR ← PL011 FR".to_string(),
        A64Map::Com1(o) => format!("COM1 +{} (16550 setup register, remembered)", o),
        A64Map::Const(_) => "COM2-4 (not connected)".to_string(),
        A64Map::DebugExit => "isa-debug-exit → exit((v << 1) | 1)".to_string(),
    }
}
