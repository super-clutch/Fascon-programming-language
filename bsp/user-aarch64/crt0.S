/*
 * crt0.S — Fascon 1.0 ユーザープログラム（EL0）の入口 / the entry of a Fascon 1.0 user program (EL0)
 *
 * カーネルの `@task.start_user(値, 入口, スタック)` が、全レジスタ 0・SP = スタックの番地でここへ入る。.bss は
 * カーネルがユーザーメモリごと 0 にしてあるので、ここでは何もしない。
 * The kernel's `@task.start_user(value, entry, stack)` enters here with every register zero and SP at the stack address.
 * The kernel has zeroed the whole user memory, .bss included, so nothing is cleared here.
 */
    .section .text.boot, "ax"
    .global _start
_start:
    mov     x29, #0
    mov     x30, #0
    bl      fascon_user_start
1:  udf     #0                          /* 戻らない / never reached */
    b       1b
