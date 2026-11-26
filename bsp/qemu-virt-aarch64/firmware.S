/*
 * firmware.S — Fascon 1.0 BSP: QEMU virt (AArch64) の「仮想 BIOS」（-bios の像。フラッシュの 0 番地から走る）
 *              Fascon 1.0 BSP: a "virtual BIOS" for QEMU virt (AArch64) (the -bios image; it runs from address 0 of flash)
 *
 * -bios で起動すると QEMU は像をフラッシュ（0x0000_0000〜）に置き、CPU0 は 0 番地から MMU オフで走り出す。ここでは
 * フラッシュの中のカーネル像（.text / .rodata / .data）を RAM の 0x4008_0000 へ写して、-kernel のときと同じ _start（start.S）へ跳ぶ。
 * With -bios QEMU puts the image in flash (from 0x0000_0000) and CPU0 starts at address 0 with the MMU off. This copies the
 * kernel image (.text / .rodata / .data) from flash to RAM at 0x4008_0000 and jumps to the same _start (start.S) as with -kernel.
 */
    .section .fwboot, "ax"
    .global _fw_reset
_fw_reset:
    ldr     x0, =__image_lma            /* フラッシュの中の像 / the image in flash */
    ldr     x1, =__image_start          /* RAM の 0x4008_0000 / RAM at 0x4008_0000 */
    ldr     x2, =__image_end
1:  cmp     x1, x2
    b.hs    2f
    ldp     x3, x4, [x0], #16           /* 両端は 16 バイト境界 / both ends are 16-byte aligned */
    stp     x3, x4, [x1], #16
    b       1b
2:  ldr     x0, =bsp_fw_boot           /* bsp.c に「仮想 BIOS から起動した」と伝える（.data は写し終えた） / tell bsp.c we came
                                           through the virtual BIOS (.data has been copied) */
    mov     w1, #1
    str     w1, [x0]
    dsb     sy
    ic      iallu                       /* 写したコードを命令側からも見えるように / make the copied code visible to fetch */
    dsb     sy
    isb
    ldr     x0, =_start
    br      x0
    .ltorg
