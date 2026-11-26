/* start.S — 汎用 AArch64 BSP（board.toml から作る実機の像）の入口。全コアがここへ来る
 * start.S — the entry of the generic AArch64 BSP (real-hardware images made from board.toml); every core comes here
 *
 *   1. 起動時の例外レベル（EL3 / EL2 / EL1）を見て EL1 へ降りる（ファームウェアの有無を問わない）
 *      drop to EL1 from whatever level we were started in (EL3 / EL2 / EL1; with or without firmware)
 *   2. コアごとのスタック（__stack_top - コア番号 × FASCON_BOARD_STACK_SIZE）/ a per-core stack
 *   3. コア 0: .bss を 0 にして bsp_main へ。ほかのコア: bsp_secondary_main（スピンテーブル・PSCI で起こされた後）
 *      core 0: zero .bss and enter bsp_main; other cores: bsp_secondary_main (after a spin-table / PSCI release) */
#include "fascon_board.h"

    .section .text.boot, "ax"
    .global _start
_start:
    mrs     x19, mpidr_el1
    and     x19, x19, #0xff
    mrs     x0, CurrentEL
    lsr     x0, x0, #2
    cmp     x0, #3
    b.ne    1f
    /* EL3 → EL1: 下位は AArch64・非セキュア。EL2 があっても EL1 は AArch64 / lower levels AArch64, non-secure */
    mov     x0, #((1 << 10) | (1 << 0))
    msr     scr_el3, x0
    mov     x0, #(1 << 31)
    msr     hcr_el2, x0
    mov     x0, #0x3c5
    msr     spsr_el3, x0
    adr     x0, el1_entry
    msr     elr_el3, x0
    eret
1:  cmp     x0, #2
    b.ne    el1_entry
    /* EL2 → EL1: HCR_EL2.RW = 1、EL1 から物理カウンタとタイマーを使えるように / let EL1 use the physical counter and timer */
    mov     x0, #(1 << 31)
    msr     hcr_el2, x0
    mrs     x0, cnthctl_el2
    orr     x0, x0, #3
    msr     cnthctl_el2, x0
    msr     cntvoff_el2, xzr
    mov     x0, #0x3c5
    msr     spsr_el2, x0
    adr     x0, el1_entry
    msr     elr_el2, x0
    eret
el1_entry:
    mov     x0, #(3 << 20)              /* CPACR_EL1.FPEN: FP / SIMD を使える / allow FP / SIMD */
    msr     cpacr_el1, x0
    isb
    ldr     x1, =__stack_top
    ldr     x2, =FASCON_BOARD_STACK_SIZE
    mul     x3, x2, x19
    sub     x1, x1, x3
    mov     sp, x1
    cbnz    x19, 3f
    ldr     x0, =__bss_start
    ldr     x1, =__bss_end
2:  cmp     x0, x1
    b.hs    4f
    str     xzr, [x0], #8
    b       2b
4:  bl      bsp_main
    b       park
3:  mov     x0, x19
    bl      bsp_secondary_main
park:
    wfe
    b       park

/* 例外: 報告して止まる（この BSP は割り込みを使わない。割り込み・タスク・EL0 は qemu-virt BSP を手本に足す）
 * exceptions: report and stop (this BSP does not use interrupts; add them following the qemu-virt BSP) */
    .balign 2048
    .global bsp_vectors
bsp_vectors:
    .rept 16
    .balign 128
    mrs     x0, esr_el1
    mrs     x1, elr_el1
    mrs     x2, far_el1
    bl      bsp_exception
    b       park
    .endr
