//! Fascon 1.0 — ライフタイム注釈なしでメモリ安全な、Rust 風の構文から最適化しやすい C を生成するコンパイラ
//! Fascon 1.0 — compiles Rust-like syntax, memory-safe without lifetime annotations, into readable, optimizable C.

mod assets;
#[allow(dead_code)]
mod ast;
mod callback;
mod codegen;
mod context;
mod diag;
mod dict;
mod driver;
mod escape;
mod flow;
mod hpc;
mod json;
mod legacy;
mod lexer;
mod lsp;
mod parser;
mod prescan;
mod qemu;
mod reframe;
mod resolve;
#[allow(dead_code)]
mod sema;
mod span;
mod strict;
mod target;
#[allow(dead_code)]
mod types;
mod universal;
mod modules;
mod stack;
mod data;
mod fixrules;
mod toml;
mod stdlib;
mod board;
mod pkg;

use diag::Lang;
use std::path::{Path, PathBuf};
use std::process::exit;

#[derive(Clone)]
struct Opts {
    lang: Lang,
    engine: String,
    out_dir: Option<String>,
    ptr_bits: u32,
    json: bool,
    markdown: bool,
    line: Option<usize>,
    code: Option<String>,
    in_place: bool,
    out: Option<String>,
    screenshot: Option<String>,
    frames: u32,
    timeout: u64,
    /// `fascon qemu --disk 像`: virtio-blk のディスク像 / the disk image for virtio-blk
    disk: Option<String>,
    /// `fascon build --user app.fsc --abi kernel.fsc`: ユーザープログラム（EL0）として翻訳する / compile as a user program (EL0)
    user: bool,
    abi: Option<String>,
    /// `fascon qemu --smp N`: コアの数（spawn_on を使うプログラムの既定は 4） / the number of cores (4 by default for programs that use spawn_on)
    smp: Option<u32>,
    /// `fascon qemu --net udp:P1:P2`: virtio-net を UDP のソケットへつなぐ / connect virtio-net to a UDP socket
    net: Option<String>,
    gui: bool,
    headless: bool,
    build_only: bool,
    cc: Option<String>,
    /// 集計の列（--columns on|mirrored|off、既定 on） / reduction columns (--columns on|mirrored|off, default on)
    columns: bool,
    /// 完全列化（--columns on で真、mirrored で偽） / True SoA (true with --columns on, false with mirrored)
    true_soa: bool,
    /// 非時間的ストア（--nontemporal auto|on|off、既定 auto） / non-temporal stores (--nontemporal auto|on|off, default auto)
    nontemporal: codegen::NtMode,
    /// fascon run の C コンパイラに -march=native / -mcpu=native を渡すか（--cpu native|baseline、既定 native）
    /// whether `fascon run` passes -march=native / -mcpu=native to the C compiler (--cpu native|baseline, default native)
    native: bool,
    positional: Vec<String>,
    /// 0.1 エンジンへそのまま渡す引数（--engine を除く） / arguments passed unchanged to the 0.1 engine (without --engine)
    passthrough: Vec<String>,
    /// `--target x86_64 | aarch64 | portable`（C の I/O ポートの形と、`fascon qemu` の起動先） / the port I/O form in the C and
    /// what `fascon qemu` boots
    target: target::Target,
    /// `--target rtos | generic-os`（`--profile` でも可）: OS のプロファイル / the OS profile (`--profile` works too)
    profile: target::Profile,
    topology: Option<target::Topology>,
    /// `--board board.toml`: 宣言的なボード定義 / a declarative board definition
    board: Option<String>,
    /// 読んだボード定義 / the parsed board definition
    board_def: Option<board::Board>,
    /// 見つけた board.toml の道 / the path of the board.toml that was found
    board_path: Option<String>,
    /// `fascon pkg` のサブコマンドの残り / the rest of a `fascon pkg` command line
    /// `--reframing=rust | off`（None = ファイル先頭の `#![reframing(rust)]` に従う） / None = follow `#![reframing(rust)]`
    reframing: Option<bool>,
    /// `--emit-strict-fascon`: 機能安全証明形態の厳格な Fascon を書き出す / write the strict Fascon form
    emit_strict: bool,
}

fn parse_opts(args: &[String]) -> Opts {
    let mut o = Opts {
        lang: Lang::detect(),
        engine: "auto".into(),
        out_dir: None,
        ptr_bits: 64,
        json: false,
        markdown: false,
        line: None,
        code: None,
        in_place: false,
        out: None,
        screenshot: None,
        frames: 1,
        timeout: 60,
        disk: None,
        user: false,
        abi: None,
        smp: None,
        net: None,
        gui: false,
        headless: false,
        build_only: false,
        cc: None,
        columns: driver::columns_default(),
        true_soa: driver::true_soa_default(),
        nontemporal: driver::nontemporal_default(),
        native: true,
        board_path: None,
        positional: vec![],
        passthrough: vec![],
        target: driver::target_default(),
        profile: driver::profile_default(),
        topology: None,
        board: None,
        board_def: None,
        reframing: None,
        emit_strict: false,
    };
    // `--name=value` は `--name value` と同じ（`--emit-strict-fascon` などの値のない旗はそのまま）
    // `--name=value` equals `--name value` (flags without a value such as `--emit-strict-fascon` stay as they are)
    let mut args2: Vec<String> = Vec::new();
    let mut rest = false;
    for a in args {
        // `--` の後ろは手を付けない（位置引数として下で受け取る） / leave everything after `--` untouched (positional below)
        if rest || a == "--" {
            rest = true;
            args2.push(a.clone());
            continue;
        }
        match a.split_once('=') {
            Some((k, v)) if k.starts_with("--") && !k.is_empty() => {
                args2.push(k.to_string());
                args2.push(v.to_string());
            }
            _ => args2.push(a.clone()),
        }
    }
    let args = &args2[..];
    let mut i = 0;
    while i < args.len() {
        let a = args[i].clone();
        let mut val = || -> String {
            i += 1;
            args.get(i).cloned().unwrap_or_default()
        };
        match a.as_str() {
            "--" => {
                // `--` の後ろはすべて位置引数（`fascon dict -- --target` で `--target` という項目を引ける）
                // everything after `--` is positional (`fascon dict -- --target` looks up the entry `--target`)
                for b in &args[i + 1..] {
                    o.positional.push(b.clone());
                    o.passthrough.push(b.clone());
                }
                break;
            }
            // `--topology cores=64,smt=2,numa=4`（--target hpc の計算ノードの形） / the shape of the --target hpc compute node
            "--topology" => {
                let v = val();
                match target::Topology::parse(&v, 2) {
                    Some(t) => o.topology = Some(t),
                    None => {
                        eprintln!("--topology: cores=N,smt=K,numa=M[,layout=adjacent|interleave] ({})", v);
                        exit(2);
                    }
                }
            }
            "--target" | "--profile" => {
                let v = val();
                match target::parse_spec(&v) {
                    Some((a, p)) => {
                        if let Some(a) = a {
                            o.target = a;
                        }
                        if let Some(p) = p {
                            o.profile = p;
                        }
                    }
                    None => {
                        eprintln!("--target: x86_64 | aarch64 | portable | rtos | generic-os | hpc | generic-os+aarch64 ({})", v);
                        exit(2);
                    }
                }
            }
            "--board" => o.board = Some(val()),
            // `--gc=off`: gc_arena を F2623 で拒否（GC のランタイムを 1 バイトもリンクしない） / reject gc_arena with F2623
            "--gc=off" => universal::GC_OFF.store(true, std::sync::atomic::Ordering::Relaxed),
            "--gc=on" => universal::GC_OFF.store(false, std::sync::atomic::Ordering::Relaxed),
            "--gc" => {
                let v = val();
                universal::GC_OFF.store(matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "false" | "0" | "no"), std::sync::atomic::Ordering::Relaxed);
            }
            // `--fast-math`: すべての関数に #[fast_math]（浮動小数点の集計の再結合を許す） / #[fast_math] on every function
            // (float reductions may reassociate)
            "--fast-math" => universal::FAST_MATH.store(true, std::sync::atomic::Ordering::Relaxed),
            // `--gpu=off`: gpu_arena の dispatch を CPU のスタブで同期に実行（fascon_plat_gpu_* を呼ばない。F2707 の警告）
            // `--gpu=off`: run gpu_arena dispatches synchronously on the CPU stub (no fascon_plat_gpu_* call; the F2707 warning)
            "--gpu=off" => universal::GPU_OFF.store(true, std::sync::atomic::Ordering::Relaxed),
            "--gpu=on" => universal::GPU_OFF.store(false, std::sync::atomic::Ordering::Relaxed),
            "--gpu" => {
                let v = val();
                universal::GPU_OFF.store(matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "false" | "0" | "no" | "stub"), std::sync::atomic::Ordering::Relaxed);
            }
            "--reframing" => {
                let v = val();
                o.reframing = Some(!matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "none" | "fascon" | "false" | "0" | "no"));
            }
            "--emit-strict-fascon" | "--emit-strict" => o.emit_strict = true,
            "--engine" => o.engine = val(),
            "--lang" => {
                let v = val();
                if let Some(l) = Lang::parse(&v) {
                    o.lang = l;
                }
                o.passthrough.extend(["--lang".to_string(), v]);
            }
            "--out-dir" => {
                let v = val();
                o.out_dir = Some(v.clone());
                o.passthrough.extend(["--out-dir".to_string(), v]);
            }
            "--ptr-bits" => {
                let v = val();
                o.ptr_bits = if v == "32" { 32 } else { 64 };
                o.passthrough.extend(["--ptr-bits".to_string(), v]);
            }
            "--line" => {
                let v = val();
                o.line = v.parse().ok();
                o.passthrough.extend(["--line".to_string(), v]);
            }
            "--code" => {
                let v = val();
                o.code = Some(v.clone());
                o.passthrough.extend(["--code".to_string(), v]);
            }
            "--out" => {
                let v = val();
                o.out = Some(v.clone());
                o.passthrough.extend(["--out".to_string(), v]);
            }
            "--screenshot" => o.screenshot = Some(val()),
            "--frames" => o.frames = val().parse().unwrap_or(1).max(1),
            "--timeout" => o.timeout = val().parse().unwrap_or(60),
            "--disk" => o.disk = Some(val()),
            "--user" => o.user = true,
            "--smp" => o.smp = val().parse().ok().map(|n: u32| n.clamp(1, 4)),
            "--abi" => o.abi = Some(val()),
            "--net" => o.net = Some(val()),
            "--cc" => o.cc = Some(val()),
            "--columns" => {
                let v = val();
                o.columns = !matches!(v.as_str(), "off" | "no" | "0" | "false");
                o.true_soa = !matches!(v.as_str(), "mirrored" | "mirror");
            }
            "--nontemporal" => {
                let v = val();
                match driver::parse_nontemporal(&v) {
                    Some(m) => o.nontemporal = m,
                    None => {
                        eprintln!("{}", o.lang.pick(&format!("--nontemporal は auto / on / off のどれか（`{}` は使えません）", v), &format!("--nontemporal takes auto, on or off (not `{}`)", v)));
                        std::process::exit(2);
                    }
                }
            }
            "--cpu" => o.native = !matches!(val().as_str(), "baseline" | "generic" | "portable"),
            "--json" => {
                o.json = true;
                o.passthrough.push(a);
            }
            "--markdown" => {
                o.markdown = true;
                o.passthrough.push(a);
            }
            "--in-place" => {
                o.in_place = true;
                o.passthrough.push(a);
            }
            "--gui" => o.gui = true,
            "--headless" => o.headless = true,
            "--build-only" => o.build_only = true,
            _ => {
                if !a.starts_with("--") {
                    o.positional.push(a.clone());
                }
                o.passthrough.push(a);
            }
        }
        i += 1;
    }
    o
}

