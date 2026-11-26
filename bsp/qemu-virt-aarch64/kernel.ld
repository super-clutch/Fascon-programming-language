/* kernel.ld — Fascon 1.0 BSP: QEMU virt AArch64（RAM は 0x4000_0000 から / RAM starts at 0x4000_0000）
 * 0x4000_0000〜0x4007_FFFF は QEMU が DTB を置くので避ける / QEMU puts the DTB there, so it is avoided
 *
 * .fascon_arena: 生成 C のアリーナ（.bss.fascon_arena）を 2 MiB 境界にまとめる。RAM は 2 MiB ブロック（または 1 GiB ブロック）
 *                で恒等マップするので、アリーナはブロックの途中から始まらない。start.S が 0 で埋める
 *                the arenas of the generated C (.bss.fascon_arena) gathered on a 2 MiB boundary; RAM is identity-mapped with
 *                2 MiB blocks (or one 1 GiB block), so no arena starts in the middle of a block. start.S zero-fills it
 * .frame       : エスケープしない own のフレームリージョン（1 MiB。bsp_main が fascon_frame_init で登録）
 *                the frame region for own values that never escape (1 MiB; registered by bsp_main with fascon_frame_init) */
ENTRY(_start)

SECTIONS
{
    . = 0x40080000;
    __kernel_start = .;
    .text : { KEEP(*(.text.boot)) *(.text .text.*) }
    __text_end = .;
    .rodata : ALIGN(16) { *(.rodata .rodata.*) }
    /* generic-os: ユーザーメモリの写し（LDTR / STTR）の例外表（命令の番地, 飛び先）/ the exception table of user copies */
    __ex_table : ALIGN(8) { __start___ex_table = .; KEEP(*(__ex_table)) __stop___ex_table = .; }
    .data : ALIGN(16) { __data_start = .; *(.data .data.*) }
    .fascon_arena (NOLOAD) : ALIGN(0x200000) {
        __arena_start = .;
        *(.bss.fascon_arena .bss.fascon_arena.*)
        . = ALIGN(16);
        __arena_end = .;
    }
    .bss (NOLOAD) : ALIGN(16) {
        __bss_start = .;
        /* generic-os のタスクスタック（守りページを置くので、起動時に 4 KiB のページへ分ける範囲）。ほかでは空
         * generic-os task stacks (split into 4 KiB pages at boot for guard pages); empty otherwise */
        __kstacks_start = .;
        *(.bss.fascon_kstacks)
        __kstacks_end = .;
        *(.bss .bss.*) *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }
    /* user_mem: タスクごとのユーザーメモリ（2 MiB 境界。起動時には 0 にしない） / per-task user memory (2 MiB aligned; not
     * zeroed at boot) */
    .fascon_user (NOLOAD) : ALIGN(0x200000) { *(.bss.fascon_user .bss.fascon_user.*) }
    /* マルチコア: コアごとの実体（__thread のアリーナ・フレームの管理・コアの番号）。.tbss はアドレスを取らないので、
     * コア 4 つぶんの置き場 .percpu を別に取り、BSP がコアごとに TPIDR_EL0 を向ける（.tdata は使わない）
     * multi-core: per-core instances (__thread arenas, frame bookkeeping, the core number). .tbss takes no address space,
     * so .percpu reserves room for four cores and the BSP points TPIDR_EL0 of each core into it (.tdata is not used) */
    .tdata : { *(.tdata .tdata.*) }
    .tbss (NOLOAD) : ALIGN(128) { *(.tbss .tbss.*) *(.tcommon) }
    ASSERT(SIZEOF(.tdata) == 0, "Fascon: initialized per-core data (.tdata) is not supported by this BSP")
    __tls_size = SIZEOF(.tbss);
    __tls_align = ALIGNOF(.tbss);
    . = ALIGN(4096);
    .percpu (NOLOAD) : { __percpu_start = .; . += 4 * ALIGN(SIZEOF(.tbss) + 4096, 4096); __percpu_end = .; }
    /* コア 1〜3 のスタック（128 KiB ずつ）とフレームリージョン（1 MiB ずつ） / stacks (128 KiB each) and frame regions
     * (1 MiB each) of cores 1 to 3 */
    . = ALIGN(4096);
    .smp_stack (NOLOAD) : { . += 0x20000 * 3; __smp_stack_top = .; }
    . = ALIGN(4096);
    .smp_frame (NOLOAD) : { __smp_frame_start = .; . += 0x100000 * 3; __smp_frame_end = .; }
    . = ALIGN(4096);
    .stack (NOLOAD) : { . += 0x20000; __stack_top = .; }
    . = ALIGN(4096);
    .frame (NOLOAD) : { __frame_start = .; . += 0x100000; __frame_end = .; }  /* own フレーム 1 MiB / own frame */
    . = ALIGN(4096);
    .heap (NOLOAD) : { __heap_start = .; . += 0x800000; __heap_end = .; }     /* own ヒープ 8 MiB / own heap */
    __kernel_end = .;
    ASSERT(__kernel_end <= 0x40000000 + 0x10000000, "Fascon: the kernel image, arenas and user memory do not fit in the 256 MiB of RAM")
    /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) }
}
