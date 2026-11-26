/*
 * bios.S — Fascon 1.0 BSP: QEMU x86_64 の「仮想 BIOS」（SeaBIOS も UEFI も使わない。QEMU の -bios にこの像を渡す）
 *          Fascon 1.0 BSP: a "virtual BIOS" for QEMU x86_64 (no SeaBIOS, no UEFI; QEMU loads this image with -bios)
 *
 * QEMU は -bios の像を 4 GiB の直下に置き、CPU はリアルモードで 0xFFFF_FFF0（リセットベクタ）から走り出す。
 * QEMU maps the -bios image right below 4 GiB, and the CPU starts in real mode at 0xFFFF_FFF0 (the reset vector).
 *
 *   1. リセットベクタ（16 ビット）: GDT を読み込んでプロテクトモードへ / reset vector (16-bit): load the GDT, enter protected mode
 *   2. 32 ビット: ページ表を作る（0〜4 GiB の恒等マップ、2 MiB ページだけ。3〜4 GiB はキャッシュ無効）→ PAE・EFER.LME・
 *      ページングを有効化してロングモードへ。キャッシュを有効化（CR0.CD / NW を下ろす）、SSE を有効化
 *      32-bit: build the page tables (identity map of 0-4 GiB with 2 MiB pages only; 3-4 GiB uncached) → enable PAE,
 *      EFER.LME and paging to enter long mode; enable the caches (clear CR0.CD / NW) and SSE
 *   3. 64 ビット: ROM の中のカーネル像（.text / .rodata / .data）を RAM の 1 MiB へ写し、.bss とアリーナ（.fascon_arena）を
 *      0 で埋め、スタックを置いて bsp_main（C）へ
 *      64-bit: copy the kernel image (.text / .rodata / .data) from the ROM to RAM at 1 MiB, zero .bss and the arenas
 *      (.fascon_arena), set the stack and call bsp_main (C)
 *
 * ページ表は 0x1000〜0x6FFF（PML4・PDPT・PD × 4）。4 KiB のページはどこにもなく、表は起動時に 1 回書くだけ
 * the page tables live at 0x1000-0x6FFF (PML4, PDPT, 4 PDs); there are no 4 KiB pages anywhere and the tables are
 * written once at boot
 */

#define PML4 0x1000
#define PDPT 0x2000
#define PD0 0x3000

/* ───────────── 16 ビット: リセットベクタの先（ROM の最後の 4 KiB） / 16-bit: after the reset vector (the last 4 KiB) ───────────── */
    .section .rom16, "ax"
    .code16
rom16_start:
rom16_entry:
    cli
    cld
    /* CS の隠れた土台は 0xFFFF0000。GDT の擬似記述子は CS 相対で読む / CS's hidden base is 0xFFFF0000: read the GDT
       pseudo-descriptor relative to CS */
    lgdtl   %cs:(0xF000 + (gdt_ptr - rom16_start))
    movl    %cr0, %eax
    orl     $1, %eax                    /* PE */
    movl    %eax, %cr0
    ljmpl   $0x08, $pm32_entry

    .balign 16
gdt:
    .quad   0x0000000000000000          /* 0x00: null */
    .quad   0x00CF9A000000FFFF          /* 0x08: 32 ビットのコード / 32-bit code, base 0, limit 4 GiB */
    .quad   0x00CF92000000FFFF          /* 0x10: データ / data, base 0, limit 4 GiB */
    .quad   0x00AF9A000000FFFF          /* 0x18: 64 ビットのコード（L = 1） / 64-bit code (L = 1) */
gdt_end:
gdt_ptr:
    .word   gdt_end - gdt - 1
    .long   gdt

/* ───────────── リセットベクタ（0xFFFF_FFF0） / the reset vector (0xFFFF_FFF0) ───────────── */
    .section .reset, "ax"
    .code16
    .global reset_vector
reset_vector:
    jmp     rom16_entry
    .balign 16, 0xF4                    /* 残りは hlt / the rest is hlt */

/* ───────────── 32 ビット: ページ表 → ロングモード / 32-bit: page tables → long mode ───────────── */
    .section .rom32, "ax"
    .code32