fn help(lang: Lang) {
    let v = driver::VERSION;
    if lang == Lang::Ja {
        println!(
            "Fascon v{v} — ライフタイム注釈なしでメモリ安全な Rust 風の構文 → 最適化しやすい C のコンパイラ

使い方:
  fascon build <file> [options]      C を生成（出力: build/<名前>.fascon.c と fascon2_rt.h。ソースは上書きしない）
  fascon check <file> [--json]       検査だけ（診断を表示）
  fascon explain <file> [--line N] [--code F2101]
                                     根本原因タイムライン・修正コード・数理的解説（Markdown）
  fascon fix <file> [--in-place]     検証済みの自動修正を適用（既定の出力: <名前>.fixed.fsc）
  fascon regions <file>              アリーナ・own・MMIO のリージョン確定結果と、静的に省いた検査の数
  fascon dict [語] [--json|--markdown]
                                     仕様辞書（0.1 と 1.0 の全項目。`--` で始まる語は `fascon dict -- --target`）
  fascon run <file> [--screenshot out.png]
                                     PC 上でビルドして実行（#[framebuffer] の画面を PNG に保存）
  fascon qemu <file> [--target x86_64|aarch64] [--headless] [--screenshot out.png] [--frames N] [--build-only] [--cc gcc|clang]
                                     QEMU で起動。--target なし: BIOS なしの AArch64（-kernel）。--target x86_64 / aarch64: Fascon の
                                     仮想 BIOS（-bios）から起動。画面は ramfb に出る
  fascon bios <file> [--target x86_64|aarch64]
                                     1 つのソースから x86_64 と AArch64 の仮想 BIOS の像（*.bios.bin）を作る
  fascon example [kernel|gui|dual|bios|rust|app|callback] [--out <file>]
                                     例のソースを書き出す（kernel・gui・dual = 両アーキテクチャの GUI・bios = 両アーキテクチャの
                                     BIOS・rust = 純 Rust 構文・app = 一般アプリ・callback = own フィールドとコールバック）
  fascon bsp [--out <dir>]           起動コード（AArch64: start.S / bsp.c / kernel.ld / firmware.S、x86_64: bios.S / bsp.c / bios.ld）を書き出す
  fascon gen-ld --board <name> ...   0.1 のボード用リンカスクリプト（0.1 エンジンへ委譲）
  fascon runtime-header [--out <dir>]
                                     fascon2_rt.h（と 0.1 の fascon_rt.h）を書き出す
  fascon doctor                      必要なツール（C コンパイラ・QEMU・0.1 エンジン）がそろっているか調べる
  fascon lsp                         言語サーバー（stdio・JSON-RPC）
  fascon version

共通オプション:
  --lang <ja|en>           表示言語（既定: FASCON_LANG → OS の表示言語）
  --engine <auto|1|0.1>    auto: 1.0 の構文（arena / bump_arena / port_io / mmio / own / type …）があれば 1.0、なければ 0.1（既定）
  --out-dir <dir>          出力フォルダ（既定: build）
  --ptr-bits <32|64>       ターゲットのポインタ幅（既定: 64）
  --columns <on|mirrored|off>
                           `for id in a` の集計ごとに、その集計が読む値だけの列を表に足す（既定: on）。on: 条件を満たすフィールドは
                           スロットから外して列を唯一の正本にする（完全列化 True SoA: 書き込みは 1 回）、ほかは写しの列（書き込みの
                           多いフィールドは列にしない）。mirrored: すべて写しの列（完全列化と書き込みの多いフィールドの除外なし）。
                           off: 1.0.0 と同じ表
  --nontemporal <auto|on|off>
                           完全列化の列への一括上書きのループで非時間的ストア（MOVNTI など）を使うか。auto: 列が 8 MiB 以上で、
                           割り込みハンドラ・タスクと共有せず、直後の文が同じ列を読まないとき（既定）。on: いつも（出口に
                           F20_SFENCE）。off: 使わない
  --fast-math              すべての関数に #[fast_math]: f32 / f64 の総和・最小 / 最大を列とベクトルで走査する（総和は結合の順序が
                           変わる。環境変数 FASCON_FAST_MATH=1 でも同じ。既定: 書いた順のループでビット一致）
  --gpu=off                gpu_arena の dispatch を CPU のスタブで同期に実行する（fascon_plat_gpu_* を呼ばない。F2707 の警告。
                           FASCON_GPU=off でも同じ）
  --cpu <native|baseline>  fascon run: この PC の CPU 向けに C をコンパイルする（-march=native / -mcpu=native、既定: native）
  --target <x86_64|aarch64|portable>
                           I/O ポート（port_io）の形: x86_64 = in / out 命令、aarch64 = コンパイラが MMIO へ写す（既定: portable = 両方を #if で）
  --target <rtos|generic-os|hpc>
                           OS のプロファイル。hpc: 64 ビット Id・2 MiB 境界のアリーナ・コアの物理コアへのピン留めと NUMA ローカルな
                           固定・計算コアの検査（F3901〜F3903）。ファイルの先頭の `// fascon-target: hpc` でも
  --topology <cores=N,smt=K,numa=M[,layout=adjacent]>
                           --target hpc の計算ノードの形（既定 cores=128 smt=2 numa=8。`// fascon-topology: ..` でも）
  --reframing <rust|off>   自動リフレーミング: ふつうの Rust の構文を Fascon へ書き換えてからコンパイル（既定: ファイル先頭の
                           #![reframing(rust)] に従う。環境変数 FASCON_REFRAMING でも指定できる）
  --emit-strict-fascon     機能安全証明形態の厳格な Fascon（すべての型・検証付きルックアップを明示）を <出力フォルダ>/<名前>.strict.fsc に書く"
        );
    } else {
        println!(
            "Fascon v{v} — Rust-like syntax, memory-safe without lifetime annotations → optimizable C compiler

Usage:
  fascon build <file> [options]      generate C (output: build/<name>.fascon.c and fascon2_rt.h; the source is never overwritten)
  fascon check <file> [--json]       check only (print diagnostics)
  fascon explain <file> [--line N] [--code F2101]
                                     root-cause timeline, fixed code and mathematical explanation (Markdown)
  fascon fix <file> [--in-place]     apply verified quick fixes (default output: <name>.fixed.fsc)
  fascon regions <file>              regions chosen for arenas, own and MMIO, and how many checks were elided statically
  fascon dict [term] [--json|--markdown]
                                     reference dictionary (all 0.1 and 1.0 entries; for a term starting with
                                     `--`, write `fascon dict -- --target`)
  fascon run <file> [--screenshot out.png]
                                     build and run on the PC (the #[framebuffer] screen is saved as PNG)
  fascon qemu <file> [--target x86_64|aarch64] [--headless] [--screenshot out.png] [--frames N] [--build-only] [--cc gcc|clang]
                                     boot on QEMU. Without --target: BIOS-less AArch64 (-kernel). With --target x86_64 / aarch64:
                                     boot through the Fascon virtual BIOS (-bios). The screen appears on ramfb
  fascon bios <file> [--target x86_64|aarch64]
                                     build the x86_64 and AArch64 virtual-BIOS images (*.bios.bin) from one source
  fascon example [kernel|gui|dual|bios|rust|app|callback] [--out <file>]
                                     write an example source (kernel, gui, dual = GUI for both architectures, bios = BIOS for
                                     both architectures, rust = pure Rust syntax, app = a general application, callback = own
                                     fields and callbacks)
  fascon bsp [--out <dir>]           write the boot code (AArch64: start.S / bsp.c / kernel.ld / firmware.S, x86_64: bios.S / bsp.c / bios.ld)
  fascon gen-ld --board <name> ...   linker script for a 0.1 board (delegated to the 0.1 engine)
  fascon runtime-header [--out <dir>]
                                     write fascon2_rt.h (and the 0.1 fascon_rt.h)
  fascon doctor                      check that the tools needed (C compilers, QEMU, 0.1 engine) are installed
  fascon lsp                         language server (stdio, JSON-RPC)
  fascon version

Common options:
  --lang <ja|en>           display language (default: FASCON_LANG, then the OS display language)
  --engine <auto|1|0.1>    auto: 1.0 when the file uses 1.0 syntax (arena / bump_arena / port_io / mmio / own / type ...), else 0.1 (default)
  --out-dir <dir>          output directory (default: build)
  --ptr-bits <32|64>       pointer width of the target (default: 64)
  --columns <on|mirrored|off>
                           give each `for id in a` reduction a column holding just the values it reads (default: on). on: eligible
                           fields leave the slot and their column becomes the only copy (True SoA: one write), the rest get mirrored
                           columns (write-heavy fields get none). mirrored: mirrored columns everywhere (no True SoA, no write-heavy
                           exclusion). off: the 1.0.0 tables
  --nontemporal <auto|on|off>
                           non-temporal stores (MOVNTI etc.) in bulk-overwrite loops over True-SoA columns. auto: when the column is
                           8 MiB or more, the arena is not shared with interrupt handlers or tasks, and the next statement does not
                           read the same column (default). on: always (F20_SFENCE at the exit). off: never
  --fast-math              #[fast_math] on every function: f32 / f64 sums and min / max scan columns with vectors (sums change
                           the association order; FASCON_FAST_MATH=1 does the same; default: loops in written order, bit for bit)
  --gpu=off                run gpu_arena dispatches synchronously on the CPU stub (no fascon_plat_gpu_* call; the F2707
                           warning; FASCON_GPU=off does the same)
  --cpu <native|baseline>  fascon run: compile the C for this PC's CPU (-march=native / -mcpu=native, default: native)
  --target <x86_64|aarch64|portable>
                           the form of I/O ports (port_io): x86_64 = in / out instructions, aarch64 = remapped to MMIO by the
                           compiler (default: portable = both, selected by #if)
  --target <rtos|generic-os|hpc>
                           the OS profile. hpc: 64-bit Ids, 2 MiB-aligned arenas, cores pinned to physical cores with NUMA-local
                           binding, compute-core checks (F3901-F3903); `// fascon-target: hpc` at the top of the file works too
  --topology <cores=N,smt=K,numa=M[,layout=adjacent]>
                           the compute node shape for --target hpc (default cores=128 smt=2 numa=8; `// fascon-topology: ..` too)
  --reframing <rust|off>   automatic reframing: rewrite ordinary Rust syntax into Fascon before compiling (default: follow
                           #![reframing(rust)] at the top of the file; the FASCON_REFRAMING environment variable works too)
  --emit-strict-fascon     write the strict Fascon form for functional-safety review (every type and verified lookup explicit)
                           to <out dir>/<name>.strict.fsc"
        );
    }
}

fn read_source(path: &str, lang: Lang) -> String {
    match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{}: {}: {}", lang.pick("ファイルを読めません", "cannot read the file"), path, e);
            exit(2);
        }
    }
}

