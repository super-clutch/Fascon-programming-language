/*
 * start.S — Fascon 1.0 BSP: QEMU virt (AArch64) のリセット直後のコード（BIOS・ファームウェア不要）
 *           Fascon 1.0 BSP: the first code after reset on QEMU virt (AArch64), no BIOS or firmware
 *
 * QEMU は ELF をそのまま読み込み、CPU0 をこの _start から（EL1、MMU オフで）走らせる。2 番目以降の CPU は
 * PSCI で電源オフのまま待機している。
 * QEMU loads the ELF as is and runs CPU0 from this _start (EL1, MMU off); secondary CPUs stay powered off under PSCI.
 *
 *   1. EL2 で起動した場合は EL1 へ降りる / drop to EL1 if started at EL2
 *   2. スタックを設定し、FP/SIMD を有効化（コンパイラの NEON 命令が例外にならないように）
 *      set up the stack and enable FP/SIMD (so NEON code emitted by the compiler does not trap)
 *   3. 例外ベクタを設定、.bss と .fascon_arena（アリーナ）をゼロ化して bsp_main へ
 *      install the vectors, zero .bss and .fascon_arena (the arenas), call bsp_main
 */
    .section .text.boot, "ax"
    .global _start
_start:
    mrs     x0, mpidr_el1
    and     x0, x0, #0xff
    cbnz    x0, park                    /* CPU0 以外は待機 / only CPU0 continues */
    mrs     x0, CurrentEL
    lsr     x0, x0, #2
    cmp     x0, #2
    b.ne    1f
    ldr     x1, =bsp_boot_el2           /* PSCI の呼び方（SMC）を bsp.c に伝える（.data なので 0 埋めで消えない） / tells bsp.c to use SMC for PSCI */
    mov     w2, #1
    str     w2, [x1]
    mov     x0, #(1 << 31)              /* HCR_EL2.RW = AArch64 */
    msr     hcr_el2, x0
    msr     cntvoff_el2, xzr            /* 仮想タイマーのずれ = 0 / virtual timer offset = 0 */
    mov     x0, #3                      /* CNTHCTL_EL2: EL1 に物理カウンタとタイマーを許可 / let EL1 use the physical counter and timer */
    msr     cnthctl_el2, x0
    mov     x0, #0x3c5                  /* EL1h, DAIF マスク / EL1h, DAIF masked */
    msr     spsr_el2, x0
    adr     x0, 1f
    msr     elr_el2, x0
    eret
1:
    ldr     x1, =__stack_top
    mov     sp, x1
    mov     x0, #(3 << 20)              /* CPACR_EL1.FPEN = 0b11 */
    msr     cpacr_el1, x0
    isb
    ldr     x0, =vectors
    msr     vbar_el1, x0
    isb
    ldr     x1, =__bss_start
    ldr     x2, =__bss_end
2:  cmp     x1, x2
    b.hs    3f
    stp     xzr, xzr, [x1], #16         /* 両端は 16 バイト境界 / both ends are 16-byte aligned */
    b       2b
3:  ldr     x1, =__arena_start          /* アリーナ（2 MiB 境界の .fascon_arena）も 0 に: 空のアリーナ / arenas too: empty */
    ldr     x2, =__arena_end
4:  cmp     x1, x2
    b.hs    5f
    stp     xzr, xzr, [x1], #16
    b       4b
5:  bl      bsp_main
park:
    wfe
    b       park

/* コア 1〜3 の入口（CPU0 が PSCI の CPU_ON で起動する。x0 = コアの番号）。EL2 なら EL1 へ降り、自分のスタックを設定して
 * C へ。MMU は C の中で、CPU0 が作ったのと同じ表（同じ TTBR0）で有効にする
 * the entry of cores 1 to 3 (started by CPU0 through PSCI CPU_ON; x0 = the core number): drop to EL1 when started at
 * EL2, set the own stack and go to C, which enables the MMU with the very tables (the same TTBR0) CPU0 built */
    .global _secondary_start
    .weak   fascon2_secondary_main          /* 1 コアのプログラムには無い（ここへは来ない） / absent in single-core programs (never reached) */
_secondary_start:
    mov     x19, x0
    mrs     x0, CurrentEL
    lsr     x0, x0, #2
    cmp     x0, #2
    b.ne    1f
    mov     x0, #(1 << 31)
    msr     hcr_el2, x0
    msr     cntvoff_el2, xzr
    mov     x0, #3
    msr     cnthctl_el2, x0
    mov     x0, #0x3c5
    msr     spsr_el2, x0
    adr     x0, 1f
    msr     elr_el2, x0
    eret