pm32_entry:
    movw    $0x10, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %ss
    movw    %ax, %fs
    movw    %ax, %gs
    /* 表の領域を 0 で埋める（0x1000〜0x6FFF） / zero the table area (0x1000-0x6FFF) */
    movl    $PML4, %edi
    xorl    %eax, %eax
    movl    $(0x6000 / 4), %ecx
    rep stosl
    movl    $((PDPT) | 3), PML4           /* PML4[0] → PDPT（P | RW） */
    movl    $((PD0 + 0x0000) | 3), PDPT + 0
    movl    $((PD0 + 0x1000) | 3), PDPT + 8
    movl    $((PD0 + 0x2000) | 3), PDPT + 16
    movl    $((PD0 + 0x3000) | 3), PDPT + 24
    /* PD: 2048 個の 2 MiB ページ（P | RW | PS）。3〜4 GiB（MMIO・ROM）は PCD | PWT
       PD: 2048 pages of 2 MiB (P | RW | PS); 3-4 GiB (MMIO, ROM) gets PCD | PWT */
    movl    $PD0, %edi
    xorl    %ecx, %ecx
1:  movl    %ecx, %eax
    shll    $21, %eax
    orl     $0x83, %eax
    cmpl    $1536, %ecx
    jb      2f
    orl     $0x18, %eax
2:  movl    %eax, (%edi)
    movl    $0, 4(%edi)
    addl    $8, %edi
    incl    %ecx
    cmpl    $2048, %ecx
    jb      1b
    /* CR4 = PAE | OSFXSR | OSXMMEXCPT（SSE を使えるように） / CR4 = PAE | OSFXSR | OSXMMEXCPT (so SSE works) */
    movl    $0x620, %eax
    movl    %eax, %cr4
    movl    $PML4, %eax
    movl    %eax, %cr3
    movl    $0xC0000080, %ecx           /* IA32_EFER */
    rdmsr
    orl     $0x100, %eax                /* LME */
    wrmsr
    movl    %cr0, %eax
    andl    $0x9FFFFFFB, %eax           /* CD・NW・EM を下ろす（キャッシュ有効・FPU あり） / clear CD, NW, EM */
    orl     $0x80000023, %eax           /* PG | NE | MP | PE */
    movl    %eax, %cr0
    ljmpl   $0x18, $lm64_entry

/* ───────────── 64 ビット: カーネル像を RAM へ / 64-bit: the kernel image to RAM ───────────── */
    .code64
lm64_entry:
    movw    $0x10, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %ss
    xorw    %ax, %ax
    movw    %ax, %fs
    movw    %ax, %gs
    fninit
    movl    $__image_lma, %esi          /* ROM の中の像 / the image inside the ROM */
    movl    $__image_start, %edi        /* RAM の 1 MiB / RAM at 1 MiB */
    movl    $__image_size, %ecx
    rep movsb
    movl    $__bss_start, %edi          /* .bss を 0 に / zero .bss */
    movl    $__bss_end, %ecx
    subl    %edi, %ecx
    xorl    %eax, %eax
    rep stosb
    movl    $__arena_start, %edi        /* アリーナも 0 に = 空のアリーナ / the arenas too = empty arenas */
    movl    $__arena_end, %ecx
    subl    %edi, %ecx
    rep stosb
    movl    $__stack_top, %esp          /* 上位 32 ビットは 0 になる / the upper 32 bits become 0 */
    xorl    %ebp, %ebp
    movl    $bsp_main, %eax             /* RAM の C へ（ROM から 2 GiB 以上離れているので間接呼び出し） / into C in RAM
                                           (more than 2 GiB away from the ROM, so an indirect call) */
    callq   *%rax
3:  cli
    hlt
    jmp     3b

/* ───────────── 割り込み・例外の入口（RAM の .text） / interrupt and exception entry points (.text in RAM) ─────────────
 * 256 個のベクタ。エラーコードのないものは 0 を積んで形をそろえ、汎用レジスタ 15 本と FXSAVE（512 バイト）を積んだ保存
 * フレーム（688 バイト）の番地を bsp_x86_trap（C）へ渡す。C が返したフレームから戻る（タスクの切り替え = 別のタスクの
 * フレームの番地を返すだけ。AArch64 の BSP と同じ形）
 * all 256 vectors. Those without an error code push 0 so every frame looks alike; the address of the saved frame (688
 * bytes: 15 general registers plus FXSAVE, 512 bytes) goes to bsp_x86_trap (C), and the exit path resumes from the frame
 * C returns (switching tasks = returning the frame of another task; the same shape as the AArch64 BSP)
 *   [fx 512][r15 .. rax: 15 x 8][vector][error][rip cs rflags rsp ss] = 688 バイト / bytes
 *
 * Ring 3 から入ったときは、ハードウェアのフレームのすぐ上（RSP0 / IST のスタックの一番上の 8 バイト）にこのコアの TLS の
 * 土台（FS の土台）が置いてある。C へ入る前に FS の土台をそれに戻す（ユーザーが FS を書き換えても、カーネルはコアごとの
 * 実体を正しく引ける）。Ring 0 からの割り込みでは FS はもうカーネルのもの
 * on entry from ring 3, the qword right above the hardware frame (the top 8 bytes of the RSP0 / IST stack) holds the
 * TLS base of this core (the FS base). It is put back into FS before entering C, so the kernel finds its per-core instances
 * even when user code rewrote FS. On interrupts from ring 0, FS already belongs to the kernel */
    .text
    .code64