/// 0.1 エンジンへ委譲するか / should this go to the 0.1 engine?
fn use_legacy(o: &Opts, text: &str) -> bool {
    // Rust の見た目（リフレーミング）・ターゲット指定・厳格形の書き出しは 1.0 だけの機能 / reframing, targets and the strict
    // form exist in 1.0 only
    if o.reframing == Some(true) || o.target != target::Target::Portable || o.profile != target::Profile::Classic || o.emit_strict || o.board.is_some() || driver::target_directive(text).is_some() {
        return false;
    }
    match o.engine.as_str() {
        // 0.1 系（同梱の fascon-core） / the 0.1 line (the bundled fascon-core)
        "0" | "0.1" | "0.1.0" | "legacy" | "core" => true,
        // 1.0（このエンジン。プレビューの頃の値 2 / 2.0 / new も互換のために 1.0 として受け付け、ヘルプには出さない） / 1.0 (this
        // engine; the values 2 / 2.0 / new from the preview are still accepted as 1.0 for compatibility, not shown in the help)
        "1" | "1.0" | "1.0.0" | "2" | "2.0" | "new" => false,
        _ => !driver::wants_v2(text) && legacy::core_path().is_some(),
    }
}

/// 0.1 エンジンが無いために 1.0 エンジンへ来た 0.1 だけの構文のファイル（F2900）への一言 / a note for a file with 0.1-only
/// syntax (F2900) that came to the 1.0 engine only because the 0.1 engine is missing
fn legacy_missing_note(o: &Opts, text: &str, cm: &driver::Compiled) {
    if o.engine == "auto" && cm.diags.iter().any(|d| d.code == "F2900") && !driver::wants_v2(text) && legacy::core_path().is_none() {
        eprintln!(
            "{}",
            o.lang.pick(
                "注: 0.1 エンジン（fascon-core）が見つからないので、このファイルは 1.0 エンジンで翻訳しました。0.1 だけの構文（spawn・chan・async・#[isr]）のファイルには、fascon と同じフォルダに fascon-core を置くか、環境変数 FASCON_CORE で場所を指定してください",
                "note: the 0.1 engine (fascon-core) was not found, so this file went to the 1.0 engine; files with 0.1-only syntax (spawn, chan, async, #[isr]) need fascon-core next to fascon or in FASCON_CORE"
            )
        );
    }
}

