/* user.ld — Fascon 1.0 ユーザープログラム（EL0）の配置 / the layout of a Fascon 1.0 user program (EL0)
 *   USER_BASE / USER_SIZE は、カーネルの `user_mem 名前 for タスク表: 大きさ at 番地;` から fascon が渡す（--defsym）
 *   USER_BASE / USER_SIZE come from the kernel's `user_mem name for tasks: size at address;` (passed with --defsym)
 *   [像: .text .rodata .data][.bss とアリーナ][フレーム 64 KiB][own ヒープ ...][スタック 64 KiB] ← 一番上が最初の SP
 *   [image: .text .rodata .data][.bss and arenas][frame 64 KiB][own heap ...][stack 64 KiB] ← the top is the first SP */
ENTRY(_start)

SECTIONS
{
    . = USER_BASE;
    .text : { KEEP(*(.text.boot)) *(.text .text.*) }
    .rodata : ALIGN(16) { *(.rodata .rodata.*) }
    .data : ALIGN(16) { *(.data .data.*) }
    __image_end = .;
    .bss (NOLOAD) : ALIGN(16) { *(.bss.fascon_arena .bss.fascon_arena.*) *(.bss .bss.*) *(COMMON) }
    . = ALIGN(4096);
    .frame (NOLOAD) : { __frame_start = .; . += 0x10000; __frame_end = .; }
    . = ALIGN(4096);
    __heap_start = .;
    __heap_end = USER_BASE + USER_SIZE - 0x10000;
    ASSERT(__heap_start + 0x4000 <= __heap_end, "Fascon: the user program does not fit in its user memory (image + arenas + frame + stack)")
    /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) }
}