isr0:
    pushq   $0
    pushq   $0
    jmp     isr_full
isr1:
    pushq   $0
    pushq   $1
    jmp     isr_full
isr2:
    pushq   $0
    pushq   $2
    jmp     isr_full
isr3:
    pushq   $0
    pushq   $3
    jmp     isr_full
isr4:
    pushq   $0
    pushq   $4
    jmp     isr_full
isr5:
    pushq   $0
    pushq   $5
    jmp     isr_full
isr6:
    pushq   $0
    pushq   $6
    jmp     isr_full
isr7:
    pushq   $0
    pushq   $7
    jmp     isr_full
isr8:
    pushq   $8
    jmp     isr_full
isr9:
    pushq   $0
    pushq   $9
    jmp     isr_full
isr10:
    pushq   $10
    jmp     isr_full
isr11:
    pushq   $11
    jmp     isr_full
isr12:
    pushq   $12
    jmp     isr_full
isr13:
    pushq   $13
    jmp     isr_full
isr14:
    pushq   $14
    jmp     isr_full
isr15:
    pushq   $0
    pushq   $15
    jmp     isr_full
isr16:
    pushq   $0
    pushq   $16
    jmp     isr_full
isr17:
    pushq   $17
    jmp     isr_full
isr18:
    pushq   $0
    pushq   $18
    jmp     isr_full
isr19:
    pushq   $0
    pushq   $19
    jmp     isr_full
isr20:
    pushq   $0
    pushq   $20
    jmp     isr_full
isr21:
    pushq   $21
    jmp     isr_full
isr22:
    pushq   $0
    pushq   $22
    jmp     isr_full
isr23:
    pushq   $0
    pushq   $23
    jmp     isr_full
isr24:
    pushq   $0
    pushq   $24
    jmp     isr_full
isr25:
    pushq   $0
    pushq   $25
    jmp     isr_full
isr26:
    pushq   $0
    pushq   $26
    jmp     isr_full
isr27:
    pushq   $0
    pushq   $27
    jmp     isr_full
isr28:
    pushq   $0
    pushq   $28
    jmp     isr_full
isr29:
    pushq   $29
    jmp     isr_full
isr30:
    pushq   $30
    jmp     isr_full
isr31:
    pushq   $0
    pushq   $31
    jmp     isr_full
isr32:
    pushq   $0
    pushq   $32
    jmp     isr_full
isr33:
    pushq   $0
    pushq   $33
    jmp     isr_full
isr34:
    pushq   $0
    pushq   $34
    jmp     isr_full
isr35:
    pushq   $0
    pushq   $35
    jmp     isr_full
isr36:
    pushq   $0
    pushq   $36
    jmp     isr_full
isr37:
    pushq   $0
    pushq   $37
    jmp     isr_full
isr38:
    pushq   $0
    pushq   $38
    jmp     isr_full
isr39:
    pushq   $0
    pushq   $39
    jmp     isr_full
isr40:
    pushq   $0
    pushq   $40
    jmp     isr_full
isr41:
    pushq   $0
    pushq   $41
    jmp     isr_full
isr42:
    pushq   $0
    pushq   $42
    jmp     isr_full
isr43:
    pushq   $0
    pushq   $43
    jmp     isr_full
isr44:
    pushq   $0
    pushq   $44
    jmp     isr_full
isr45:
    pushq   $0
    pushq   $45
    jmp     isr_full
isr46:
    pushq   $0
    pushq   $46
    jmp     isr_full
isr47:
    pushq   $0
    pushq   $47
    jmp     isr_full
isr48:
    pushq   $0
    pushq   $48
    jmp     isr_full
isr49:
    pushq   $0
    pushq   $49
    jmp     isr_full
isr50:
    pushq   $0
    pushq   $50
    jmp     isr_full
isr51:
    pushq   $0
    pushq   $51
    jmp     isr_full
isr52:
    pushq   $0
    pushq   $52
    jmp     isr_full
isr53:
    pushq   $0
    pushq   $53
    jmp     isr_full
isr54:
    pushq   $0
    pushq   $54
    jmp     isr_full
isr55:
    pushq   $0
    pushq   $55
    jmp     isr_full
isr56:
    pushq   $0
    pushq   $56
    jmp     isr_full
isr57:
    pushq   $0
    pushq   $57
    jmp     isr_full