fn delegate(cmd: &str, o: &Opts) -> ! {
    let mut args = vec![cmd.to_string()];
    args.extend(o.passthrough.iter().cloned());
    match legacy::run(&args) {
        Some(code) => exit(code),
        None => {
            eprintln!(
                "{}",
                o.lang.pick(
                    "0.1 エンジン（fascon-core）が見つかりません。fascon と同じフォルダに置くか、環境変数 FASCON_CORE で場所を指定してください（1.0 エンジンで試すには --engine 1）",
                    "the 0.1 engine (fascon-core) was not found: put it next to fascon or set FASCON_CORE (to try the 1.0 engine, use --engine 1)"
                )
            );
            exit(2);
        }
    }
}

fn need_file(o: &Opts, cmd: &str) -> String {
    match o.positional.first() {
        Some(f) => f.clone(),
        None => {
            eprintln!("{}: fascon {} <file>", o.lang.pick("ファイルを指定してください", "specify a file"), cmd);
            exit(2);
        }
    }
}

fn stem_of(path: &str) -> String {
    let p = Path::new(path);
    let s = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "out".into());
    for ext in [".fsc.rs", ".fsc", ".fascon", ".rs"] {
        if let Some(x) = s.strip_suffix(ext) {
            return x.to_string();
        }
    }
    s
}

/// `--board board.toml` を読み、プロファイル・アーキテクチャの既定に使う / read `--board board.toml` and use it for the default
/// profile and architecture
/// `--board <名前か toml>` を board.toml の道へ: ファイルならそのまま。名前なら boards/<名前>/board.toml・boards/<名前>.toml を
/// 今のフォルダ・ソースのフォルダとその上・FASCON_BOARDS・実行ファイルの横（と 1 つ上）から探す。見つけたボードの
/// パッケージのフォルダは import の検索先（FASCON_PATH）に足す（ボードの .fsc を `import` できる）
/// `--board <name or toml>` → the path of board.toml: a file is used as is; a name is looked up as boards/<name>/board.toml
/// or boards/<name>.toml in the current folder, the source folder and its parents, FASCON_BOARDS and next to the executable
/// (and one level up). The folder of a board package is added to the import search path (FASCON_PATH)
fn resolve_board(spec: &str, src: Option<&str>) -> Option<std::path::PathBuf> {
    use std::path::{Path, PathBuf};
    let p = Path::new(spec);
    if p.is_file() {
        return Some(p.to_path_buf());
    }
    let mut roots: Vec<PathBuf> = vec![PathBuf::from(".")];
    if let Some(s) = src {
        let mut d = Path::new(s).parent().map(|x| x.to_path_buf()).unwrap_or_default();
        for _ in 0..4 {
            roots.push(if d.as_os_str().is_empty() { PathBuf::from(".") } else { d.clone() });
            if !d.pop() {
                break;
            }
        }
    }
    if let Ok(e) = std::env::var("FASCON_BOARDS") {
        for x in std::env::split_paths(&e) {
            roots.push(x.clone());
            if let Some(par) = x.parent() {
                roots.push(par.to_path_buf());
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        // 実行ファイルの横・1 つ上・2 つ上（VS Code の拡張機能では bin/<OS>/fascon の 2 つ上に boards/ がある）
        // next to the executable, one and two levels up (in the VS Code extension boards/ sits two levels above bin/<OS>/fascon)
        let mut d = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..3 {
            if let Some(x) = d.clone() {
                roots.push(x.clone());
                d = x.parent().map(|p| p.to_path_buf());
            }
        }
    }
    for r in roots {
        for c in [r.join("boards").join(spec).join("board.toml"), r.join("boards").join(format!("{}.toml", spec)), r.join(spec).join("board.toml")] {
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

fn load_board(o: &mut Opts) {
    let Some(spec) = o.board.clone() else { return };
    let Some(pb) = resolve_board(&spec, o.positional.first().map(|x| x.as_str())) else {
        eprintln!("--board {}: {}", spec, o.lang.pick("board.toml が見つかりません（ファイルの道、または boards/<名前>/board.toml・boards/<名前>.toml）", "board.toml not found (a file path, or boards/<name>/board.toml / boards/<name>.toml)"));
        exit(2);
    };
    if pb.file_name().map_or(false, |n| n == "board.toml") {
        if let Some(dir) = pb.parent() {
            let mut paths = vec![dir.to_path_buf()];
            if let Ok(e) = std::env::var("FASCON_PATH") {
                paths.extend(std::env::split_paths(&e));
            }
            if let Ok(j) = std::env::join_paths(paths) {
                std::env::set_var("FASCON_PATH", j);
            }
        }
    }
    let p = pb.to_string_lossy().to_string();
    o.board_path = Some(p.clone());
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("--board {}: {}", p, e);
            exit(2);
        }
    };
    match board::parse(&text) {
        Ok(b) => {
            if o.profile == target::Profile::Classic {
                if let Some(pr) = b.profile {
                    o.profile = pr;
                }
            }
            // [boot] のあるボード（汎用 BSP の実機の像）は移植可能な C のまま（SMP・ボードの BSP が使える）。arch は道具の選択だけ
            // a board with [boot] (a real-hardware image from the generic BSP) stays portable C (SMP and the board BSP work);
            // arch only picks the toolchain
            if o.target == target::Target::Portable && b.boot.is_none() {
                if let Some(a) = b.arch {
                    o.target = a;
                }
            }
            o.board_def = Some(b);
        }
        Err((line, ja, en)) => {
            eprintln!("error[F3801]: {}:{}: {}", p, line, o.lang.pick(&ja, &en));
            exit(2);
        }
    }
}

/// fascon_board.h を出力フォルダへ書く / write fascon_board.h into the output folder
fn write_board_header(o: &Opts, dir: &std::path::Path) {
    if let Some(b) = &o.board_def {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(dir.join("fascon_board.h"), board::header(b));
    }
}

fn driver_opts(o: &Opts, gen_c: bool) -> driver::Opts {
    let mut d = driver::Opts::new(o.lang, o.ptr_bits);
    d.board = o.board_def.is_some();
    d.user_str_max = o.board_def.as_ref().and_then(|b| b.user_str_max);
    d.gen_c = gen_c;
    d.columns = o.columns;
    d.true_soa = o.true_soa;
    d.nontemporal = o.nontemporal;
    if o.reframing.is_some() {
        d.reframing = o.reframing;
    }
    d.target = o.target;
    d.profile = o.profile;
    d.emit_strict = o.emit_strict;
    d.topology = o.topology;
    d
}

/// 厳格形を <出力フォルダ>/<名前>.strict.fsc に書く / write the strict form to <out dir>/<name>.strict.fsc
fn write_strict(cm: &driver::Compiled, path: &str, o: &Opts) {
    let Some(text) = &cm.strict else { return };
    let dir = PathBuf::from(o.out_dir.clone().unwrap_or_else(|| "build".into()));
    let target = dir.join(format!("{}.strict.fsc", stem_of(path)));
    let src_abs = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    if std::fs::canonicalize(&target).ok().as_ref() == Some(&src_abs) {
        eprintln!("{}", o.lang.pick("出力先がソースと同じファイルです", "the output would overwrite the source"));
        exit(2);
    }
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&target, text)) {
        eprintln!("{}: {}", o.lang.pick("書き込めません", "cannot write"), e);
        exit(2);
    }
    println!("{} {}", o.lang.pick("厳格形:", "strict form:"), target.display());
}

fn compile_or_exit(path: &str, text: &str, o: &Opts) -> driver::Compiled {
    let cm = driver::compile_user(path, text, &driver_opts(o, true), o.user, o.abi.as_deref());
    if !cm.diags.is_empty() {
        eprint!("{}", driver::render_diags(&cm, o.lang));
    }
    if cm.has_errors() {
        legacy_missing_note(o, text, &cm);
        let n = cm.diags.iter().filter(|d| d.is_error()).count();
        eprintln!("{}", o.lang.pick(&format!("エラー {} 件のため C を生成できません", n), &format!("cannot generate C: {} error(s)", n)));
        exit(1);
    }
    cm
}

/// C とランタイムヘッダを書き出す / write the C file and the runtime header
fn write_c(cm: &driver::Compiled, path: &str, o: &Opts) -> (PathBuf, PathBuf) {
    let dir = PathBuf::from(o.out_dir.clone().unwrap_or_else(|| "build".into()));
    let src_abs = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    let target = dir.join(format!("{}.fascon.c", stem_of(path)));
    if std::fs::canonicalize(&target).ok().as_ref() == Some(&src_abs) {
        eprintln!("{}", o.lang.pick("出力先がソースと同じファイルです", "the output would overwrite the source"));
        exit(2);
    }
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&target, cm.c.as_ref().unwrap())).and_then(|_| std::fs::write(dir.join("fascon2_rt.h"), assets::RUNTIME_H)) {
        eprintln!("{}: {}", o.lang.pick("書き込めません", "cannot write"), e);
        exit(2);
    }
    write_board_header(o, &dir);
    let abs = std::fs::canonicalize(&target).unwrap_or(target);
    let absdir = std::fs::canonicalize(&dir).unwrap_or(dir);
    (strip_verbatim(abs), strip_verbatim(absdir))
}

