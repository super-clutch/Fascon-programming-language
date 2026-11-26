/* link.ld — 移植用の参照プラットフォーム層の配置（翻訳とリンクの確かめ用。RAM の番地は ★ ボードに合わせる）
 * the layout for the reference platform layer (for compile / link checks; ★ set the RAM address for a board) */
ENTRY(fascon_stub_start)
SECTIONS
{
    . = 0x400000; /* 4 MiB: x86-64 の小さなコードモデル（32 ビットの番地）でも届く / reachable by the small code model of x86-64 */
    .text : { *(.text .text.*) }
    .rodata : ALIGN(16) { *(.rodata .rodata.*) *(.srodata .srodata.*) }
    .data : ALIGN(16) { *(.data .data.*) *(.sdata .sdata.*) }
    .tdata : ALIGN(16) { *(.tdata .tdata.*) }
    .tbss : ALIGN(16) { *(.tbss .tbss.*) *(.tcommon) }
    .bss (NOLOAD) : ALIGN(16) { *(.bss .bss.* .sbss .sbss.*) *(COMMON) }
    /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) *(.ARM.exidx*) *(.MIPS.abiflags) *(.reginfo) }
}