isr58:
    pushq   $0
    pushq   $58
    jmp     isr_full
isr59:
    pushq   $0
    pushq   $59
    jmp     isr_full
isr60:
    pushq   $0
    pushq   $60
    jmp     isr_full
isr61:
    pushq   $0
    pushq   $61
    jmp     isr_full
isr62:
    pushq   $0
    pushq   $62
    jmp     isr_full
isr63:
    pushq   $0
    pushq   $63
    jmp     isr_full
isr64:
    pushq   $0
    pushq   $64
    jmp     isr_full
isr65:
    pushq   $0
    pushq   $65
    jmp     isr_full
isr66:
    pushq   $0
    pushq   $66
    jmp     isr_full
isr67:
    pushq   $0
    pushq   $67
    jmp     isr_full
isr68:
    pushq   $0
    pushq   $68
    jmp     isr_full
isr69:
    pushq   $0
    pushq   $69
    jmp     isr_full
isr70:
    pushq   $0
    pushq   $70
    jmp     isr_full
isr71:
    pushq   $0
    pushq   $71
    jmp     isr_full
isr72:
    pushq   $0
    pushq   $72
    jmp     isr_full
isr73:
    pushq   $0
    pushq   $73
    jmp     isr_full
isr74:
    pushq   $0
    pushq   $74
    jmp     isr_full
isr75:
    pushq   $0
    pushq   $75
    jmp     isr_full
isr76:
    pushq   $0
    pushq   $76
    jmp     isr_full
isr77:
    pushq   $0
    pushq   $77
    jmp     isr_full
isr78:
    pushq   $0
    pushq   $78
    jmp     isr_full
isr79:
    pushq   $0
    pushq   $79
    jmp     isr_full
isr80:
    pushq   $0
    pushq   $80
    jmp     isr_full
isr81:
    pushq   $0
    pushq   $81
    jmp     isr_full
isr82:
    pushq   $0
    pushq   $82
    jmp     isr_full
isr83:
    pushq   $0
    pushq   $83
    jmp     isr_full
isr84:
    pushq   $0
    pushq   $84
    jmp     isr_full
isr85:
    pushq   $0
    pushq   $85
    jmp     isr_full
isr86:
    pushq   $0
    pushq   $86
    jmp     isr_full
isr87:
    pushq   $0
    pushq   $87
    jmp     isr_full
isr88:
    pushq   $0
    pushq   $88
    jmp     isr_full
isr89:
    pushq   $0
    pushq   $89
    jmp     isr_full
isr90:
    pushq   $0
    pushq   $90
    jmp     isr_full
isr91:
    pushq   $0
    pushq   $91
    jmp     isr_full
isr92:
    pushq   $0
    pushq   $92
    jmp     isr_full
isr93:
    pushq   $0
    pushq   $93
    jmp     isr_full
isr94:
    pushq   $0
    pushq   $94
    jmp     isr_full
isr95:
    pushq   $0
    pushq   $95
    jmp     isr_full
isr96:
    pushq   $0
    pushq   $96
    jmp     isr_full
isr97:
    pushq   $0
    pushq   $97
    jmp     isr_full
isr98:
    pushq   $0
    pushq   $98
    jmp     isr_full
isr99:
    pushq   $0
    pushq   $99
    jmp     isr_full
isr100:
    pushq   $0
    pushq   $100
    jmp     isr_full
isr101:
    pushq   $0
    pushq   $101
    jmp     isr_full
isr102:
    pushq   $0
    pushq   $102
    jmp     isr_full
isr103:
    pushq   $0
    pushq   $103
    jmp     isr_full
isr104:
    pushq   $0
    pushq   $104
    jmp     isr_full
isr105:
    pushq   $0
    pushq   $105
    jmp     isr_full
isr106:
    pushq   $0
    pushq   $106
    jmp     isr_full
isr107:
    pushq   $0
    pushq   $107
    jmp     isr_full
isr108:
    pushq   $0
    pushq   $108
    jmp     isr_full
isr109:
    pushq   $0
    pushq   $109
    jmp     isr_full
isr110:
    pushq   $0
    pushq   $110
    jmp     isr_full
isr111:
    pushq   $0
    pushq   $111
    jmp     isr_full
isr112:
    pushq   $0
    pushq   $112
    jmp     isr_full
isr113:
    pushq   $0
    pushq   $113
    jmp     isr_full
isr114:
    pushq   $0
    pushq   $114
    jmp     isr_full
isr115:
    pushq   $0
    pushq   $115
    jmp     isr_full
isr116:
    pushq   $0
    pushq   $116
    jmp     isr_full
isr117:
    pushq   $0
    pushq   $117
    jmp     isr_full
