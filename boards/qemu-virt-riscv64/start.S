/* start.S — QEMU virt RISC-V 64 のボードのパッケージ（M モード）。全ハートがここへ来る。ハート 0 以外は WFI で IPI を待つ
 * start.S — the QEMU virt RISC-V 64 board package (M-mode); every hart arrives here, harts other than 0 wait for an IPI */
#include "fascon_board.h"
    .section .text.boot, "ax"
    .global _start
_start:
    csrr    t0, mhartid
    la      sp, __stack_top
    li      t1, FASCON_BOARD_STACK_SIZE
    mul     t2, t1, t0
    sub     sp, sp, t2
    la      t3, bsp_trap
    csrw    mtvec, t3
    bnez    t0, 2f
    la      t3, __bss_start
    la      t4, __bss_end
1:  bgeu    t3, t4, 3f
    sd      zero, 0(t3)
    addi    t3, t3, 8
    j       1b
3:  call    bsp_main
    j       park
2:  li      t5, 8
    csrs    mie, t5             /* MSIE: ソフトウェア割り込み（IPI）で WFI から覚める / wake from WFI on a software interrupt */
4:  wfi
    csrr    t6, mip
    andi    t6, t6, 8
    beqz    t6, 4b
    mv      a0, t0
    call    bsp_secondary_main
park:
    wfi
    j       park

    .balign 4
bsp_trap:
    csrr    a0, mcause
    csrr    a1, mepc
    csrr    a2, mtval
    call    bsp_exception
    j       park