/// Windows の `\\?\C:\...` を普通のパスに戻す（C コンパイラが読めるように） / turn Windows `\\?\C:\...` back into a plain path (for C compilers)
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy().to_string();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

fn lang_arg(lang: Lang) -> String {
    if lang == Lang::Ja { "ja".into() } else { "en".into() }
}

fn cmd_dict(o: &Opts) {
    let lang = o.lang;
    let term = o.positional.first().cloned().unwrap_or_default();
    let have_core = legacy::core_path().is_some();
    // 同梱の 0.1.0 の辞書（`fascon-core dict --json` は検索語を見ずに全件を返す）。1.0 と同じ名前の項目は除き、1.0 の文を出す
    // the bundled 0.1.0 dictionary (`fascon-core dict --json` returns every entry whatever the term); entries whose name
    // 1.0 also has are dropped so that the 1.0 text is shown
    let legacy_entries = || -> Vec<json::J> {
        if !have_core {
            return Vec::new();
        }
        let la = ["dict", "--json", "--lang", if lang == Lang::Ja { "ja" } else { "en" }];
        match legacy::capture(&la).map(|(_, out)| json::parse(out.trim())) {
            Some(Ok(json::J::Arr(v))) => v.into_iter().filter(|e| !dict::entries().iter().any(|x| Some(x.name) == e.get("name").as_str())).collect(),
            _ => Vec::new(),
        }
    };
    let ja = lang == Lang::Ja;
    if term.is_empty() {
        // 全件: 0.1.0 の目次（文）または項目（JSON）と、1.0 の全項目 / everything: the 0.1.0 index (text) or entries (JSON),
        // then every 1.0 entry
        if o.json {
            let mut all = legacy_entries();
            all.extend(dict::entries_json(lang));
            println!("{}", json::J::Arr(all).dump());
            return;
        }
        let mut printed = false;
        if have_core {
            let mut a = vec!["dict".to_string()];
            if o.markdown {
                a.push("--markdown".into());
            }
            a.extend(["--lang".to_string(), lang_arg(lang)]);
            let la: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
            if let Some((0, out)) = legacy::capture(&la) {
                print!("{}", out);
                printed = true;
            }
        }
        if printed {
            println!("\n── Fascon 1.0 ──");
        }
        for e in dict::entries() {
            print_entry(e, o.markdown, lang);
        }
        return;
    }
    // 検索: 1.0 の完全一致 → 0.1.0 の完全一致（と 1.0 の部分一致）→ 1.0 の部分一致 → 0.1.0 の部分一致。完全一致（名前か
    // 別名）が、どちらの辞書でも部分一致より先に出る（`fascon dict match` は match の項目を出す）
    // lookup: exact 1.0 matches → exact 0.1.0 matches (with partial 1.0 matches) → partial 1.0 matches → partial 0.1.0
    // matches. An exact match (name or alias) in either dictionary comes before partial matches (`fascon dict match`
    // shows the entry for match)
    let exact = dict::find_exact(&term);
    let (theirs, ours): (Vec<json::J>, Vec<&dict::Entry>) = if !exact.is_empty() {
        (Vec::new(), exact)
    } else {
        let all = legacy_entries();
        let theirs_exact = dict::filter_json_exact(&all, &term);
        let partial = dict::find(&term);
        if !theirs_exact.is_empty() {
            (theirs_exact, partial)
        } else if !partial.is_empty() {
            (Vec::new(), partial)
        } else {
            (dict::filter_json(all, &term), Vec::new())
        }
    };
    if o.json {
        let names: Vec<&str> = ours.iter().map(|e| e.name).collect();
        let mut all = theirs;
        all.extend(dict::entries_json(lang).into_iter().filter(|e| e.get("name").as_str().map_or(false, |n| names.contains(&n))));
        println!("{}", json::J::Arr(all).dump());
        return;
    }
    if theirs.is_empty() && ours.is_empty() {
        println!("{}", lang.pick(&format!("`{}` に一致する項目はありません", term), &format!("no entry matches `{}`", term)));
        return;
    }
    let both = !theirs.is_empty() && !ours.is_empty();
    if both {
        println!("── Fascon 0.1.0 ──");
    }
    for e in &theirs {
        let f = |k: &str| e.get(k).as_str().unwrap_or("").to_string();
        if o.markdown {
            println!("### {}\n\n*{}* — {}\n\n{}\n", f("name"), f("category"), f("summary"), f("detail"));
        } else {
            print!("{}", dict::render_parts(&f("name"), &f("category"), &f("summary"), &f("detail"), &f("constraints"), &f("remedy"), ja));
        }
    }
    if both {
        println!("\n── Fascon 1.0 ──");
    }
    for e in &ours {
        print_entry(e, o.markdown, lang);
    }
}

fn print_entry(e: &dict::Entry, markdown: bool, lang: Lang) {
    if markdown {
        println!("### {}\n\n*{}* — {}\n\n{}\n", e.name, lang.pick(e.cat.0, e.cat.1), lang.pick(e.summary.0, e.summary.1), lang.pick(e.detail.0, e.detail.1));
    } else {
        print!("{}", dict::render(e, lang));
    }
}

/// 出力先のパイプが閉じられたとき（`fascon dict | head` など）に、内部エラーの報告ではなく静かに終える
/// end quietly, instead of reporting an internal error, when the output pipe is closed (e.g. `fascon dict | head`).
/// SIGPIPE は無視のまま（子プロセスへの書き込みの失敗はエラーとして扱える）、println! の失敗だけを拾う
/// SIGPIPE stays ignored (writes to child processes still fail with an error); only println! failures are caught
fn quiet_broken_pipe() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let text = info.to_string();
        let printing = text.contains("failed printing to stdout") || text.contains("failed printing to stderr");
        // EPIPE = 32（Linux / macOS）、ERROR_NO_DATA = 232・ERROR_BROKEN_PIPE = 109（Windows）
        let pipe = text.contains("(os error 32)") || text.contains("(os error 232)") || text.contains("(os error 109)");
        if printing && pipe {
            std::process::exit(if cfg!(unix) { 141 } else { 1 });
        }
        prev(info);
    }));
}