isr118:
    pushq   $0
    pushq   $118
    jmp     isr_full
isr119:
    pushq   $0
    pushq   $119
    jmp     isr_full
isr120:
    pushq   $0
    pushq   $120
    jmp     isr_full
isr121:
    pushq   $0
    pushq   $121
    jmp     isr_full
isr122:
    pushq   $0
    pushq   $122
    jmp     isr_full
isr123:
    pushq   $0
    pushq   $123
    jmp     isr_full
isr124:
    pushq   $0
    pushq   $124
    jmp     isr_full
isr125:
    pushq   $0
    pushq   $125
    jmp     isr_full
isr126:
    pushq   $0
    pushq   $126
    jmp     isr_full
isr127:
    pushq   $0
    pushq   $127
    jmp     isr_full
isr128:
    pushq   $0
    pushq   $128
    jmp     isr_full
isr129:
    pushq   $0
    pushq   $129
    jmp     isr_full
isr130:
    pushq   $0
    pushq   $130
    jmp     isr_full
isr131:
    pushq   $0
    pushq   $131
    jmp     isr_full
isr132:
    pushq   $0
    pushq   $132
    jmp     isr_full
isr133:
    pushq   $0
    pushq   $133
    jmp     isr_full
isr134:
    pushq   $0
    pushq   $134
    jmp     isr_full
isr135:
    pushq   $0
    pushq   $135
    jmp     isr_full
isr136:
    pushq   $0
    pushq   $136
    jmp     isr_full
isr137:
    pushq   $0
    pushq   $137
    jmp     isr_full
isr138:
    pushq   $0
    pushq   $138
    jmp     isr_full
isr139:
    pushq   $0
    pushq   $139
    jmp     isr_full
isr140:
    pushq   $0
    pushq   $140
    jmp     isr_full
isr141:
    pushq   $0
    pushq   $141
    jmp     isr_full
isr142:
    pushq   $0
    pushq   $142
    jmp     isr_full
isr143:
    pushq   $0
    pushq   $143
    jmp     isr_full
isr144:
    pushq   $0
    pushq   $144
    jmp     isr_full
isr145:
    pushq   $0
    pushq   $145
    jmp     isr_full
isr146:
    pushq   $0
    pushq   $146
    jmp     isr_full
isr147:
    pushq   $0
    pushq   $147
    jmp     isr_full
isr148:
    pushq   $0
    pushq   $148
    jmp     isr_full
isr149:
    pushq   $0
    pushq   $149
    jmp     isr_full
isr150:
    pushq   $0
    pushq   $150
    jmp     isr_full
isr151:
    pushq   $0
    pushq   $151
    jmp     isr_full
isr152:
    pushq   $0
    pushq   $152
    jmp     isr_full
isr153:
    pushq   $0
    pushq   $153
    jmp     isr_full
isr154:
    pushq   $0
    pushq   $154
    jmp     isr_full
isr155:
    pushq   $0
    pushq   $155
    jmp     isr_full
isr156:
    pushq   $0
    pushq   $156
    jmp     isr_full
isr157:
    pushq   $0
    pushq   $157
    jmp     isr_full
isr158:
    pushq   $0
    pushq   $158
    jmp     isr_full
isr159:
    pushq   $0
    pushq   $159
    jmp     isr_full
isr160:
    pushq   $0
    pushq   $160
    jmp     isr_full
isr161:
    pushq   $0
    pushq   $161
    jmp     isr_full
isr162:
    pushq   $0
    pushq   $162
    jmp     isr_full
isr163:
    pushq   $0
    pushq   $163
    jmp     isr_full
isr164:
    pushq   $0
    pushq   $164
    jmp     isr_full
isr165:
    pushq   $0
    pushq   $165
    jmp     isr_full
isr166:
    pushq   $0
    pushq   $166
    jmp     isr_full
isr167:
    pushq   $0
    pushq   $167
    jmp     isr_full
isr168:
    pushq   $0
    pushq   $168
    jmp     isr_full
isr169:
    pushq   $0
    pushq   $169
    jmp     isr_full
isr170:
    pushq   $0
    pushq   $170
    jmp     isr_full
isr171:
    pushq   $0
    pushq   $171
    jmp     isr_full
isr172:
    pushq   $0
    pushq   $172
    jmp     isr_full
isr173:
    pushq   $0
    pushq   $173
    jmp     isr_full
isr174:
    pushq   $0
    pushq   $174
    jmp     isr_full
isr175:
    pushq   $0
    pushq   $175
    jmp     isr_full
isr176:
    pushq   $0
    pushq   $176
    jmp     isr_full
isr177:
    pushq   $0
    pushq   $177
    jmp     isr_full
