//! 実行ファイルに埋め込むファイル（ランタイム・BSP・例） / files embedded in the executable (runtime, BSP, examples)

pub const RUNTIME_H: &str = include_str!("../runtime/fascon2_rt.h");
pub const BSP_START_S: &str = include_str!("../bsp/qemu-virt-aarch64/start.S");
pub const BSP_C: &str = include_str!("../bsp/qemu-virt-aarch64/bsp.c");
pub const BSP_LD: &str = include_str!("../bsp/qemu-virt-aarch64/kernel.ld");
pub const BSP_GENOS_C: &str = include_str!("../bsp/qemu-virt-aarch64/genos.c");
/// 汎用 AArch64 BSP（board.toml の [boot] から実機の像を作る） / the generic AArch64 BSP (real-hardware images from [boot])
pub const GEN_A64_START: &str = include_str!("../bsp/generic-aarch64/start.S");
pub const GEN_A64_BSP: &str = include_str!("../bsp/generic-aarch64/bsp.c");
/// AArch64 の仮想 BIOS（-bios の像。フラッシュの 0 番地から走る） / the AArch64 virtual BIOS (-bios image run from flash at 0)
pub const BSP_FW_S: &str = include_str!("../bsp/qemu-virt-aarch64/firmware.S");
pub const BSP_FW_LD: &str = include_str!("../bsp/qemu-virt-aarch64/firmware.ld");
/// x86_64 の仮想 BIOS（-bios の像。4 GiB の直下・リセットベクタ 0xFFFF_FFF0） / the x86_64 virtual BIOS (-bios image right
/// below 4 GiB, reset vector at 0xFFFF_FFF0)
pub const BSP_X86_S: &str = include_str!("../bsp/qemu-x86_64-bios/bios.S");
pub const BSP_X86_C: &str = include_str!("../bsp/qemu-x86_64-bios/bsp.c");
pub const BSP_X86_LD: &str = include_str!("../bsp/qemu-x86_64-bios/bios.ld");
/// ユーザープログラム（EL0）の入口・プラットフォーム層・配置 / entry, platform layer and layout of user programs (EL0)
pub const USER_CRT0_S: &str = include_str!("../bsp/user-aarch64/crt0.S");
pub const USER_C: &str = include_str!("../bsp/user-aarch64/user.c");
pub const USER_LD: &str = include_str!("../bsp/user-aarch64/user.ld");
pub const USER_X86_CRT0_S: &str = include_str!("../bsp/user-x86_64/crt0.S");
pub const USER_X86_C: &str = include_str!("../bsp/user-x86_64/user.c");
pub const HOST_SHIM: &str = include_str!("../bsp/host/fascon2_plat_host.c");

/// (名前, ファイル名, 説明 ja, 説明 en, 本文) / (name, file name, description ja, description en, text)
pub const EXAMPLES: &[(&str, &str, &str, &str, &str)] = &[
    (
        "kernel",
        "kernel_core.fsc",
        "ミッション1: task_struct / VFS 風の双方向・循環構造（@process / @file / @inode アリーナ、世代タグ、ABA 防止）",
        "Mission 1: task_struct / VFS-like bidirectional, cyclic structures (@process / @file / @inode arenas, generation tags, ABA prevention)",
        include_str!("../examples/kernel_core.fsc"),
    ),
    (
        "gui",
        "gui_desktop.fsc",
        "ミッション2: BIOS なし AArch64 の GUI（固定番地 VRAM、own のウィンドウツリー、ボタン 2 個のウィンドウ）",
        "Mission 2: GUI on BIOS-less AArch64 (fixed-address VRAM, own window tree, a window with two buttons)",
        include_str!("../examples/gui_desktop.fsc"),
    ),
    (
        "dual",
        "gui_dual.fsc",
        "1 つのソースで x86_64 と AArch64 の両方で動く GUI カーネル（仮想 BIOS・port_io のキー入力・bump_arena・own_alloc）",
        "one GUI kernel source for both x86_64 and AArch64 (virtual BIOS, port_io key input, bump_arena, own_alloc)",
        include_str!("../examples/gui_dual.fsc"),
    ),
    (
        "bios",
        "bios_dual.fsc",
        "1 つのソースから x86_64 と AArch64 の仮想 BIOS（port_io の COM1・POST・bump_arena の行バッファ）",
        "one source, two virtual BIOS images for x86_64 and AArch64 (port_io COM1, POST, a bump_arena line buffer)",
        include_str!("../examples/bios_dual.fsc"),
    ),
    (
        "rust",
        "rust_style.rs",
        "自動リフレーミング: ふつうの Rust の構文（static mut + link_section・マクロ・println! の書式）のままのカーネル",
        "automatic reframing: a kernel in ordinary Rust syntax (static mut + link_section, macros, println! formatting)",
        include_str!("../examples/rust_style.rs"),
    ),
    (
        "app",
        "app_wordfreq.fsc",
        "一般アプリ: 単語の出現回数（arena のハッシュ表・own_alloc・行ごとの bump_arena）",
        "a general application: word frequencies (an arena hash table, own_alloc, a bump_arena per line)",
        include_str!("../examples/app_wordfreq.fsc"),
    ),
    (
        "callback",
        "own_callbacks.fsc",
        "own フィールド（`buffer: own i32`）と Move・連動解放、関数ポインタのコールバックと高階関数（stack_only / F0520）",
        "own fields (`buffer: own i32`) with moves and linked frees, function-pointer callbacks and higher-order functions (stack_only / F0520)",
        include_str!("../examples/own_callbacks.fsc"),
    ),
];

pub fn example(name: &str) -> Option<&'static (&'static str, &'static str, &'static str, &'static str, &'static str)> {
    EXAMPLES.iter().find(|e| e.0 == name || e.1 == name || e.1.trim_end_matches(".fsc") == name || e.1.trim_end_matches(".rs") == name)
}
