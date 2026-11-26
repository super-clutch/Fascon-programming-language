/* firmware.ld — Fascon 1.0 BSP: QEMU virt AArch64 の仮想 BIOS（-bios の像） / the virtual BIOS for QEMU virt AArch64 (-bios image)
 *   フラッシュ 0x0: [firmware.S の起動コード][カーネル像（RAM の 0x4008_0000 へ写す）]
 *   flash 0x0: [firmware.S boot code][kernel image (copied to RAM at 0x4008_0000)]
 *   RAM の配置は kernel.ld と同じ / the RAM layout is the same as kernel.ld */
ENTRY(_fw_reset)
FLASH_IMAGE = 0x1000;

SECTIONS
{
    .fwboot 0x0 : AT(0x0) { KEEP(*(.fwboot)) }
    . = 0x40080000;
    __kernel_start = .;
    __image_start = .;
    .text : AT(FLASH_IMAGE) { KEEP(*(.text.boot)) *(.text .text.*) }
    .rodata : AT(FLASH_IMAGE + (ADDR(.rodata) - 0x40080000)) { *(.rodata .rodata.*) }
    .data : AT(FLASH_IMAGE + (ADDR(.data) - 0x40080000)) { *(.data .data.*) . = ALIGN(16); }
    __image_end = .;
    __image_lma = FLASH_IMAGE;
    .fascon_arena (NOLOAD) : ALIGN(0x200000) {
        __arena_start = .;
        *(.bss.fascon_arena .bss.fascon_arena.*)
        . = ALIGN(16);
        __arena_end = .;
    }
    .bss (NOLOAD) : ALIGN(16) {
        __bss_start = .;
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
    .frame (NOLOAD) : { __frame_start = .; . += 0x100000; __frame_end = .; }
    . = ALIGN(4096);
    .heap (NOLOAD) : { __heap_start = .; . += 0x800000; __heap_end = .; }
    __kernel_end = .;
    ASSERT(__kernel_end <= 0x40000000 + 0x10000000, "Fascon: the kernel image, arenas and user memory do not fit in the 256 MiB of RAM")
    /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) }
}