isr178:
    pushq   $0
    pushq   $178
    jmp     isr_full
isr179:
    pushq   $0
    pushq   $179
    jmp     isr_full
isr180:
    pushq   $0
    pushq   $180
    jmp     isr_full
isr181:
    pushq   $0
    pushq   $181
    jmp     isr_full
isr182:
    pushq   $0
    pushq   $182
    jmp     isr_full
isr183:
    pushq   $0
    pushq   $183
    jmp     isr_full
isr184:
    pushq   $0
    pushq   $184
    jmp     isr_full
isr185:
    pushq   $0
    pushq   $185
    jmp     isr_full
isr186:
    pushq   $0
    pushq   $186
    jmp     isr_full
isr187:
    pushq   $0
    pushq   $187
    jmp     isr_full
isr188:
    pushq   $0
    pushq   $188
    jmp     isr_full
isr189:
    pushq   $0
    pushq   $189
    jmp     isr_full
isr190:
    pushq   $0
    pushq   $190
    jmp     isr_full
isr191:
    pushq   $0
    pushq   $191
    jmp     isr_full
isr192:
    pushq   $0
    pushq   $192
    jmp     isr_full
isr193:
    pushq   $0
    pushq   $193
    jmp     isr_full
isr194:
    pushq   $0
    pushq   $194
    jmp     isr_full
isr195:
    pushq   $0
    pushq   $195
    jmp     isr_full
isr196:
    pushq   $0
    pushq   $196
    jmp     isr_full
isr197:
    pushq   $0
    pushq   $197
    jmp     isr_full
isr198:
    pushq   $0
    pushq   $198
    jmp     isr_full
isr199:
    pushq   $0
    pushq   $199
    jmp     isr_full
isr200:
    pushq   $0
    pushq   $200
    jmp     isr_full
isr201:
    pushq   $0
    pushq   $201
    jmp     isr_full
isr202:
    pushq   $0
    pushq   $202
    jmp     isr_full
isr203:
    pushq   $0
    pushq   $203
    jmp     isr_full
isr204:
    pushq   $0
    pushq   $204
    jmp     isr_full
isr205:
    pushq   $0
    pushq   $205
    jmp     isr_full
isr206:
    pushq   $0
    pushq   $206
    jmp     isr_full
isr207:
    pushq   $0
    pushq   $207
    jmp     isr_full
isr208:
    pushq   $0
    pushq   $208
    jmp     isr_full
isr209:
    pushq   $0
    pushq   $209
    jmp     isr_full
isr210:
    pushq   $0
    pushq   $210
    jmp     isr_full
isr211:
    pushq   $0
    pushq   $211
    jmp     isr_full
isr212:
    pushq   $0
    pushq   $212
    jmp     isr_full
isr213:
    pushq   $0
    pushq   $213
    jmp     isr_full
isr214:
    pushq   $0
    pushq   $214
    jmp     isr_full
isr215:
    pushq   $0
    pushq   $215
    jmp     isr_full
isr216:
    pushq   $0
    pushq   $216
    jmp     isr_full
isr217:
    pushq   $0
    pushq   $217
    jmp     isr_full
isr218:
    pushq   $0
    pushq   $218
    jmp     isr_full
isr219:
    pushq   $0
    pushq   $219
    jmp     isr_full
isr220:
    pushq   $0
    pushq   $220
    jmp     isr_full
isr221:
    pushq   $0
    pushq   $221
    jmp     isr_full
isr222:
    pushq   $0
    pushq   $222
    jmp     isr_full
isr223:
    pushq   $0
    pushq   $223
    jmp     isr_full
isr224:
    pushq   $0
    pushq   $224
    jmp     isr_full
isr225:
    pushq   $0
    pushq   $225
    jmp     isr_full
isr226:
    pushq   $0
    pushq   $226
    jmp     isr_full
isr227:
    pushq   $0
    pushq   $227
    jmp     isr_full
isr228:
    pushq   $0
    pushq   $228
    jmp     isr_full
isr229:
    pushq   $0
    pushq   $229
    jmp     isr_full
isr230:
    pushq   $0
    pushq   $230
    jmp     isr_full
isr231:
    pushq   $0
    pushq   $231
    jmp     isr_full
isr232:
    pushq   $0
    pushq   $232
    jmp     isr_full
isr233:
    pushq   $0
    pushq   $233
    jmp     isr_full
isr234:
    pushq   $0
    pushq   $234
    jmp     isr_full
isr235:
    pushq   $0
    pushq   $235
    jmp     isr_full
isr236:
    pushq   $0
    pushq   $236
    jmp     isr_full