1:
    ldr     x1, =__smp_stack_top        /* コア n のスタックの上端 = 上端 - (n - 1) × 128 KiB / core n: top - (n - 1) * 128 KiB */
    sub     x2, x19, #1
    lsl     x2, x2, #17
    sub     x1, x1, x2
    mov     sp, x1
    mov     x0, #(3 << 20)
    msr     cpacr_el1, x0
    isb
    ldr     x0, =vectors
    msr     vbar_el1, x0
    isb
    mov     x0, x19
    bl      fascon2_secondary_main
    b       park

    .balign 2048
vectors:
    .rept 4                             /* Current EL with SP0 */
    .balign 128
    b       bad_exception
    .endr
    .balign 128                         /* Current EL with SPx: Synchronous */
    b       el1_sync
    .balign 128                         /* IRQ */
    b       el1_irq
    .balign 128                         /* FIQ */
    b       bad_exception
    .balign 128                         /* SError */
    b       bad_exception
    .balign 128                         /* Lower EL (AArch64): Synchronous = EL0 の svc・例外 / an svc or exception at EL0 */
    b       el0_sync
    .balign 128                         /* IRQ: EL0 の実行中の割り込み。フレームは同じ / an interrupt while at EL0; the same frame */
    b       el1_irq
    .balign 128                         /* FIQ */
    b       bad_exception
    .balign 128                         /* SError */
    b       bad_exception
    .rept 4                             /* Lower EL (AArch32) */
    .balign 128
    b       bad_exception
    .endr

