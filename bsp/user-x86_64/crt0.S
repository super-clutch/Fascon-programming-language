/*
 * crt0.S — Fascon 1.0 ユーザープログラム（x86_64 の Ring 3）の入口 / the entry of a Fascon 1.0 user program (x86_64 ring 3)
 *
 * カーネルの `@task.start_user(値, 入口, スタック)` が、全レジスタ 0・RSP = スタックの番地でここへ入る（iretq で Ring 3 へ）。
 * .bss はカーネルがユーザーメモリごと 0 にしてあるので、ここでは何もしない。コードは位置独立（USER_BASE は 4 GiB 以上）
 * The kernel `@task.start_user(value, entry, stack)` enters here with every register zero and RSP at the stack address
 * (iretq to ring 3). The kernel has zeroed the whole user memory, .bss included, so nothing is cleared here. The code is
 * position independent (USER_BASE lies at or above 4 GiB)
 */
    .section .text.boot, "ax"
    .global _start
_start:
    xorl    %ebp, %ebp
    andq    $-16, %rsp                  /* 呼び出しの前は 16 バイト境界 / 16-byte aligned before the call */
    callq   fascon_user_start
1:  ud2                                 /* 戻らない / never reached */
    jmp     1b