isr237:
    pushq   $0
    pushq   $237
    jmp     isr_full
isr238:
    pushq   $0
    pushq   $238
    jmp     isr_full
isr239:
    pushq   $0
    pushq   $239
    jmp     isr_full
isr240:
    pushq   $0
    pushq   $240
    jmp     isr_full
isr241:
    pushq   $0
    pushq   $241
    jmp     isr_full
isr242:
    pushq   $0
    pushq   $242
    jmp     isr_full
isr243:
    pushq   $0
    pushq   $243
    jmp     isr_full
isr244:
    pushq   $0
    pushq   $244
    jmp     isr_full
isr245:
    pushq   $0
    pushq   $245
    jmp     isr_full
isr246:
    pushq   $0
    pushq   $246
    jmp     isr_full
isr247:
    pushq   $0
    pushq   $247
    jmp     isr_full
isr248:
    pushq   $0
    pushq   $248
    jmp     isr_full
isr249:
    pushq   $0
    pushq   $249
    jmp     isr_full
isr250:
    pushq   $0
    pushq   $250
    jmp     isr_full
isr251:
    pushq   $0
    pushq   $251
    jmp     isr_full
isr252:
    pushq   $0
    pushq   $252
    jmp     isr_full
isr253:
    pushq   $0
    pushq   $253
    jmp     isr_full
isr254:
    pushq   $0
    pushq   $254
    jmp     isr_full
isr255:
    pushq   $0
    pushq   $255
    jmp     isr_full

isr_full:
    pushq   %rax
    pushq   %rbx
    pushq   %rcx
    pushq   %rdx
    pushq   %rsi
    pushq   %rdi
    pushq   %rbp
    pushq   %r8
    pushq   %r9
    pushq   %r10
    pushq   %r11
    pushq   %r12
    pushq   %r13
    pushq   %r14
    pushq   %r15
    subq    $512, %rsp
    fxsave64 (%rsp)                     /* CPU が 16 バイト境界にそろえた後なので、ここも 16 バイト境界 / 16-byte aligned */
    testb   $3, 656(%rsp)               /* 割り込まれた CS の RPL: Ring 3 か / RPL of the interrupted CS: ring 3? */
    jz      1f
    movl    $0xC0000100, %ecx           /* IA32_FS_BASE */
    movl    688(%rsp), %eax
    movl    692(%rsp), %edx
    wrmsr
1:  cld
    movq    %rsp, %rdi
    callq   bsp_x86_trap                /* 戻り値 = 復帰するフレーム / returns the frame to restore */
    movq    %rax, %rsp
    fxrstor64 (%rsp)
    addq    $512, %rsp
    popq    %r15
    popq    %r14
    popq    %r13
    popq    %r12
    popq    %r11
    popq    %r10
    popq    %r9
    popq    %r8
    popq    %rbp
    popq    %rdi
    popq    %rsi
    popq    %rdx
    popq    %rcx
    popq    %rbx
    popq    %rax
    addq    $16, %rsp                   /* ベクタとエラーコード / vector and error code */
    iretq

    .section .rodata
    .balign 8
    .global bsp_isr_table
bsp_isr_table:
    .quad isr0, isr1, isr2, isr3, isr4, isr5, isr6, isr7
    .quad isr8, isr9, isr10, isr11, isr12, isr13, isr14, isr15
    .quad isr16, isr17, isr18, isr19, isr20, isr21, isr22, isr23
    .quad isr24, isr25, isr26, isr27, isr28, isr29, isr30, isr31
    .quad isr32, isr33, isr34, isr35, isr36, isr37, isr38, isr39
    .quad isr40, isr41, isr42, isr43, isr44, isr45, isr46, isr47
    .quad isr48, isr49, isr50, isr51, isr52, isr53, isr54, isr55
    .quad isr56, isr57, isr58, isr59, isr60, isr61, isr62, isr63
    .quad isr64, isr65, isr66, isr67, isr68, isr69, isr70, isr71
    .quad isr72, isr73, isr74, isr75, isr76, isr77, isr78, isr79
    .quad isr80, isr81, isr82, isr83, isr84, isr85, isr86, isr87
    .quad isr88, isr89, isr90, isr91, isr92, isr93, isr94, isr95
    .quad isr96, isr97, isr98, isr99, isr100, isr101, isr102, isr103
    .quad isr104, isr105, isr106, isr107, isr108, isr109, isr110, isr111
    .quad isr112, isr113, isr114, isr115, isr116, isr117, isr118, isr119
    .quad isr120, isr121, isr122, isr123, isr124, isr125, isr126, isr127
    .quad isr128, isr129, isr130, isr131, isr132, isr133, isr134, isr135
    .quad isr136, isr137, isr138, isr139, isr140, isr141, isr142, isr143
    .quad isr144, isr145, isr146, isr147, isr148, isr149, isr150, isr151
    .quad isr152, isr153, isr154, isr155, isr156, isr157, isr158, isr159
    .quad isr160, isr161, isr162, isr163, isr164, isr165, isr166, isr167
    .quad isr168, isr169, isr170, isr171, isr172, isr173, isr174, isr175
    .quad isr176, isr177, isr178, isr179, isr180, isr181, isr182, isr183
    .quad isr184, isr185, isr186, isr187, isr188, isr189, isr190, isr191
    .quad isr192, isr193, isr194, isr195, isr196, isr197, isr198, isr199
    .quad isr200, isr201, isr202, isr203, isr204, isr205, isr206, isr207
    .quad isr208, isr209, isr210, isr211, isr212, isr213, isr214, isr215
    .quad isr216, isr217, isr218, isr219, isr220, isr221, isr222, isr223
    .quad isr224, isr225, isr226, isr227, isr228, isr229, isr230, isr231
    .quad isr232, isr233, isr234, isr235, isr236, isr237, isr238, isr239
    .quad isr240, isr241, isr242, isr243, isr244, isr245, isr246, isr247
    .quad isr248, isr249, isr250, isr251, isr252, isr253, isr254, isr255