/* ───── 保存フレーム f20_trapframe_t（800 バイト）: 割り込み・yield（EL1 の svc）に共通
 *       the saved frame f20_trapframe_t (800 bytes), shared by interrupts and yields (svc at EL1)
 *   [0]   x0..x30      [248] SP_EL0   [256] ELR_EL1   [264] SPSR_EL1   [272] FPSR   [280] FPCR   [288] q0..q31
 * フレームは割り込まれたタスクのカーネルスタックに積む。出口は「戻す先のフレームの番地」を C から受け取って復帰する
 * ので、タスクの切り替えは「別のタスクのフレームの番地を返す」だけ。
 * The frame is pushed on the interrupted task's kernel stack. The exit path takes the address of the frame to restore
 * from C, so switching tasks is just returning the address of another task's frame. */
    .macro SAVE_FRAME
    sub     sp, sp, #800
    stp     x0, x1, [sp, #0]
    stp     x2, x3, [sp, #16]
    stp     x4, x5, [sp, #32]
    stp     x6, x7, [sp, #48]
    stp     x8, x9, [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x19, [sp, #144]
    stp     x20, x21, [sp, #160]
    stp     x22, x23, [sp, #176]
    stp     x24, x25, [sp, #192]
    stp     x26, x27, [sp, #208]
    stp     x28, x29, [sp, #224]
    str     x30, [sp, #240]
    mrs     x0, sp_el0
    mrs     x1, elr_el1
    mrs     x2, spsr_el1
    str     x0, [sp, #248]
    stp     x1, x2, [sp, #256]
    mrs     x0, fpsr
    mrs     x1, fpcr
    stp     x0, x1, [sp, #272]
    stp     q0, q1, [sp, #288]
    stp     q2, q3, [sp, #320]
    stp     q4, q5, [sp, #352]
    stp     q6, q7, [sp, #384]
    stp     q8, q9, [sp, #416]
    stp     q10, q11, [sp, #448]
    stp     q12, q13, [sp, #480]
    stp     q14, q15, [sp, #512]
    stp     q16, q17, [sp, #544]
    stp     q18, q19, [sp, #576]
    stp     q20, q21, [sp, #608]
    stp     q22, q23, [sp, #640]
    stp     q24, q25, [sp, #672]
    stp     q26, q27, [sp, #704]
    stp     q28, q29, [sp, #736]
    stp     q30, q31, [sp, #768]
    .endm
    .macro RESTORE_FRAME
    ldp     q0, q1, [sp, #288]
    ldp     q2, q3, [sp, #320]
    ldp     q4, q5, [sp, #352]
    ldp     q6, q7, [sp, #384]
    ldp     q8, q9, [sp, #416]
    ldp     q10, q11, [sp, #448]
    ldp     q12, q13, [sp, #480]
    ldp     q14, q15, [sp, #512]
    ldp     q16, q17, [sp, #544]
    ldp     q18, q19, [sp, #576]
    ldp     q20, q21, [sp, #608]
    ldp     q22, q23, [sp, #640]
    ldp     q24, q25, [sp, #672]
    ldp     q26, q27, [sp, #704]
    ldp     q28, q29, [sp, #736]
    ldp     q30, q31, [sp, #768]
    ldp     x0, x1, [sp, #272]
    msr     fpsr, x0
    msr     fpcr, x1
    ldr     x0, [sp, #248]
    ldp     x1, x2, [sp, #256]
    msr     sp_el0, x0
    msr     elr_el1, x1
    msr     spsr_el1, x2
    ldp     x0, x1, [sp, #0]
    ldp     x2, x3, [sp, #16]
    ldp     x4, x5, [sp, #32]
    ldp     x6, x7, [sp, #48]
    ldp     x8, x9, [sp, #64]
    ldp     x10, x11, [sp, #80]
    ldp     x12, x13, [sp, #96]
    ldp     x14, x15, [sp, #112]
    ldp     x16, x17, [sp, #128]
    ldp     x18, x19, [sp, #144]
    ldp     x20, x21, [sp, #160]
    ldp     x22, x23, [sp, #176]
    ldp     x24, x25, [sp, #192]
    ldp     x26, x27, [sp, #208]
    ldp     x28, x29, [sp, #224]
    ldr     x30, [sp, #240]
    add     sp, sp, #800
    .endm

/* EL1 の同期例外: svc（タスクの切り替え）ならフレームを積んで C へ。それ以外は致命的
 * synchronous exception at EL1: an svc (task switch) pushes a frame and goes to C; anything else is fatal */
#if defined(FASCON2_GENERIC_OS)
/* generic-os: 保存フレームを積む前に、積む先（SP - 1 と SP - 800）が写っているかを AT で確かめる。守りページなら
 * コアごとの非常用スタックへ移って報告する（積もうとして二重に例外を起こさない）
 * generic-os: before the frame is pushed, AT checks that SP - 1 and SP - 800 are mapped; on a guard page, move to the
 * per-core emergency stack and report (instead of faulting again while pushing) */
    .macro STACK_CHECK
    msr     tpidr_el1, x0
    mov     x0, sp
    sub     x0, x0, #1
    at      s1e1w, x0
    isb
    mrs     x0, par_el1
    tbnz    x0, #0, el1_stack_overflow
    mov     x0, sp
    sub     x0, x0, #800
    at      s1e1w, x0
    isb
    mrs     x0, par_el1
    tbnz    x0, #0, el1_stack_overflow
    mrs     x0, tpidr_el1
    .endm
#endif

el1_sync:
#if defined(FASCON2_GENERIC_OS)
    STACK_CHECK
#endif
    SAVE_FRAME
    mrs     x0, esr_el1
    lsr     x1, x0, #26
    cmp     x1, #0x15                   /* EC = SVC (AArch64) */
    b.ne    el1_fault                   /* 例外表・要求時ページング・致命的な例外は C で / fixups, demand paging, fatal: in C */
    mov     x0, sp
    bl      bsp_el1_svc                 /* 戻り値 = 復帰するフレーム / returns the frame to restore */
    mov     sp, x0
    RESTORE_FRAME
    eret

/* IRQ: フレームを積んで C のディスパッチへ（GIC の ACK・ハンドラ・EOI・切り替えの判定）
 * IRQ: push a frame and dispatch in C (GIC acknowledge, the handler, end of interrupt, the switch decision) */
el1_irq:
#if defined(FASCON2_GENERIC_OS)
    STACK_CHECK
#endif
    SAVE_FRAME
    mov     x0, sp
    bl      bsp_irq
    mov     sp, x0
    RESTORE_FRAME
    eret

/* EL1 の SVC 以外の同期例外: 例外表の飛び先・ユーザーページの要求時の写し（どのプロファイルでも）、generic-os では
 * アリーナの要求時の写し・守り・BTI・PAC の報告（C が戻すフレームで復帰）
 * a non-SVC synchronous exception at EL1: exception-table fixups and on-demand user pages (every profile); on generic-os
 * also demand mapping of arenas and guard / BTI / PAC reports (resumes with the frame C returns) */
el1_fault:
    mov     x0, sp
    bl      bsp_el1_fault
    mov     sp, x0
    RESTORE_FRAME
    eret

#if defined(FASCON2_GENERIC_OS)
el1_stack_overflow:
    mov     x2, sp
    mrs     x1, mpidr_el1
    and     x1, x1, #3
    add     x1, x1, #1
    lsl     x1, x1, #12
    ldr     x0, =bsp_emerg_stack
    add     x0, x0, x1
    mov     sp, x0
    mrs     x0, far_el1
    mrs     x1, elr_el1
    bl      bsp_stack_overflow
    b       park
#endif

/* EL0 からの同期例外: システムコール（svc）か、そのタスクの不正な動作。フレームはそのタスクのカーネルスタックの一番上
 * a synchronous exception from EL0: a system call (svc) or a fault of that task; the frame sits at the very top of the
 * task's kernel stack */
el0_sync:
    SAVE_FRAME
    mov     x0, sp
    bl      bsp_el0_sync                /* 戻り値 = 復帰するフレーム / returns the frame to restore */
    mov     sp, x0
    RESTORE_FRAME
    eret

sync_exception:
    mrs     x0, esr_el1
    mrs     x1, elr_el1
    mrs     x2, far_el1
    bl      bsp_sync_exception
    b       park
bad_exception:
    mov     x0, #0
    mov     x1, #0
    mov     x2, #0
    bl      bsp_sync_exception
    b       park