fn main() {
    // FASCON_GC=off（VS Code の設定・CI から）は --gc=off と同じ / FASCON_GC=off (from VS Code settings or CI) equals --gc=off
    if std::env::var("FASCON_GC").map_or(false, |v| matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "false" | "0" | "no")) {
        universal::GC_OFF.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // FASCON_GPU=off は --gpu=off、FASCON_FAST_MATH=1 は --fast-math と同じ / FASCON_GPU=off equals --gpu=off,
    // FASCON_FAST_MATH=1 equals --fast-math
    if std::env::var("FASCON_GPU").map_or(false, |v| matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "false" | "0" | "no" | "stub")) {
        universal::GPU_OFF.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    if std::env::var("FASCON_FAST_MATH").map_or(false, |v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "on" | "true" | "yes")) {
        universal::FAST_MATH.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    quiet_broken_pipe();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().cloned().unwrap_or_default();
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
    let mut o = parse_opts(&rest);
    load_board(&mut o);
    let lang = o.lang;
    match cmd.as_str() {
        "" | "help" | "--help" | "-h" => help(lang),
        "pkg" => exit(pkg::run(&rest, lang)),
        // `fascon stack app.fsc [--limit N] [--cc gcc] [--ext-budget N]`: rtos のスタックの最大消費量の証明
        // a proof of the maximum stack use for rtos
        "stack" => {
            let path = need_file(&o, &cmd);
            let text = read_source(&path, lang);
            let num = |k: &str, d: u64| -> u64 { rest.iter().position(|a| a == k).and_then(|i| rest.get(i + 1)).and_then(|v| v.replace('_', "").parse().ok()).unwrap_or(d) };
            let mut o2 = o.clone();
            if o2.profile == target::Profile::Classic && driver::target_directive(&text).map_or(true, |(_, p)| p.is_none()) {
                o2.profile = target::Profile::Rtos;
            }
            let cm = compile_or_exit(&path, &text, &o2);
            let (c_file, _) = write_c(&cm, &path, &o2);
            let rep = cm.report.as_ref();
            let cbs = rep.map(|r| r.callback_fns.clone()).unwrap_or_default();
            let irqs = rep.map(|r| r.irq_fns.clone()).unwrap_or_default();
            let cc = o.cc.clone().unwrap_or_else(|| "gcc".into());
            let extra = vec!["-DFASCON2_HOSTED_MMIO".to_string()];
            match stack::analyse(&c_file, &cc, &extra, &cbs, &irqs, num("--ext-budget", 512), num("--limit", 16384), lang) {
                Ok(r) => {
                    print!("{}", r.text);
                    let _ = r.worst;
                    exit(if r.ok { 0 } else { 1 });
                }
                Err(e) => {
                    eprintln!("fascon stack: {}", e);
                    exit(2);
                }
            }
        }
        // 内部用: 辞書と解説を両言語の JSON に書き出す（data/ を作り直すとき） / internal: dump the dictionary and explanations
        "__dump-data" => {
            let dir = PathBuf::from(rest.first().cloned().unwrap_or_else(|| "data".into()));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("dictionary.json"), dict::dump_bilingual().dump());
            let _ = std::fs::write(dir.join("rules.json"), driver::dump_rules().dump());
            println!("{}", dir.display());
        }
        "version" | "--version" | "-V" => {
            println!("fascon {} (engine 1.0: arenas / generations / own regions / MMIO regions)", driver::VERSION);
            match legacy::version() {
                Some(v) => println!("  + {} — {}", v, lang.pick("0.1 だけの構文のファイルはこちらが処理", "handles 0.1-only files")),
                None => println!("  {}", lang.pick("（0.1 エンジン fascon-core は見つかりません）", "(0.1 engine fascon-core not found)")),
            }
        }
        "build" | "check" | "explain" | "fix" | "regions" => {
            let path = need_file(&o, &cmd);
            let text = read_source(&path, lang);
            if use_legacy(&o, &text) {
                delegate(&cmd, &o);
            }
            match cmd.as_str() {
                "build" => {
                    let cm = compile_or_exit(&path, &text, &o);
                    let (target, dir) = write_c(&cm, &path, &o);
                    println!("{} {}", lang.pick("生成:", "generated:"), target.display());
                    // パッケージの中でのビルド: fascon.toml の安全性のラベルを構文から付け直す / building inside a package: refresh
                    // the safety label of fascon.toml from the syntax
                    {
                        let mut d = std::path::Path::new(&path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
                        for _ in 0..3 {
                            if d.join("fascon.toml").exists() {
                                if let Ok(label) = pkg::label_manifest(&d, lang) {
                                    println!("{} {} ({})", lang.pick("安全性:", "safety:"), label, d.join("fascon.toml").display());
                                }
                                break;
                            }
                            if !d.pop() {
                                break;
                            }
                        }
                    }
                    write_strict(&cm, &path, &o);
                    // ユーザープログラム（EL0）: 像（平らなバイナリ）まで作る。-o で置き場所を選べる（カーネルの embed が読む場所）
                    // a user program (EL0): build the image (a flat binary) too; -o picks where it goes (where the kernel's embed reads it)
                    // --board の board.toml に [boot] があれば、実機の像（ELF と生の .img）まで作る
                    // with [boot] in the --board board.toml, build the real-hardware image (ELF and raw .img) too
                    if let Some(b) = o.board_def.clone().filter(|b| b.boot.is_some()) {
                        if let Some(bp) = &o.board_path {
                            if let Some(d) = std::path::Path::new(bp).parent() {
                                std::env::set_var("FASCON_BOARD_DIR", d);
                            }
                        }
                        match qemu::build_board_image(&target, &dir, &stem_of(&path), &b, lang, o.cc.as_deref()) {
                            Ok((elf, img)) => {
                                let sz = std::fs::metadata(&img).map(|m| m.len()).unwrap_or(0);
                                println!("{} {}", lang.pick("生成:", "generated:"), elf.display());
                                println!("{} {} ({} bytes, {} {:#x})", lang.pick("生成:", "generated:"), img.display(), sz, lang.pick("ロード番地", "load address"), b.boot.as_ref().map_or(0, |x| x.load_addr));
                                if let Some(m) = &b.qemu_machine {
                                    println!("  {} -M {} -kernel {} -serial stdio -display none {}", b.qemu_binary.as_deref().unwrap_or("qemu-system-aarch64"), m, img.display(), b.qemu_args.join(" "));
                                }
                            }
                            Err(e) => {
                                eprintln!("{}", e);
                                exit(1);
                            }
                        }
                    }
                    if let Some((base, size)) = cm.user_layout {
                        // --target x86_64 なら Ring 3 の像、それ以外は AArch64 の EL0 の像 / a ring 3 image with --target x86_64,
                        // otherwise an AArch64 EL0 image
                        let built = if o.target == target::Target::X86_64 {
                            qemu::build_user_x86_64(&target, &dir, &stem_of(&path), base, size, lang, o.cc.as_deref())
                        } else {
                            qemu::build_user_aarch64(&target, &dir, &stem_of(&path), base, size, lang, o.cc.as_deref())
                        };
                        match built {
                            Ok(bin) => {
                                let mut shown = bin.clone();
                                if let Some(out) = &o.out {
                                    if let Err(e) = std::fs::copy(&bin, out) {
                                        eprintln!("{}: {}", lang.pick("書き込めません", "cannot write"), e);
                                        exit(2);
                                    }
                                    shown = PathBuf::from(out);
                                }
                                let n = std::fs::metadata(&shown).map(|m| m.len()).unwrap_or(0);
                                println!("{} {} ({} {}, {} {:#x})", lang.pick("ユーザープログラムの像:", "user program image:"), shown.display(), n, lang.pick("バイト", "bytes"), lang.pick("入口", "entry"), base);
                            }
                            // C までは出来ている。像が要る（--out）ときだけ失敗にする / the C exists; fail only when the image was asked for (--out)
                            Err(e) => {
                                eprintln!("{}", e);
                                if o.out.is_some() {
                                    exit(3);
                                }
                            }
                        }
                    }
                }
                "check" => {
                    let cm = driver::compile_user(&path, &text, &driver_opts(&o, false), o.user, o.abi.as_deref());
                    write_strict(&cm, &path, &o);
                    if o.json {
                        println!("{}", driver::diags_json(&cm, lang).dump());
                    } else if cm.diags.is_empty() {
                        println!("{} {}", lang.pick("エラーはありません:", "no errors:"), path);
                    } else {
                        eprint!("{}", driver::render_diags(&cm, lang));
                        legacy_missing_note(&o, &text, &cm);
                    }
                    exit(if cm.has_errors() { 1 } else { 0 });
                }
                "explain" => {
                    let cm = driver::compile_user(&path, &text, &driver_opts(&o, false), o.user, o.abi.as_deref());
                    match driver::explain(&cm, o.line, o.code.as_deref(), lang) {
                        Some((md, _)) => println!("{}", md),
                        None => println!("{}", lang.pick("このファイルにエラーはありません", "no errors in this file")),
                    }
                }
                "fix" => {
                    let cm = driver::compile_user(&path, &text, &driver_opts(&o, false), o.user, o.abi.as_deref());
                    if !cm.has_errors() {
                        println!("{}", lang.pick("このファイルにエラーはありません", "no errors in this file"));
                        return;
                    }
                    // 1. ライフタイム注釈の削除（F0001）→ 2. data/fixes.json の検証済みの修正を、進まなくなるまで繰り返す
                    // 1. remove lifetime annotations (F0001) → 2. apply verified fixes from data/fixes.json until nothing improves
                    let mut titles: Vec<String> = Vec::new();
                    let mut src_text = text.clone();
                    if let Some(f) = driver::explain(&cm, None, Some("F0001"), lang).map(|x| x.1).unwrap_or_default().into_iter().find(|f| f.verified && f.title.contains("lifetime") || f.verified && f.title.contains("ライフタイム")) {
                        titles.push(f.title.clone());
                        src_text = f.fixed_source;
                    }
                    let (fixed, more) = fixrules::fix_all(&path, &src_text, &driver_opts(&o, false));
                    titles.extend(more);
                    if titles.is_empty() {
                        eprint!("{}", driver::render_diags(&cm, lang));
                        eprintln!("{}", lang.pick("検証済みの自動修正はありません（fascon explain で直し方を確認してください）", "no verified quick fix (see fascon explain for how to fix it)"));
                        exit(1);
                    }
                    let f = driver::Fix { title: titles.join(" / "), verified: true, fixed_source: fixed };
                    let out = if o.in_place { PathBuf::from(&path) } else { Path::new(&path).with_file_name(format!("{}.fixed.fsc", stem_of(&path))) };
                    let re = driver::compile(&path, &f.fixed_source, lang, o.ptr_bits, false);
                    if let Err(e) = std::fs::write(&out, &f.fixed_source) {
                        eprintln!("{}: {}", lang.pick("書き込めません", "cannot write"), e);
                        exit(2);
                    }
                    println!("{} {}", lang.pick("適用:", "applied:"), f.title);
                    println!("{} {} ({} {})", lang.pick("出力:", "output:"), out.display(), lang.pick("残りのエラー:", "remaining errors:"), re.diags.iter().filter(|d| d.is_error()).count());
                }
                _ => {
                    let cm = driver::compile_user(&path, &text, &driver_opts(&o, false), o.user, o.abi.as_deref());
                    if cm.has_errors() {
                        eprint!("{}", driver::render_diags(&cm, lang));
                        exit(1);
                    }
                    print!("{}", driver::regions_text(&cm, lang));
                }
            }
        }
        "run" | "qemu" => {
            let path = need_file(&o, &cmd);
            let text = read_source(&path, lang);
            let cm = compile_or_exit(&path, &text, &o);
            let (c_file, dir) = write_c(&cm, &path, &o);
            let name = stem_of(&path);
            let has_fb = cm.c.as_deref().map_or(false, |c| c.contains("FASCON_MMIO_FRAMEBUFFER,"));
            // `fascon qemu --board <名前>`: ボードのパッケージの像を、その板の QEMU のマシン（[qemu] machine）で起動する。
            // 終わりはセミホスティング（実機の像は [boot] exit のまま）
            // `fascon qemu --board <name>`: boot the board package's image on that board's QEMU machine ([qemu] machine);
            // it ends through semihosting (the hardware image keeps [boot] exit)
            if cmd == "qemu" {
                if let Some(b) = o.board_def.clone().filter(|b| b.boot.is_some()) {
                    let Some(machine) = b.qemu_machine.clone() else {
                        eprintln!("{}", lang.pick("board.toml に [qemu] machine がありません（`fascon build --board` で像だけ作れます）", "board.toml has no [qemu] machine (`fascon build --board` still builds the image)"));
                        exit(2);
                    };
                    if let Some(bp) = &o.board_path {
                        if let Some(d) = std::path::Path::new(bp).parent() {
                            std::env::set_var("FASCON_BOARD_DIR", d);
                        }
                    }
                    let prev = std::env::var("FASCON_CFLAGS").unwrap_or_default();
                    std::env::set_var("FASCON_CFLAGS", format!("{} -DFASCON_BOARD_EXIT_SEMIHOSTING=1", prev));
                    let (_elf, img) = match qemu::build_board_image(&c_file, &dir, &name, &b, lang, o.cc.as_deref()) {
                        Ok(x) => x,
                        Err(e) => {
                            eprintln!("{}", e);
                            exit(3);
                        }
                    };
                    let qb = b.qemu_binary.clone().unwrap_or_else(|| "qemu-system-aarch64".to_string());
                    let Some(q) = qemu::find_tool(&[qb.as_str()]) else {
                        eprintln!("{} not found", qb);
                        exit(4);
                    };
                    let mut c = std::process::Command::new(&q);
                    c.args(["-M", &machine, "-kernel"]).arg(&img).args(["-serial", "stdio", "-display", "none", "-monitor", "none", "-semihosting"]).args(&b.qemu_args);
                    eprintln!("[fascon] {} -M {} -kernel {} -serial stdio -display none -monitor none -semihosting {}", q.display(), machine, img.display(), b.qemu_args.join(" "));
                    let st = c.status();
                    exit(st.ok().and_then(|s| s.code()).unwrap_or(5));
                }
            }
            if cmd == "run" {
                // GPU のカーネル: ソースの隣の `<名前>.gpu.c`（FASCON_GPU_KERNEL(番号) で登録）を一緒にビルドする
                // GPU kernels: `<name>.gpu.c` next to the source (registered with FASCON_GPU_KERNEL(id)) is built along
                let mut extra: Vec<PathBuf> = Vec::new();
                let kern = std::path::Path::new(&path).with_extension("gpu.c");
                if kern.is_file() && cm.c.as_deref().map_or(false, |c| c.contains("#define FASCON2_GPU 1")) {
                    extra.push(kern);
                }
                let exe = match qemu::build_host(&c_file, &dir, &name, lang, o.native, &extra) {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("{}", e);
                        exit(3);
                    }
                };
                let ppm = dir.join(format!("{}.ppm", name));
                let _ = std::fs::remove_file(&ppm);
                let st = std::process::Command::new(&exe).env("FASCON_SCREENSHOT", &ppm).status();
                let code = st.map(|s| s.code().unwrap_or(1)).unwrap_or(1);
                if ppm.is_file() {
                    let png_path = o.screenshot.clone().map(PathBuf::from).unwrap_or_else(|| dir.join(format!("{}.png", name)));
                    if let Some(png) = std::fs::read(&ppm).ok().and_then(|d| qemu::ppm_to_png(&d)) {
                        if std::fs::write(&png_path, png).is_ok() {
                            let _ = std::fs::remove_file(&ppm);
                            eprintln!("[fascon] {} {}", lang.pick("画面を保存:", "screen saved:"), png_path.display());
                        }
                    }
                }
                exit(code);
            }
            // 起動のしかた: 既定は AArch64 に ELF を -kernel で（BIOS なし）。--target を付けると Fascon の仮想 BIOS（-bios）
            // how to boot: by default the ELF goes to AArch64 with -kernel (no BIOS); with --target, the Fascon virtual BIOS (-bios)
            let built = match o.target {
                target::Target::Portable => qemu::build_aarch64(&c_file, &dir, &name, lang, o.cc.as_deref()).map(|e| (e, qemu::Boot::VirtKernel)),
                target::Target::Aarch64 => qemu::build_aarch64_bios(&c_file, &dir, &name, lang, o.cc.as_deref()).map(|e| (e, qemu::Boot::VirtBios)),
                target::Target::X86_64 => qemu::build_x86_64(&c_file, &dir, &name, lang, o.cc.as_deref()).map(|e| (e, qemu::Boot::PcBios)),
            };
            let (elf, boot) = match built {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("{}", e);
                    exit(3);
                }
            };
            println!("{} {}", lang.pick("生成:", "generated:"), elf.display());
            write_strict(&cm, &path, &o);
            if o.build_only {
                return;
            }
            let headless = o.headless || o.screenshot.is_some();
            let qo = qemu::QemuOpts { headless, screenshot: o.screenshot.clone().map(PathBuf::from), frames: o.frames, timeout_s: o.timeout, framebuffer: has_fb, disk: o.disk.clone().map(PathBuf::from), net: o.net.clone(), smp: o.smp.unwrap_or(if qemu::is_smp_c(&c_file) { 4 } else { 0 }), generic_os: boot == qemu::Boot::VirtKernel && qemu::is_generic_os_c(&c_file) };
            match qemu::run_qemu_boot(&elf, boot, &qo, lang) {
                Ok(code) => exit(code),
                Err(e) => {
                    eprintln!("{}", e);
                    exit(3);
                }
            }
        }
        "bios" => {
            // 1 つのソースから x86_64 と AArch64 の仮想 BIOS の像を作る（--target で片方だけ）
            // build the x86_64 and AArch64 virtual-BIOS images from one source (--target picks just one)
            let path = need_file(&o, &cmd);
            let text = read_source(&path, lang);
            let targets = match o.target {
                target::Target::Portable => vec![target::Target::X86_64, target::Target::Aarch64],
                t => vec![t],
            };
            let name = stem_of(&path);
            let dir = PathBuf::from(o.out_dir.clone().unwrap_or_else(|| "build".into()));
            let mut first = true;
            for t in targets {
                let mut d = driver_opts(&o, true);
                d.target = t;
                d.emit_strict = o.emit_strict && first;
                let cm = driver::compile_opts(&path, &text, &d);
                if !cm.diags.is_empty() && first {
                    eprint!("{}", driver::render_diags(&cm, lang));
                }
                if cm.has_errors() {
                    let n = cm.diags.iter().filter(|d| d.is_error()).count();
                    eprintln!("{}", lang.pick(&format!("エラー {} 件のため C を生成できません", n), &format!("cannot generate C: {} error(s)", n)));
                    exit(1);
                }
                if first {
                    write_strict(&cm, &path, &o);
                }
                first = false;
                let c_file = dir.join(format!("{}.{}.fascon.c", name, t.name()));
                if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&c_file, cm.c.as_ref().unwrap())).and_then(|_| std::fs::write(dir.join("fascon2_rt.h"), assets::RUNTIME_H)) {
                    eprintln!("{}: {}", lang.pick("書き込めません", "cannot write"), e);
                    exit(2);
                }
                write_board_header(&o, &dir);
                let c_file = strip_verbatim(std::fs::canonicalize(&c_file).unwrap_or(c_file));
                let absdir = strip_verbatim(std::fs::canonicalize(&dir).unwrap_or(dir.clone()));
                let r = match t {
                    target::Target::X86_64 => qemu::build_x86_64(&c_file, &absdir, &name, lang, o.cc.as_deref()),
                    _ => qemu::build_aarch64_bios(&c_file, &absdir, &name, lang, o.cc.as_deref()),
                };
                match r {
                    Ok(bin) => {
                        let size = std::fs::metadata(&bin).map(|m| m.len()).unwrap_or(0);
                        println!("{} {} ({} KiB)", lang.pick("生成:", "generated:"), bin.display(), size >> 10);
                        match t {
                            target::Target::X86_64 => println!(
                                "  qemu-system-x86_64 -M pc -m 2G -bios {} -nic none -no-reboot -device isa-debug-exit,iobase=0xf4,iosize=0x04 -nographic",
                                bin.display()
                            ),
                            _ => println!("  qemu-system-aarch64 -M virt -cpu cortex-a72 -m 256M -nic none -semihosting -nographic -bios {}", bin.display()),
                        }
                    }
                    Err(e) => {
                        eprintln!("{}", e);
                        exit(3);
                    }
                }
            }
        }
        "example" | "new" => {
            let name = o.positional.first().cloned().unwrap_or_default();
            if name.is_empty() {
                for e in assets::EXAMPLES {
                    println!("  {:<8} {:<18} {}", e.0, e.1, lang.pick(e.2, e.3));
                }
                println!("{}", lang.pick("書き出す: fascon example <名前> [--out <file>]", "write one: fascon example <name> [--out <file>]"));
                return;
            }
            let Some(e) = assets::example(&name) else {
                eprintln!("{}: {}", lang.pick("その例はありません", "no such example"), name);
                exit(2);
            };
            let out = PathBuf::from(o.out.clone().unwrap_or_else(|| e.1.to_string()));
            if out.exists() {
                eprintln!("{}: {}", lang.pick("すでにあるので上書きしません", "already exists, not overwriting"), out.display());
                exit(2);
            }
            if let Err(err) = std::fs::write(&out, e.4) {
                eprintln!("{}: {}", lang.pick("書き込めません", "cannot write"), err);
                exit(2);
            }
            println!("{} {}", lang.pick("生成:", "generated:"), out.display());
        }
        "bsp" => {
            let dir = PathBuf::from(o.out.clone().or_else(|| o.out_dir.clone()).unwrap_or_else(|| ".".into()));
            match qemu::write_bsp(&dir).and_then(|p| qemu::write_bsp_x86(&dir).map(|x| (p, x))) {
                Ok((p, x)) => {
                    let _ = std::fs::write(p.join("fascon2_plat_host.c"), assets::HOST_SHIM);
                    let _ = std::fs::write(p.join("fascon2_rt.h"), assets::RUNTIME_H);
                    let _ = std::fs::write(x.join("fascon2_rt.h"), assets::RUNTIME_H);
                    println!("{} {}", lang.pick("生成:", "generated:"), p.display());
                    println!("{} {}", lang.pick("生成:", "generated:"), x.display());
                }
                Err(e) => {
                    eprintln!("{}", e);
                    exit(2);
                }
            }
        }
        "runtime-header" => {
            let dir = PathBuf::from(o.out.clone().unwrap_or_else(|| ".".into()));
            if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join("fascon2_rt.h"), assets::RUNTIME_H)) {
                eprintln!("{}", e);
                exit(2);
            }
            println!("{} {}", lang.pick("生成:", "generated:"), dir.join("fascon2_rt.h").display());
            if legacy::core_path().is_some() {
                let mut a = vec!["runtime-header".to_string()];
                a.extend(o.passthrough.iter().cloned());
                let _ = legacy::run(&a);
            }
        }
        "doctor" => {
            let (text, ok) = qemu::doctor(lang);
            print!("{}", text);
            if ok {
                println!("{}", lang.pick("すべてそろっています。", "everything is in place."));
            }
        }
        "dict" => cmd_dict(&o),
        "gen-ld" => delegate("gen-ld", &o),
        "lsp" => exit(lsp::serve(lang)),
        other => {
            eprintln!("{}: {}", lang.pick("不明なコマンド", "unknown command"), other);
            help(lang);
            exit(2);
        }
    }
}