/* ───────────── 2 番目以降のコアのトランポリン（BSP が 0x8000 へ写し、SIPI のベクタ 0x08 で起こす）─────────────
 * the trampoline of the other cores (the boot core copies it to 0x8000 and wakes them with SIPI vector 0x08)
 *   リアルモード（CS = 0x0800）→ プロテクトモード → ページング（起動時の表 0x1000）+ ロングモード → 郵便受けに書かれた
 *   スタックで C の入口 bsp_x86_ap_main(コア) へ。番地はすべて 0x8000 からの相対で書く
 *   real mode (CS = 0x0800) → protected mode → paging (the boot tables at 0x1000) + long mode → the C entry
 *   bsp_x86_ap_main(core) on the stack left in the mailbox. Every address is written relative to 0x8000 */
#define TRAMP 0x8000
#define T(x) (TRAMP + ((x) - ap_tramp_start))
    .section .rodata.ap_tramp, "a"
    .balign 16
    .global ap_tramp_start, ap_tramp_end, ap_tramp_stack, ap_tramp_entry, ap_tramp_core
    .code16
ap_tramp_start:
    cli
    cld
    movw    %cs, %ax
    movw    %ax, %ds
    lgdtl   (ap_gdt_ptr - ap_tramp_start)
    movl    %cr0, %eax
    orl     $1, %eax                    /* PE */
    movl    %eax, %cr0
    ljmpl   $0x08, $T(ap_pm32)
    .code32
ap_pm32:
    movw    $0x10, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %ss
    movl    $0x620, %eax                /* PAE | OSFXSR | OSXMMEXCPT */
    movl    %eax, %cr4
    movl    $PML4, %eax                 /* 起動時の表（恒等写像） / the boot tables (identity map) */
    movl    %eax, %cr3
    movl    $0xC0000080, %ecx           /* IA32_EFER */
    rdmsr
    orl     $0x100, %eax                /* LME */
    wrmsr
    movl    %cr0, %eax
    andl    $0x9FFFFFFB, %eax           /* CD・NW・EM を下ろす / clear CD, NW, EM */
    orl     $0x80000023, %eax           /* PG | NE | MP | PE */
    movl    %eax, %cr0
    ljmpl   $0x18, $T(ap_lm64)
    .code64
ap_lm64:
    movw    $0x10, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %ss
    xorw    %ax, %ax
    movw    %ax, %fs
    movw    %ax, %gs
    fninit
    movq    T(ap_tramp_stack), %rsp
    xorl    %ebp, %ebp
    movl    T(ap_tramp_core), %edi
    movq    T(ap_tramp_entry), %rax
    callq   *%rax
2:  cli
    hlt
    jmp     2b
    .balign 8
ap_gdt:
    .quad   0x0000000000000000
    .quad   0x00CF9A000000FFFF          /* 0x08: 32 ビットのコード / 32-bit code */
    .quad   0x00CF92000000FFFF          /* 0x10: データ / data */
    .quad   0x00AF9A000000FFFF          /* 0x18: 64 ビットのコード / 64-bit code */
ap_gdt_ptr:
    .word   31
    .long   T(ap_gdt)
    .balign 8
ap_tramp_stack:
    .quad   0
ap_tramp_entry:
    .quad   0
ap_tramp_core:
    .long   0
ap_tramp_end:
