/*
 * bsp.c — Fascon 1.0 BSP: QEMU x86_64（Fascon 自身の仮想 BIOS の上。SeaBIOS / UEFI は使わない）
 *         Fascon 1.0 BSP for QEMU x86_64, running on Fascon's own virtual BIOS (no SeaBIOS / UEFI)
 *
 * bios.S がロングモード（0〜4 GiB の恒等マップ、2 MiB ページ）に入り、カーネル像を RAM の 1 MiB へ写してから bsp_main を呼ぶ。
 * bios.S enters long mode (identity map of 0-4 GiB, 2 MiB pages), copies the kernel image to RAM at 1 MiB, then calls bsp_main.
 *
 *   1. COM1（16550、I/O ポート 0x3F8）を 115200 8N1 に設定 / set up COM1 (16550, I/O port 0x3F8) as 115200 8N1
 *   2. GDT（Ring 0 と Ring 3 のセグメント、コアごとの TSS）と IDT（256 個。NMI・#DF・#MC は IST の別スタック）
 *      the GDT (ring 0 and ring 3 segments, one TSS per core) and the IDT (256 gates; NMI, #DF and #MC on IST stacks)
 *   3. カーネルの表を作り直す（0〜4 GiB の恒等写像を 2 MiB ページで、大域 = G）。PCID・INVPCID・SMEP・SMAP・NX は CPU に
 *      あれば使う / rebuild the kernel tables (identity map of 0-4 GiB with global 2 MiB pages) and use PCID, INVPCID,
 *      SMEP, SMAP and NX when the CPU has them
 *   4. Local APIC（タイマー = irq 27 / 30、IPI）と HPET（timer_now / timer_freq の時計）。8259 は全部マスクする
 *      the local APIC (its timer = irq 27 / 30, IPIs) and the HPET (the clock of timer_now / timer_freq); the 8259 is masked
 *   5. マルチコア: INIT-SIPI-SIPI でコア 1〜3 を起こす（トランポリンは 0x8000）。どのコアも FS の土台を自分の TLS へ向ける
 *      multi-core: cores 1 to 3 are started with INIT-SIPI-SIPI (trampoline at 0x8000); every core points its FS base at
 *      its own TLS block
 *   6. own ヒープとフレームリージョンを登録して fascon2_main() / register the own heap and the frame region, call fascon2_main()
 *
 * 抽象プラットフォーム API（fascon_plat_*）は AArch64 の BSP と同じ名前・同じ意味で実装する。コンパイラが出す C には x86 の
 * 命令は 1 つも無い（コアごとの切り替え・ユーザー空間・TLB の撃ち落とし・パニックの全コア停止はすべてここ）
 * the abstract platform API (fascon_plat_*) is implemented with the same names and meaning as the AArch64 BSP; the C the
 * compiler emits contains no x86 instruction at all (task switching, user space, TLB shootdown and the all-core stop on
 * panic all live here)
 *
 * 終了は isa-debug-exit（I/O ポート 0xF4）: QEMU は (コード << 1) | 1 で終わる。fascon qemu は "[bsp] exit code N" の行から
 * 元のコードを読む / exit goes through isa-debug-exit (I/O port 0xF4): QEMU ends with (code << 1) | 1; fascon qemu reads
 * the original code from the "[bsp] exit code N" line
 *
 * 必要な QEMU の起動オプション / QEMU options:
 *   qemu-system-x86_64 -M pc -cpu max -m 2G -bios app.bios.bin -nic none -no-reboot [-smp 4]
 *                      -device isa-debug-exit,iobase=0xf4,iosize=0x04 -serial stdio [-vga none -device ramfb]
 */
#include "fascon2_rt.h"


#define COM1 0x3F8u
#define DEBUG_EXIT 0xF4u
#define FWCFG_SEL 0x510u
#define FWCFG_DATA 0x511u
#define FWCFG_DMA_HI 0x514u
#define FWCFG_DMA_LO 0x518u
#define LAPIC_BASE 0xFEE00000ul
#define IOAPIC_BASE 0xFEC00000ul
#define HPET_BASE 0xFED00000ul

/* GDT のセレクタ（bios.S の起動時の GDT と 0x08〜0x18 は同じ） / GDT selectors (0x08-0x18 as in the boot GDT of bios.S) */
#define SEL_KCODE 0x18u
#define SEL_KDATA 0x10u
#define SEL_UDATA 0x23u /* GDT[4] | RPL 3 */
#define SEL_UCODE 0x2Bu /* GDT[5] | RPL 3 */
#define SEL_TSS(c) (0x30u + 16u * (c))

/* 割り込みのベクタ / interrupt vectors
 *   0〜31 例外、0x20〜0x2F 8259（マスク済み）、0x40 LAPIC タイマー（= irq 27 / 30）、0x50〜0x67 I/O APIC の入力 0〜23
 *   （= irq 32〜55）、0x80 システムコール（DPL 3）、0x81 カーネルの自発的な切り替え、0xF0 TLB の撃ち落とし、0xF1 コア 0 を
 *   起こす、0xFF スプリアス。パニックの全コア停止は NMI
 *   0-31 exceptions, 0x20-0x2F the 8259 (masked), 0x40 the LAPIC timer (= irq 27 / 30), 0x50-0x67 I/O APIC inputs 0-23
 *   (= irq 32-55), 0x80 system calls (DPL 3), 0x81 voluntary kernel switches, 0xF0 TLB shootdown, 0xF1 wake core 0, 0xFF
 *   spurious; the all-core stop on panic is an NMI */
#define VEC_NMI 2u
#define VEC_DF 8u
#define VEC_PF 14u
#define VEC_MC 18u
#define VEC_TIMER 0x40u
#define VEC_GSI0 0x50u
#define GSI_COUNT 24u
#define VEC_SYSCALL 0x80u
#define VEC_SVC 0x81u
#define VEC_TLB 0xF0u
#define VEC_WAKE 0xF1u
#define VEC_SPURIOUS 0xFFu

void fascon2_main(void);
extern char __kernel_start[], __kernel_end[], __heap_start[], __heap_end[], __frame_start[], __frame_end[], __arena_start[], __arena_end[];
extern const uint64_t bsp_isr_table[256];

/* 保存フレーム（bios.S の isr_full と同じ並び。688 バイト） / the saved frame (laid out as isr_full in bios.S, 688 bytes) */
typedef struct {
    uint8_t fx[512];
    uint64_t r15, r14, r13, r12, r11, r10, r9, r8, rbp, rdi, rsi, rdx, rcx, rbx, rax;
    uint64_t vector, error;
    uint64_t rip, cs, rflags, rsp, ss;
} BspFrame;
_Static_assert(sizeof(BspFrame) == 688, "BspFrame must match isr_full in bios.S (688 bytes)");
_Static_assert(offsetof(BspFrame, rax) == 624 && offsetof(BspFrame, cs) == 656 && offsetof(BspFrame, ss) == 680, "BspFrame layout (bios.S)");

/* 新しい文脈の FPU / SSE の状態: FCW = 0x037F、MXCSR = 0x1F80（例外はすべてマスク） / the FPU / SSE state of a new context:
 * FCW = 0x037F, MXCSR = 0x1F80 (every exception masked) */
static void bsp_fx_init(BspFrame *f) {
    f->fx[0] = 0x7Fu;
    f->fx[1] = 0x03u;
    f->fx[24] = 0x80u;
    f->fx[25] = 0x1Fu;
}

/* ───────────── コアの番号 / the core number ───────────── */

#if defined(FASCON2_SMP)
#define BSP_NCORE FASCON2_MAX_CPUS
static __thread uint32_t bsp_core_tls; /* FS 相対の 1 命令で読める / one FS-relative load */
static inline uint32_t bsp_core(void) { return bsp_core_tls; }
#else
#define BSP_NCORE 1u
static inline uint32_t bsp_core(void) { return 0u; }
#endif

/* ───────────── COM1（16550） ───────────── */

static void uart_init(void) {
    fascon_outb(COM1 + 1u, 0x00u); /* 割り込みなし / no interrupts */
    fascon_outb(COM1 + 3u, 0x80u); /* DLAB */
    fascon_outb(COM1 + 0u, 0x01u); /* 115200 baud */
    fascon_outb(COM1 + 1u, 0x00u);
    fascon_outb(COM1 + 3u, 0x03u); /* 8N1 */
    fascon_outb(COM1 + 2u, 0xC7u); /* FIFO */
    fascon_outb(COM1 + 4u, 0x03u); /* DTR | RTS */
}

static void uart_putc(char c) {
    for (int spin = 0; spin < 1000000 && (fascon_inb(COM1 + 5u) & 0x20u) == 0u; spin++) FASCON_CPU_RELAX();
    fascon_outb(COM1, (uint8_t)c);
}

void fascon_plat_print(const char *s, size_t len) {
    for (size_t i = 0; i < len; i++) {
        if (s[i] == '\n') uart_putc('\r');
        uart_putc(s[i]);
    }
}

static void puts_raw(const char *s) {
    size_t n = 0;
    while (s[n]) n++;
    fascon_plat_print(s, n);
}

static void put_hex(uint64_t v) {
    char b[19] = "0x";
    for (int i = 0; i < 16; i++) {
        unsigned d = (unsigned)((v >> (60 - 4 * i)) & 0xf);
        b[2 + i] = (char)(d < 10 ? '0' + d : 'a' + d - 10);
    }
    b[18] = 0;
    puts_raw(b);
}

static void put_dec(uint64_t v) {
    char b[24];
    int i = 23;
    b[i] = 0;
    if (v == 0) b[--i] = '0';
    while (v) {
        b[--i] = (char)('0' + v % 10);
        v /= 10;
    }
    puts_raw(&b[i]);
}

/* ───────────── CPU のレジスタ / CPU registers ───────────── */

static inline uint64_t bsp_rdmsr(uint32_t msr) {
    uint32_t lo, hi;
    __asm__ __volatile__("rdmsr" : "=a"(lo), "=d"(hi) : "c"(msr));
    return ((uint64_t)hi << 32) | lo;
}
static inline void bsp_wrmsr(uint32_t msr, uint64_t v) { __asm__ __volatile__("wrmsr" : : "c"(msr), "a"((uint32_t)v), "d"((uint32_t)(v >> 32)) : "memory"); }
static inline uint64_t bsp_cr2(void) {
    uint64_t v;
    __asm__ __volatile__("movq %%cr2, %0" : "=r"(v));
    return v;
}
static inline uint64_t bsp_cr4(void) {
    uint64_t v;
    __asm__ __volatile__("movq %%cr4, %0" : "=r"(v));
    return v;
}
static inline void bsp_set_cr4(uint64_t v) { __asm__ __volatile__("movq %0, %%cr4" : : "r"(v) : "memory"); }
static inline void bsp_set_cr3(uint64_t v) { __asm__ __volatile__("movq %0, %%cr3" : : "r"(v) : "memory"); }
static inline uint64_t bsp_rdtsc(void) {
    uint32_t lo, hi;
    __asm__ __volatile__("rdtsc" : "=a"(lo), "=d"(hi));
    return ((uint64_t)hi << 32) | lo;
}
static void bsp_cpuid(uint32_t leaf, uint32_t sub, uint32_t r[4]) { __asm__ __volatile__("cpuid" : "=a"(r[0]), "=b"(r[1]), "=c"(r[2]), "=d"(r[3]) : "a"(leaf), "c"(sub)); }

/* CPU の機能（コア 0 が調べ、全コアで同じものを使う） / CPU features (probed by core 0, used the same way on every core) */
static uint32_t bsp_has_pge, bsp_has_pcid, bsp_has_invpcid, bsp_has_smep, bsp_has_smap, bsp_has_nx;
static uint32_t bsp_pcid_on, bsp_smap_on;

static void bsp_detect(void) {
    uint32_t r[4];
    bsp_cpuid(0u, 0u, r);
    const uint32_t max = r[0];
    bsp_cpuid(1u, 0u, r);
    bsp_has_pcid = (r[2] >> 17) & 1u;
    bsp_has_pge = (r[3] >> 13) & 1u;
    if (max >= 7u) {
        bsp_cpuid(7u, 0u, r);
        bsp_has_smep = (r[1] >> 7) & 1u;
        bsp_has_invpcid = (r[1] >> 10) & 1u;
        bsp_has_smap = (r[1] >> 20) & 1u;
    }
    bsp_cpuid(0x80000000u, 0u, r);
    if (r[0] >= 0x80000001u) {
        bsp_cpuid(0x80000001u, 0u, r);
        bsp_has_nx = (r[3] >> 20) & 1u;
    }
}

/* ───────────── 終了・停止 / exit and halt ───────────── */

#if defined(FASCON2_SMP)
static volatile uint32_t bsp_panic_sent;
static uint32_t bsp_cpu_count = 1u;
#endif
static volatile uint32_t bsp_panic_acks;

static FASCON_NORETURN void power_off(int code) {
    /* isa-debug-exit: QEMU は (値 << 1) | 1 で終了する / QEMU exits with (value << 1) | 1 */
    fascon_outb(DEBUG_EXIT, (uint8_t)code);
    for (;;) __asm__ __volatile__("cli; hlt" ::: "memory");
}

static void bsp_vm_report(void);
FASCON_NORETURN void fascon_plat_exit(int code) {
    bsp_vm_report();
    puts_raw("[bsp] exit code ");
    put_dec((uint64_t)(code < 0 ? -code : code));
    puts_raw("\n");
    power_off(code);
}

FASCON_NORETURN void fascon_plat_halt(void) {
    /* 画面を表示したまま止まる（割り込みは使わない） / stop while keeping the screen (no interrupts) */
    puts_raw("[bsp] halted (screen kept)\n");
    for (;;) __asm__ __volatile__("cli; hlt" ::: "memory");
}

FASCON_NORETURN void fascon_plat_kernel_panic(uint32_t code, const char *msg) {
    puts_raw("\nFASCON PANIC ");
    char b[8] = "0x0000 ";
    for (int i = 0; i < 4; i++) {
        unsigned d = (code >> (12 - 4 * i)) & 0xf;
        b[2 + i] = (char)(d < 10 ? '0' + d : 'a' + d - 10);
    }
    fascon_plat_print(b, 7);
    puts_raw(msg ? msg : "");
    puts_raw("\n");
#if defined(FASCON2_SMP)
    if (bsp_panic_sent) {
        puts_raw("[bsp] panic broadcast (NMI IPI): ");
        put_dec(__atomic_load_n(&bsp_panic_acks, __ATOMIC_ACQUIRE));
        puts_raw(" other core(s) stopped\n");
    }
#endif
    power_off(70);
}

/* パニックの全コア停止（NMI を受けたコア）: 割り込みを止めて眠る。NMI は戻らないので、次の NMI も来ない
 * the all-core stop on panic (a core that took the NMI): mask interrupts and sleep; the NMI never returns, so no further
 * NMI arrives either */
static uint8_t bsp_stopped[FASCON2_MAX_CPUS];
static FASCON_NORETURN void bsp_panic_stop(void) {
    __asm__ __volatile__("cli" ::: "memory");
    const uint32_t me = bsp_core();
    if (!bsp_stopped[me]) { /* 止まってから来た NMI は数えない / an NMI arriving after the stop is not counted again */
        bsp_stopped[me] = 1u;
        __atomic_fetch_add(&bsp_panic_acks, 1u, __ATOMIC_RELEASE);
    }
    for (;;) __asm__ __volatile__("cli; hlt" ::: "memory");
}
/* ランタイムの待ちループでパニックに気づいたコアも、ここで止まって数えられる / a core that noticed the panic in a waiting
 * loop of the runtime stops and is counted here too */
FASCON_NORETURN void fascon_plat_core_parked(void) { bsp_panic_stop(); }

/* ───────────── カーネルの表: 0〜4 GiB の恒等写像（2 MiB ページ、大域） / the kernel tables: identity map of 0-4 GiB ─────────────
 * 3〜4 GiB（LAPIC・I/O APIC・HPET・ROM）はキャッシュ無効。ユーザーのアドレス空間はこの表の上位の項目を写して作るので、
 * カーネルの写しは大域（G）のまま、どのアドレス空間でも TLB に残る
 * 3-4 GiB (LAPIC, I/O APIC, HPET, ROM) is uncached. User address spaces copy the upper entries of these tables, so the
 * kernel mappings stay global (G) and survive in the TLB across every address space */
#define PG_P 0x001ull
#define PG_RW 0x002ull
#define PG_US 0x004ull
#define PG_PWT 0x008ull
#define PG_PCD 0x010ull
#define PG_PS 0x080ull
#define PG_G 0x100ull
#define PG_NX (1ull << 63)
#define PG_ADDR 0x000FFFFFFFFFF000ull
#define CR3_NOFLUSH (1ull << 63)
static uint64_t bsp_kpml4[512] __attribute__((aligned(4096)));
static uint64_t bsp_kpdpt[512] __attribute__((aligned(4096)));
static uint64_t bsp_kpd[4][512] __attribute__((aligned(4096)));

static void bsp_kernel_tables(void) {
    const uint64_t g = bsp_has_pge ? PG_G : 0u;
    for (uint32_t k = 0; k < 4u; k++) {
        bsp_kpdpt[k] = (uint64_t)(uintptr_t)bsp_kpd[k] | PG_P | PG_RW;
        for (uint32_t i = 0; i < 512u; i++) {
            uint64_t e = ((uint64_t)k << 30) | ((uint64_t)i << 21) | PG_P | PG_RW | PG_PS | g;
            if (k == 3u) e |= PG_PCD | PG_PWT;
            bsp_kpd[k][i] = e;
        }
    }
    bsp_kpml4[0] = (uint64_t)(uintptr_t)bsp_kpdpt | PG_P | PG_RW;
}

/* ───────────── GDT・TSS・IDT ───────────── */

typedef struct __attribute__((packed)) {
    uint32_t rsv0;
    uint64_t rsp0, rsp1, rsp2;
    uint64_t rsv1;
    uint64_t ist[7];
    uint64_t rsv2;
    uint16_t rsv3, iomap;
} BspTss;
_Static_assert(sizeof(BspTss) == 104, "a 64-bit TSS is 104 bytes");

static uint64_t bsp_gdt[6u + 2u * FASCON2_MAX_CPUS] __attribute__((aligned(16))) = {
    0x0000000000000000ull, /* 0x00 */
    0x00CF9A000000FFFFull, /* 0x08: 32 ビットのコード / 32-bit code */
    0x00CF92000000FFFFull, /* 0x10: カーネルのデータ / kernel data */
    0x00AF9A000000FFFFull, /* 0x18: カーネルの 64 ビットのコード / kernel 64-bit code */
    0x00CFF2000000FFFFull, /* 0x20: ユーザーのデータ（DPL 3） / user data (DPL 3) */
    0x00AFFA000000FFFFull, /* 0x28: ユーザーの 64 ビットのコード（DPL 3） / user 64-bit code (DPL 3) */
};
static BspTss bsp_tss[FASCON2_MAX_CPUS] __attribute__((aligned(128)));
/* IST のスタック（コアごとに 2 本: 1 = NMI、2 = #DF / #MC） / IST stacks (two per core: 1 = NMI, 2 = #DF / #MC) */
static uint8_t bsp_ist[FASCON2_MAX_CPUS][2][8192] __attribute__((aligned(16)));

typedef struct {
    uint16_t off_lo, sel;
    uint8_t ist, type;
    uint16_t off_mid;
    uint32_t off_hi, zero;
} __attribute__((packed)) IdtGate;
static IdtGate bsp_idt[256] __attribute__((aligned(16)));

static void bsp_tables_init(void) {
    for (uint32_t c = 0; c < FASCON2_MAX_CPUS; c++) {
        const uint64_t base = (uint64_t)(uintptr_t)&bsp_tss[c];
        const uint64_t limit = sizeof(BspTss) - 1u;
        bsp_gdt[6u + 2u * c] = (limit & 0xFFFFu) | ((base & 0xFFFFFFu) << 16) | (0x89ull << 40) | (((limit >> 16) & 0xFu) << 48) | (((base >> 24) & 0xFFu) << 56);
        bsp_gdt[7u + 2u * c] = base >> 32;
        bsp_tss[c].iomap = (uint16_t)sizeof(BspTss); /* I/O 許可ビットマップなし: Ring 3 の in / out は #GP / no I/O bitmap */
    }
    for (uint32_t i = 0; i < 256u; i++) {
        const uint64_t h = bsp_isr_table[i];
        bsp_idt[i].off_lo = (uint16_t)h;
        bsp_idt[i].sel = SEL_KCODE;
        bsp_idt[i].ist = 0u;
        bsp_idt[i].type = 0x8Eu; /* 割り込みゲート（P, DPL 0） / interrupt gate (P, DPL 0) */
        bsp_idt[i].off_mid = (uint16_t)(h >> 16);
        bsp_idt[i].off_hi = (uint32_t)(h >> 32);
        bsp_idt[i].zero = 0u;
    }
    bsp_idt[VEC_SYSCALL].type = 0xEEu; /* DPL 3: Ring 3 の int $0x80 だけを通す / DPL 3: lets ring 3 use int $0x80 */
    bsp_idt[VEC_NMI].ist = 1u;
    bsp_idt[VEC_DF].ist = 2u;
    bsp_idt[VEC_MC].ist = 2u;
}

/* このコアに GDT・TSS・IDT を読み込む / load the GDT, the TSS and the IDT on this core */
static void bsp_load_tables(uint32_t core) {
    struct {
        uint16_t limit;
        uint64_t base;
    } __attribute__((packed)) g = {(uint16_t)(sizeof bsp_gdt - 1u), (uint64_t)(uintptr_t)bsp_gdt}, i = {(uint16_t)(sizeof bsp_idt - 1u), (uint64_t)(uintptr_t)bsp_idt};
    __asm__ __volatile__("lgdt %0" : : "m"(g) : "memory");
    /* CS と DS / ES / SS を読み直す（FS と GS には触れない: FS の土台は TLS） / reload CS and DS / ES / SS (FS and GS are
     * left alone: the FS base is the TLS) */
    __asm__ __volatile__("pushq $0x18\n\t"
                         "leaq 1f(%%rip), %%rax\n\t"
                         "pushq %%rax\n\t"
                         "lretq\n"
                         "1:\n\t"
                         "movw $0x10, %%ax\n\t"
                         "movw %%ax, %%ds\n\t"
                         "movw %%ax, %%es\n\t"
                         "movw %%ax, %%ss"
                         :
                         :
                         : "rax", "memory");
    const uint16_t tss = (uint16_t)SEL_TSS(core);
    __asm__ __volatile__("ltr %0" : : "r"(tss) : "memory");
    __asm__ __volatile__("lidt %0" : : "m"(i) : "memory");
}

/* ───────────── Local APIC・I/O APIC・8259 ───────────── */

#define LAPIC_ID 0x020u
#define LAPIC_TPR 0x080u
#define LAPIC_EOI 0x0B0u
#define LAPIC_SVR 0x0F0u
#define LAPIC_ESR 0x280u
#define LAPIC_ICR_LO 0x300u
#define LAPIC_ICR_HI 0x310u
#define LAPIC_LVT_TIMER 0x320u
#define LAPIC_LVT_ERR 0x370u
#define LAPIC_TIMER_INIT 0x380u
#define LAPIC_TIMER_CUR 0x390u
#define LAPIC_TIMER_DIV 0x3E0u
static inline uint32_t lapic_rd(uint32_t r) { return *(volatile uint32_t *)(uintptr_t)(LAPIC_BASE + r); }
static inline void lapic_wr(uint32_t r, uint32_t v) { *(volatile uint32_t *)(uintptr_t)(LAPIC_BASE + r) = v; }
static inline void lapic_eoi(void) { lapic_wr(LAPIC_EOI, 0u); }
static uint32_t bsp_apic_id[FASCON2_MAX_CPUS];

/* IPI を送る（前の IPI が出るのを待ってから書き、出終わるのを待つ） / send an IPI (wait until the previous one is out, write,
 * wait until this one is out) */
static void lapic_send(uint32_t apic, uint32_t lo) {
    for (uint32_t spin = 0; spin < 10000000u && (lapic_rd(LAPIC_ICR_LO) & (1u << 12)); spin++) FASCON_CPU_RELAX();
    lapic_wr(LAPIC_ICR_HI, apic << 24);
    lapic_wr(LAPIC_ICR_LO, lo);
    for (uint32_t spin = 0; spin < 10000000u && (lapic_rd(LAPIC_ICR_LO) & (1u << 12)); spin++) FASCON_CPU_RELAX();
}

/* このコアの LAPIC: ソフトウェアで有効化、タイマーは単発（分周 1、ベクタ 0x40、マスクしない: 割り込みを開くまでは
 * EFLAGS.IF が止める。単発のタイマーが IF = 0 の間に切れても、要求は IRR に残る）
 * the LAPIC of this core: software-enabled, the timer one-shot (divide by 1, vector 0x40, unmasked: EFLAGS.IF holds it back until
 * interrupts are opened; a one-shot expiring while IF = 0 stays pending in the IRR) */
static void lapic_on(uint32_t core) {
    lapic_wr(LAPIC_TPR, 0u);
    lapic_wr(LAPIC_SVR, 0x100u | VEC_SPURIOUS);
    lapic_wr(LAPIC_LVT_ERR, 0x10000u);
    lapic_wr(LAPIC_TIMER_DIV, 0xBu);
    lapic_wr(LAPIC_LVT_TIMER, VEC_TIMER);
    lapic_wr(LAPIC_ESR, 0u);
    lapic_wr(LAPIC_ESR, 0u);
    bsp_apic_id[core] = lapic_rd(LAPIC_ID) >> 24;
}

static void ioapic_wr(uint32_t reg, uint32_t v) {
    *(volatile uint32_t *)(uintptr_t)IOAPIC_BASE = reg;
    *(volatile uint32_t *)(uintptr_t)(IOAPIC_BASE + 0x10u) = v;
}

/* 8259 はベクタ 0x20〜0x2F へずらしてから全部マスクする（割り込みは LAPIC と I/O APIC だけ） / the 8259 is remapped to
 * 0x20-0x2F and fully masked (interrupts come only through the LAPIC and the I/O APIC) */
static void pic_off(void) {
    fascon_outb(0x20u, 0x11u);
    fascon_outb(0xA0u, 0x11u);
    fascon_outb(0x21u, 0x20u);
    fascon_outb(0xA1u, 0x28u);
    fascon_outb(0x21u, 0x04u);
    fascon_outb(0xA1u, 0x02u);
    fascon_outb(0x21u, 0x01u);
    fascon_outb(0xA1u, 0x01u);
    fascon_outb(0x21u, 0xFFu);
    fascon_outb(0xA1u, 0xFFu);
}

/* ───────────── 時計: HPET（無ければ PIT で測った TSC）と LAPIC タイマー / the clock: HPET (or the TSC measured with the PIT)
 *               and the LAPIC timer ─────────────
 * timer_now / timer_freq は全コアで共通の単調な時計（HPET の主カウンタ。QEMU では 100 MHz）。timer_arm は自分のコアの
 * LAPIC タイマーの単発で、時計の刻みを起動時に測った比で LAPIC の刻みへ直す
 * timer_now / timer_freq are a monotonic clock shared by all cores (the HPET main counter, 100 MHz on QEMU). timer_arm is
 * a one-shot of the LAPIC timer of the calling core; clock ticks are converted to LAPIC ticks with a ratio measured at boot */
static uint64_t bsp_hpet_hz, bsp_tsc_hz;
static uint64_t bsp_lapic_q32; /* LAPIC の刻み / 時計の刻み（32.32 の固定小数点） / LAPIC ticks per clock tick (32.32 fixed point) */
static uint32_t bsp_timer_intid = 27u;

static inline uint32_t hpet_rd(uint32_t r) { return *(volatile uint32_t *)(uintptr_t)(HPET_BASE + r); }
static uint64_t hpet_now(void) {
    uint32_t hi, lo, hi2;
    do { /* 32 ビットずつ読んで、上位が変わらなかったら確定 / read halves until the upper one is stable */
        hi = hpet_rd(0xF4u);
        lo = hpet_rd(0xF0u);
        hi2 = hpet_rd(0xF4u);
    } while (hi != hi2);
    return ((uint64_t)hi << 32) | lo;
}

uint64_t fascon_plat_timer_freq(void) { return bsp_hpet_hz ? bsp_hpet_hz : bsp_tsc_hz; }
uint64_t fascon_plat_timer_now(void) { return bsp_hpet_hz ? hpet_now() : bsp_rdtsc(); }

static void bsp_delay_us(uint64_t us) {
    const uint64_t f = fascon_plat_timer_freq();
    if (f == 0u) {
        for (uint64_t i = 0; i < us * 1000u; i++) FASCON_CPU_RELAX();
        return;
    }
    const uint64_t t0 = fascon_plat_timer_now(), dt = f / 1000000u * us + 1u;
    while (fascon_plat_timer_now() - t0 < dt) FASCON_CPU_RELAX();
}

static void bsp_clock_init(void) {
    const uint32_t period_fs = hpet_rd(0x04u);
    if (period_fs != 0u && period_fs <= 100000000u) {
        bsp_hpet_hz = 1000000000000000ull / period_fs;
        *(volatile uint32_t *)(uintptr_t)(HPET_BASE + 0x10u) |= 1u; /* ENABLE_CNF */
    } else {
        /* HPET が無い: PIT のチャネル 2（10 ms の単発）で TSC の速さを測る / no HPET: measure the TSC with a 10 ms one-shot
         * of PIT channel 2 */
        const uint8_t p61 = fascon_inb(0x61u);
        fascon_outb(0x61u, (uint8_t)((p61 & ~0x02u) | 0x01u));
        fascon_outb(0x43u, 0xB0u);
        fascon_outb(0x42u, (uint8_t)(11932u & 0xFFu));
        fascon_outb(0x42u, (uint8_t)(11932u >> 8));
        const uint64_t t0 = bsp_rdtsc();
        for (uint32_t spin = 0; spin < 100000000u && (fascon_inb(0x61u) & 0x20u) == 0u; spin++) FASCON_CPU_RELAX();
        bsp_tsc_hz = (bsp_rdtsc() - t0) * 100u;
        fascon_outb(0x61u, p61);
        if (bsp_tsc_hz == 0u) bsp_tsc_hz = 1000000000u;
    }
    /* LAPIC タイマーの速さを時計で 10 ms 測る（マスクして数えるだけ） / measure the LAPIC timer against the clock for 10 ms
     * (masked: it only counts) */
    lapic_wr(LAPIC_LVT_TIMER, 0x10000u | VEC_TIMER);
    lapic_wr(LAPIC_TIMER_DIV, 0xBu);
    const uint64_t f = fascon_plat_timer_freq();
    const uint64_t t0 = fascon_plat_timer_now();
    lapic_wr(LAPIC_TIMER_INIT, 0xFFFFFFFFu);
    while (fascon_plat_timer_now() - t0 < f / 100u) FASCON_CPU_RELAX();
    const uint32_t cur = lapic_rd(LAPIC_TIMER_CUR);
    const uint64_t dt = fascon_plat_timer_now() - t0;
    lapic_wr(LAPIC_TIMER_INIT, 0u);
    const uint64_t counted = 0xFFFFFFFFull - cur;
    bsp_lapic_q32 = dt ? (counted << 32) / dt : (1ull << 32);
    if (bsp_lapic_q32 == 0u) bsp_lapic_q32 = 1u;
    lapic_wr(LAPIC_LVT_TIMER, VEC_TIMER);
}

/* 時計の ticks 刻み後に、このコアのタイマー割り込み（irq 27 / 30）を 1 回 / one timer interrupt (irq 27 / 30) on this core
 * after `ticks` clock ticks */
void fascon_plat_timer_arm(uint64_t ticks) {
    uint64_t c = (uint64_t)(((unsigned __int128)ticks * bsp_lapic_q32) >> 32);
    if (c == 0u) c = 1u;
    if (c > 0xFFFFFFFFull) c = 0xFFFFFFFFull;
    lapic_wr(LAPIC_LVT_TIMER, VEC_TIMER);
    lapic_wr(LAPIC_TIMER_INIT, (uint32_t)c);
}

/* ───────────── 割り込みの表・タスク / the interrupt table and tasks ─────────────
 * - irq の番号は AArch64 と同じ抽象の番号: 27 / 30 = このコアのタイマー（LAPIC タイマー）、32 + n = I/O APIC の入力 n（GSI n。
 *   CPU0 へ、0〜15 はエッジ・ハイ、16 以上はレベル・ロー）。ACK は不要で、ハンドラの後に LAPIC の EOI
 *   irq numbers are the same abstract numbers as on AArch64: 27 / 30 = this core timer (the LAPIC timer), 32 + n = input n
 *   of the I/O APIC (GSI n; to CPU0, 0-15 edge / high, 16 and up level / low). No acknowledge is needed; the LAPIC EOI
 *   follows the handler
 * - タスクの表・いまのタスク・実行キューはコアごと（fascon_cpu_sched）。タスクの文脈は、そのタスクのカーネルスタックに
 *   積んだ保存フレームの番地 1 つで、切り替えは bios.S の出口へ別のフレームの番地を返すだけ
 *   the task table, the current task and the run queue are per core (fascon_cpu_sched). The context of a task is one
 *   address: its saved frame on its own kernel stack; switching returns another frame address to the exit path of bios.S
 * - Ring 3 のタスクはそれぞれ自分のアドレス空間（PCID 付きの CR3）を持つ。ユーザーメモリは 4 KiB のページで、初めて触れた
 *   ときに写す。切り替えは CR3 を（掃かない印を付けて）書くだけ。掃くのはページを外す・アドレス空間を捨てるときだけ
 *   every ring 3 task has its own address space (CR3 with a PCID). User memory is mapped in 4 KiB pages on first touch.
 *   A switch only writes CR3 (with the no-flush bit); flushes happen only when a page is unmapped or a space is dropped */

typedef struct {
    BspFrame *sp;
    f20_id_t id;
    int alive;
    void (*entry)(uint64_t);
    uint64_t arg;
    void (*on_exit)(void);
    uint8_t *stack;
    uint8_t *ktop; /* カーネルスタックの上端（16 バイト境界） / the top of the kernel stack (16-byte aligned) */
    uint8_t *f_top, *f_end, *f_base;
    int user;      /* Ring 3 のタスク / a ring 3 task */
    uint32_t asid; /* Ring 3 のタスクのアドレス空間（PCID）。カーネルのタスクは 0 / address space (PCID) of a ring 3 task; 0 for kernel tasks */
#if defined(FASCON2_GC)
    void *gc_top; /* fascon-gc: このタスクのシャドウスタックの先頭 / the shadow-stack top of this task */
#endif
} BspTask;

static BspTask bsp_tasks[BSP_NCORE][FASCON2_TASK_MAX];
static const FasconIrqEntry *bsp_irqs = 0;
static uint32_t bsp_irq_count = 0;
static uint64_t bsp_tp[FASCON2_MAX_CPUS]; /* コアごとの FS の土台（TLS） / the FS base (TLS) of each core */

static inline f20_sched_t *bsp_sched(void) { return fascon_cpu_sched(bsp_core()); }
static inline BspTask *bsp_cur_task(void) {
    const f20_sched_t *S = bsp_sched();
    return S->cur >= 0 ? &bsp_tasks[bsp_core()][S->cur] : (BspTask *)0;
}

void fascon_plat_irq_register(const FasconIrqEntry *table, uint32_t count) {
    bsp_irqs = table;
    bsp_irq_count = count;
    int has27 = 0, has30 = 0;
    for (uint32_t i = 0; i < count; i++) {
        const uint32_t n = table[i].intid;
        if (n == 27u) has27 = 1;
        if (n == 30u) has30 = 1;
        if (n >= 32u && n < 32u + GSI_COUNT) {
            const uint32_t gsi = n - 32u;
            const uint32_t lo = (VEC_GSI0 + gsi) | (gsi >= 16u ? (1u << 13) | (1u << 15) : 0u);
            ioapic_wr(0x10u + 2u * gsi + 1u, (uint32_t)bsp_apic_id[0] << 24); /* CPU0 へ / to CPU0 */
            ioapic_wr(0x10u + 2u * gsi, lo);
        } else if (n != 27u && n != 30u) {
            puts_raw("[bsp] irq ");
            put_dec(n);
            puts_raw(" (");
            puts_raw(table[i].name ? table[i].name : "?");
            puts_raw("): QEMU pc has no such interrupt (27 / 30 = the core timer, 32 + n = I/O APIC input n); it never fires\n");
        }
    }
    if (has30 && !has27) bsp_timer_intid = 30u;
}

uint64_t fascon_plat_irq_save(void) {
    uint64_t f;
    __asm__ __volatile__("pushfq\n\tpopq %0\n\tcli" : "=r"(f) : : "memory");
    return f;
}

void fascon_plat_irq_restore(uint64_t state) {
    if (state & 0x200u) {
        __asm__ __volatile__("sti" ::: "memory");
    } else {
        __asm__ __volatile__("cli" ::: "memory");
    }
}

void fascon_plat_irq_open(void) { __asm__ __volatile__("sti" ::: "memory"); }

/* 割り込みを禁止したまま確かめた後で sti; hlt（sti の直後の 1 命令は割り込まれないので、hlt の前に来た割り込みも hlt を
 * 起こす）→ 元の状態へ / sti; hlt after checking with interrupts masked (the instruction right after sti cannot be
 * interrupted, so an interrupt arriving before hlt still wakes it) → back to the previous state */
void fascon_plat_wait_irq(void) {
    const uint64_t d = fascon_plat_irq_save();
    __asm__ __volatile__("sti\n\thlt" ::: "memory");
    fascon_plat_irq_restore(d);
}

static void bsp_check_canary(int t) {
    if (t >= 0 && bsp_tasks[bsp_core()][t].stack != 0) {
        uint64_t c;
        __builtin_memcpy(&c, bsp_tasks[bsp_core()][t].stack, sizeof c);
        if (c != FASCON_STACK_CANARY) fascon_plat_kernel_panic(FASCON_PANIC_STACK, "task stack overflow (the canary at the bottom of the stack was overwritten)");
    }
}

/* ───── 4 KiB のページとタスクごとのアドレス空間（fascon_plat_vm_*） / 4 KiB pages and per-task address spaces ─────
 * タスクごとに PML4 と PDPT（カーネルの PDPT の項目を写す: 0〜4 GiB の恒等写像はカーネル専用の大域ページのまま）、その下に
 * ユーザーの範囲の PD と PT（ページ表の置き場から取る）。ASID は PCID（12 ビット）そのもの。PCID が無い CPU では CR3 を
 * 書くたびに大域でない項目が消える（それは CPU の性質で、避けられない）
 * every task has its own PML4 and PDPT (copying the entries of the kernel PDPT: the identity map of 0-4 GiB stays
 * kernel-only global pages), and below them the PD and PTs of the user range (taken from the page-table pool). An ASID is
 * the PCID (12 bits) itself. On CPUs without PCID every CR3 write drops the non-global entries (a property of the CPU
 * that cannot be avoided) */
#ifndef BSP_PT_PAGES
#define BSP_PT_PAGES 384u
#endif
#define BSP_SPACES (BSP_NCORE * FASCON2_TASK_MAX + 1u)
_Static_assert(BSP_SPACES <= 4096u, "every address space needs its own 12-bit PCID");
static uint64_t bsp_pt_pool[BSP_PT_PAGES][512] __attribute__((aligned(4096)));
static uint32_t bsp_pt_used[(BSP_PT_PAGES + 31u) / 32u];
static volatile uint32_t bsp_pt_lock;
static uint32_t bsp_pt_peak, bsp_pt_now;
typedef struct {
    uint64_t *pml4, *pdpt;
    uint64_t cpus; /* この PCID を CR3 に載せたコア / cores that have loaded this PCID into CR3 */
    uint32_t pages;
} BspSpace;
static BspSpace bsp_spaces[BSP_SPACES];
static uint32_t bsp_cur_asid[FASCON2_MAX_CPUS];
static uint8_t bsp_stale[FASCON2_MAX_CPUS][BSP_SPACES]; /* INVPCID の無い CPU: 次に載せるときに掃く / no INVPCID: flush on the next load */
static uint64_t bsp_vm_maps, bsp_vm_flush_local, bsp_vm_flush_ipi, bsp_vm_flush_served, bsp_vm_drops;

static uint64_t *bsp_pt_alloc(void) {
    const uint64_t im = fascon_plat_irq_save();
    f20_bitlock(&bsp_pt_lock);
    uint64_t *p = 0;
    for (uint32_t w = 0; w < (BSP_PT_PAGES + 31u) / 32u && !p; w++) {
        const uint32_t free = ~bsp_pt_used[w];
        if (free == 0u) continue;
        const uint32_t b = (uint32_t)__builtin_ctz(free);
        const uint32_t i = w * 32u + b;
        if (i >= BSP_PT_PAGES) break;
        bsp_pt_used[w] |= 1u << b;
        p = bsp_pt_pool[i];
        if (++bsp_pt_now > bsp_pt_peak) bsp_pt_peak = bsp_pt_now;
    }
    f20_bitunlock(&bsp_pt_lock);
    fascon_plat_irq_restore(im);
    if (!p) fascon_plat_kernel_panic(FASCON_PANIC_OOM, "vm: the page-table pool is exhausted (BSP_PT_PAGES)");
    for (uint32_t k = 0; k < 512u; k++) p[k] = 0;
    return p;
}
static void bsp_pt_free(uint64_t *p) {
    const uint32_t i = (uint32_t)((p - &bsp_pt_pool[0][0]) / 512);
    const uint64_t im = fascon_plat_irq_save();
    f20_bitlock(&bsp_pt_lock);
    bsp_pt_used[i / 32u] &= ~(1u << (i % 32u));
    bsp_pt_now--;
    f20_bitunlock(&bsp_pt_lock);
    fascon_plat_irq_restore(im);
}
static int bsp_pt_owned(uint64_t e) {
    const uint64_t a = e & PG_ADDR;
    return (e & PG_P) && !(e & PG_PS) && a >= (uint64_t)(uintptr_t)bsp_pt_pool && a < (uint64_t)(uintptr_t)bsp_pt_pool + sizeof bsp_pt_pool;
}
static inline uint64_t *bsp_pt_of(uint64_t e) { return (uint64_t *)(uintptr_t)(e & PG_ADDR); }

/* このコアの TLB から (asid, va) を消す（va = UINT64_MAX ならその PCID 全部）。いま載っている PCID なら INVLPG か CR3 の
 * 読み直し、載っていなければ INVPCID、それも無ければ「次に載せるときに掃く」印
 * remove (asid, va) from the TLB of this core (va = UINT64_MAX: the whole PCID). The PCID loaded now gets INVLPG or a CR3
 * reload; another PCID gets INVPCID, or without it a mark to flush when it is loaded next */
static void bsp_tlb_local(uint32_t asid, uint64_t va) {
    const uint32_t me = bsp_core();
    if (asid == bsp_cur_asid[me]) {
        if (va == UINT64_MAX) {
            const uint64_t pml4 = asid ? (uint64_t)(uintptr_t)bsp_spaces[asid].pml4 : (uint64_t)(uintptr_t)bsp_kpml4;
            bsp_set_cr3(pml4 | (bsp_pcid_on ? asid : 0u)); /* 掃かない印なし = その PCID を掃く / no no-flush bit = flush it */
        } else {
            __asm__ __volatile__("invlpg (%0)" : : "r"(va) : "memory");
        }
    } else if (bsp_pcid_on) {
        if (bsp_has_invpcid) {
            struct {
                uint64_t pcid, addr;
            } d = {asid, va == UINT64_MAX ? 0u : va};
            const uint64_t type = va == UINT64_MAX ? 1u : 0u;
            __asm__ __volatile__("invpcid %0, %1" : : "m"(d), "r"(type) : "memory");
        } else if (asid < BSP_SPACES) {
            bsp_stale[me][asid] = 1u;
        }
    }
    /* PCID なし: 載っていないアドレス空間の項目は、次に CR3 を書いたときに消える / without PCID the entries of a space that
     * is not loaded vanish at the next CR3 write */
}

bool fascon_plat_vm_space(uint32_t asid) {
    if (asid == 0u || asid >= BSP_SPACES) return false;
    if (bsp_spaces[asid].pml4) fascon_plat_vm_drop(asid);
    uint64_t *pml4 = bsp_pt_alloc(), *pdpt = bsp_pt_alloc();
    for (uint32_t k = 0; k < 512u; k++) pdpt[k] = bsp_kpdpt[k]; /* カーネルの項目（スーパーバイザ専用） / kernel entries (supervisor only) */
    for (uint32_t k = 1; k < 512u; k++) pml4[k] = bsp_kpml4[k];
    pml4[0] = (uint64_t)(uintptr_t)pdpt | PG_P | PG_RW | PG_US;
    __atomic_thread_fence(__ATOMIC_RELEASE);
    bsp_spaces[asid].pdpt = pdpt;
    bsp_spaces[asid].pml4 = pml4;
    bsp_spaces[asid].cpus = 0u;
    bsp_spaces[asid].pages = 0u;
    return true;
}

bool fascon_plat_vm_map(uint32_t asid, uint64_t va, uint64_t pa, uint32_t flags) {
    if (asid == 0u || asid >= BSP_SPACES || !bsp_spaces[asid].pml4 || ((va | pa) & 0xFFFu) != 0u || (va >> 39) != 0u) return false;
    uint64_t *e1 = &bsp_spaces[asid].pdpt[(va >> 30) & 511u];
    if (*e1 != 0u && !bsp_pt_owned(*e1)) return false; /* カーネルの範囲には写さない / never map over a kernel range */
    if (*e1 == 0u) *e1 = (uint64_t)(uintptr_t)bsp_pt_alloc() | PG_P | PG_RW | PG_US;
    uint64_t *e2 = &bsp_pt_of(*e1)[(va >> 21) & 511u];
    if (*e2 == 0u) *e2 = (uint64_t)(uintptr_t)bsp_pt_alloc() | PG_P | PG_RW | PG_US;
    uint64_t *pte = &bsp_pt_of(*e2)[(va >> 12) & 511u];
    uint64_t e = pa | PG_P;
    if (flags & F20_VM_WRITE) e |= PG_RW;
    if (flags & F20_VM_USER) e |= PG_US;
    if (!(flags & F20_VM_EXEC) && bsp_has_nx) e |= PG_NX;
    const uint64_t old = *pte;
    *pte = e;
    if (!(old & PG_P)) {
        bsp_spaces[asid].pages++; /* 無効 → 有効は TLB を掃かなくてよい / invalid → valid needs no flush */
    } else if (old != e) {
        fascon_plat_vm_flush(asid, va, bsp_spaces[asid].cpus);
    }
    __atomic_fetch_add(&bsp_vm_maps, 1u, __ATOMIC_RELAXED);
    return true;
}

/* ───── TLB の撃ち落とし（ベクタ 0xF0 の IPI）: 自分のコアは局所の命令、ほかのコアは郵便受けと IPI ─────
 *       TLB shootdown (IPI vector 0xF0): local instructions on this core, a mailbox and an IPI for the others
 * 送り手は 1 度に 1 つ（全体のロック）。ロックや返事を待つ間も自分の郵便受けを読むので、コアどうしが同時に頼み合っても
 * 止まらない / one sender at a time (a global lock); while waiting for the lock or for answers a core keeps serving its own
 * mailbox, so cores asking each other at the same time never deadlock */
static struct {
    volatile uint64_t va;
    volatile uint32_t asid, req, ack;
    uint8_t pad_[128 - 20];
} bsp_vm_mbox[BSP_NCORE] __attribute__((aligned(128)));
static FASCON_MAYBE_UNUSED volatile uint32_t bsp_vm_lock;
static void bsp_vm_serve(uint32_t me) {
    const uint32_t r = __atomic_load_n(&bsp_vm_mbox[me].req, __ATOMIC_ACQUIRE);
    if (r != bsp_vm_mbox[me].ack) {
        bsp_tlb_local(bsp_vm_mbox[me].asid, bsp_vm_mbox[me].va);
        __atomic_fetch_add(&bsp_vm_flush_served, 1u, __ATOMIC_RELAXED);
        __atomic_store_n(&bsp_vm_mbox[me].ack, r, __ATOMIC_RELEASE);
    }
}

void fascon_plat_vm_flush(uint32_t asid, uint64_t va, uint64_t cpu_mask) {
    const uint64_t im = fascon_plat_irq_save();
    const uint32_t me = bsp_core();
    if (va != UINT64_MAX) va &= ~0xFFFull;
    if (cpu_mask & (1ull << me)) {
        bsp_tlb_local(asid, va);
        __atomic_fetch_add(&bsp_vm_flush_local, 1u, __ATOMIC_RELAXED);
    }
#if defined(FASCON2_SMP)
    const uint64_t others = cpu_mask & ~(1ull << me) & ((1ull << bsp_cpu_count) - 1u);
    if (others != 0u) {
        while (__atomic_exchange_n(&bsp_vm_lock, 1u, __ATOMIC_ACQUIRE) != 0u) {
            bsp_vm_serve(me);
            FASCON_CPU_RELAX();
        }
        uint32_t want[BSP_NCORE] = {0};
        for (uint32_t c = 0; c < BSP_NCORE; c++) {
            if (!(others & (1ull << c))) continue;
            bsp_vm_mbox[c].asid = asid;
            bsp_vm_mbox[c].va = va;
            want[c] = __atomic_add_fetch(&bsp_vm_mbox[c].req, 1u, __ATOMIC_ACQ_REL);
            __atomic_fetch_add(&bsp_vm_flush_ipi, 1u, __ATOMIC_RELAXED);
            lapic_send(bsp_apic_id[c], VEC_TLB | (1u << 14)); /* 固定・物理宛先 / fixed, physical destination */
        }
        for (uint32_t c = 0; c < BSP_NCORE; c++) {
            if (!(others & (1ull << c))) continue;
            uint64_t spins = 0;
            while ((int32_t)(__atomic_load_n(&bsp_vm_mbox[c].ack, __ATOMIC_ACQUIRE) - want[c]) < 0) {
                bsp_vm_serve(me);
                FASCON_CPU_RELAX();
                if (++spins == 400000000ull) fascon_plat_kernel_panic(FASCON_PANIC_SMP, "vm: TLB shootdown: a core did not answer IPI 0xf0");
            }
        }
        __atomic_store_n(&bsp_vm_lock, 0u, __ATOMIC_RELEASE);
    }
#else
    (void)cpu_mask;
#endif
    fascon_plat_irq_restore(im);
}

/* このコアのアドレス空間を切り替える。PCID があれば「掃かない」印（bit 63）付きで書く（INVPCID の無い CPU で掃く印が
 * 付いた PCID だけは印なしで書いて掃く）/ switch the address space of this core. With PCID, CR3 is written with the
 * no-flush bit (bit 63), except for a PCID marked for flushing on CPUs without INVPCID, which is written without it */
void fascon_plat_vm_switch(uint32_t asid) {
    const uint64_t im = fascon_plat_irq_save();
    const uint32_t me = bsp_core();
    uint64_t cr3 = (uint64_t)(uintptr_t)bsp_kpml4;
    if (asid != 0u && asid < BSP_SPACES && bsp_spaces[asid].pml4) {
        cr3 = (uint64_t)(uintptr_t)bsp_spaces[asid].pml4;
        __atomic_fetch_or(&bsp_spaces[asid].cpus, 1ull << me, __ATOMIC_RELAXED);
    } else {
        asid = 0u;
    }
    if (asid != bsp_cur_asid[me] || (bsp_pcid_on && bsp_stale[me][asid])) {
        if (bsp_pcid_on) {
            cr3 |= asid;
            if (bsp_stale[me][asid]) {
                bsp_stale[me][asid] = 0u;
            } else {
                cr3 |= CR3_NOFLUSH;
            }
        }
        bsp_cur_asid[me] = asid;
        bsp_set_cr3(cr3);
    }
    fascon_plat_irq_restore(im);
}

uint64_t fascon_plat_vm_unmap(uint32_t asid, uint64_t va) {
    if (asid == 0u || asid >= BSP_SPACES || !bsp_spaces[asid].pml4) return 0u;
    const uint64_t e1 = bsp_spaces[asid].pdpt[(va >> 30) & 511u];
    if (!bsp_pt_owned(e1)) return 0u;
    const uint64_t e2 = bsp_pt_of(e1)[(va >> 21) & 511u];
    if (!bsp_pt_owned(e2)) return 0u;
    uint64_t *e = &bsp_pt_of(e2)[(va >> 12) & 511u];
    if (!(*e & PG_P)) return 0u;
    const uint64_t pa = *e & PG_ADDR;
    *e = 0u;
    bsp_spaces[asid].pages--;
    fascon_plat_vm_flush(asid, va & ~0xFFFull, bsp_spaces[asid].cpus); /* 使ったコアへだけ / only the cores that used it */
    return pa;
}

void fascon_plat_vm_drop(uint32_t asid) {
    if (asid == 0u || asid >= BSP_SPACES || !bsp_spaces[asid].pml4) return;
    const uint64_t im = fascon_plat_irq_save();
    if (bsp_cur_asid[bsp_core()] == asid) fascon_plat_vm_switch(0u); /* 先にカーネルの表へ / to the kernel tables first */
    uint64_t *pml4 = bsp_spaces[asid].pml4, *pdpt = bsp_spaces[asid].pdpt;
    bsp_spaces[asid].pml4 = 0;
    bsp_spaces[asid].pdpt = 0;
    /* 表を手放す前に、その PCID の TLB（ページ構造のキャッシュも）を、使ったコアだけで消す / before the tables are released,
     * remove that PCID from the TLBs (paging-structure caches included) of the cores that used it */
    fascon_plat_vm_flush(asid, UINT64_MAX, bsp_spaces[asid].cpus);
    for (uint32_t i = 0; i < 512u; i++) {
        if (!bsp_pt_owned(pdpt[i])) continue;
        uint64_t *pd = bsp_pt_of(pdpt[i]);
        for (uint32_t j = 0; j < 512u; j++)
            if (bsp_pt_owned(pd[j])) bsp_pt_free(bsp_pt_of(pd[j]));
        bsp_pt_free(pd);
    }
    bsp_pt_free(pdpt);
    bsp_pt_free(pml4);
    bsp_spaces[asid].cpus = 0u;
    bsp_spaces[asid].pages = 0u;
    bsp_vm_drops++;
    fascon_plat_irq_restore(im);
}

/* ───── ユーザー空間（Ring 3）─────
 * ユーザーメモリはスロットごと（マルチコアでは [コア][スロット]）の物理の置き場で、そのタスクのアドレス空間の user_mem の
 * 番地へ 4 KiB ずつ要求時に写す。カーネルの窓（umem.of）は恒等写像の側から同じ物理ページを見る
 * user memory is a physical area per slot ([core][slot] on multi-core), mapped 4 KiB at a time on demand at the user_mem
 * address of the address space of that task; kernel windows (umem.of) see the same physical pages through the identity map */
static uint8_t *bsp_umem = 0;
static uint64_t bsp_usize = 0, bsp_uva = 0;
static uint32_t bsp_uslots = 0;
static const FasconSyscallFn *bsp_syscalls = 0;
static uint32_t bsp_syscall_count = 0;
static void (*bsp_trap)(uint64_t, uint64_t, uint64_t) = 0;

void fascon_plat_syscall_register(const FasconSyscallFn *table, uint32_t count) {
    bsp_syscalls = table;
    bsp_syscall_count = count;
}

void fascon_plat_trap_register(void (*handler)(uint64_t, uint64_t, uint64_t)) { bsp_trap = handler; }

static inline uint32_t bsp_asid_of(uint32_t core, uint32_t slot) { return 1u + core * FASCON2_TASK_MAX + slot; }
static inline uint8_t *bsp_umem_of(uint32_t core, uint32_t slot) { return bsp_umem + ((uint64_t)core * bsp_uslots + slot) * bsp_usize; }

void fascon_plat_user_register(uint8_t *mem, uint64_t size, uint64_t va, uint32_t slots) {
    /* x86_64: 4 GiB 以上（0〜4 GiB はカーネルの恒等写像）、512 GiB 未満（PML4 の 0 番）、1 つの 1 GiB の枠の中
     * x86_64: at or above 4 GiB (0-4 GiB is the identity map of the kernel), below 512 GiB (PML4 entry 0), inside one
     * 1 GiB slot */
    if (((uint64_t)(uintptr_t)mem & 0xFFFull) != 0 || size == 0 || (size & 0xFFFull) != 0 || (va & 0xFFFull) != 0 || (va >> 30) < 4u || (va >> 39) != 0u || (va >> 30) != ((va + size - 1u) >> 30))
        fascon_plat_kernel_panic(FASCON_PANIC_TASK, "user_mem: bad placement (x86_64: 4 KiB aligned, from 4 GiB up to 512 GiB, inside one 1 GiB slot)");
    bsp_umem = mem;
    bsp_usize = size;
    bsp_uva = va;
    bsp_uslots = slots;
    puts_raw("[bsp] user_mem: ");
    put_dec(slots);
    puts_raw(" slot(s) x ");
    put_dec(size >> 10);
    puts_raw(" KiB");
#if defined(FASCON2_SMP)
    puts_raw(" per core");
#endif
    puts_raw(" at ");
    put_hex(va);
    puts_raw(", ring 3, 4 KiB pages on demand, one address space per task (");
    puts_raw(bsp_pcid_on ? "CR3 with a 12-bit PCID" : "CR3 without PCID");
    puts_raw(bsp_pcid_on ? "), no TLB flush on task switches\n" : "), each CR3 load drops non-global TLB entries\n");
}

/* いまのタスクのユーザーメモリの中か（番地の形だけ。中身は読まない） / whether a range lies in the user memory of the current
 * task (addresses only; nothing is read) */
static int bsp_user_range(uint64_t addr, uint64_t len) {
    const BspTask *t = bsp_cur_task();
    if (!t || !t->user || bsp_umem == 0) return 0;
    return addr >= bsp_uva && len <= bsp_usize && addr - bsp_uva <= bsp_usize - len;
}

bool fascon_plat_user_window(uint64_t addr, uint64_t len, f20_slice_t *out) {
    if (!bsp_user_range(addr, len)) return false;
    out->p = bsp_umem_of(bsp_core(), (uint32_t)bsp_sched()->cur) + (addr - bsp_uva);
    out->n = len;
    return true;
}

/* 要求時ページング: いまの Ring 3 のタスクのユーザーメモリの中の、まだ写していないページ（#PF の P = 0）なら 4 KiB を写す
 * demand paging: a not-yet-mapped page (P = 0 in the #PF error code) inside the user memory of the current ring 3 task
 * gets 4 KiB mapped */
static int bsp_demand_map(uint64_t cr2, uint64_t err) {
    if (err & 1u) return 0; /* 写してあるページの権限違反は対象外 / a protection fault on a mapped page is not ours */
    BspTask *t = bsp_cur_task();
    if (!t || !t->user || bsp_umem == 0 || cr2 < bsp_uva || cr2 - bsp_uva >= bsp_usize) return 0;
    const uint64_t va = cr2 & ~0xFFFull;
    const uint64_t pa = (uint64_t)(uintptr_t)bsp_umem_of(bsp_core(), (uint32_t)bsp_sched()->cur) + (va - bsp_uva);
    if (!fascon_plat_vm_map(t->asid, va, pa, F20_VM_READ | F20_VM_WRITE | F20_VM_EXEC | F20_VM_USER)) return 0;
    bsp_sched()->faults++;
    return 1;
}

/* ユーザーメモリの写し: SMAP があれば STAC / CLAC の間だけ Ring 3 のページに触れる。番地はまずユーザーメモリの範囲で検査し
 * （カーネルの番地は通さない）、まだ写していないページは #PF で要求時に写してやり直す。ほかのフォールトは例外表の飛び先が
 * err = 1 にする / user copies: with SMAP, ring 3 pages are touched only between STAC and CLAC. The address is first
 * checked against the user memory range (kernel addresses never pass), a page not mapped yet is mapped on demand by the
 * #PF and the access is retried; any other fault makes the exception-table fixup set err = 1 */
static inline uint32_t bsp_get_user8(uint64_t ua, uint8_t *out) {
    uint8_t v = 0;
    uint32_t err = 0;
    __asm__ __volatile__("1: movb (%2), %0\n"
                         "2:\n"
                         ".pushsection __ex_table, \"a\"\n.balign 8\n.quad 1b, 3f\n.popsection\n"
                         ".pushsection .text.fixup, \"ax\"\n3: movl $1, %1\n jmp 2b\n.popsection\n"
                         : "+q"(v), "+r"(err)
                         : "r"(ua)
                         : "memory");
    *out = v;
    return err;
}
static inline uint32_t bsp_put_user8(uint64_t ua, uint8_t v) {
    uint32_t err = 0;
    __asm__ __volatile__("1: movb %1, (%2)\n"
                         "2:\n"
                         ".pushsection __ex_table, \"a\"\n.balign 8\n.quad 1b, 3f\n.popsection\n"
                         ".pushsection .text.fixup, \"ax\"\n3: movl $1, %0\n jmp 2b\n.popsection\n"
                         : "+r"(err)
                         : "q"(v), "r"(ua)
                         : "memory");
    return err;
}
static inline void bsp_stac(void) {
    if (bsp_smap_on) __asm__ __volatile__("stac" ::: "memory");
}
static inline void bsp_clac(void) {
    if (bsp_smap_on) __asm__ __volatile__("clac" ::: "memory");
}
uint64_t fascon_plat_copy_from_user(void *dst, uint64_t uaddr, uint64_t n) {
    if (!bsp_user_range(uaddr, n)) return n;
    bsp_stac();
    for (uint64_t i = 0; i < n; i++) {
        if (bsp_get_user8(uaddr + i, (uint8_t *)dst + i)) {
            bsp_clac();
            return n - i;
        }
    }
    bsp_clac();
    return 0u;
}
uint64_t fascon_plat_copy_to_user(uint64_t uaddr, const void *src, uint64_t n) {
    if (!bsp_user_range(uaddr, n)) return n;
    bsp_stac();
    for (uint64_t i = 0; i < n; i++) {
        if (bsp_put_user8(uaddr + i, ((const uint8_t *)src)[i])) {
            bsp_clac();
            return n - i;
        }
    }
    bsp_clac();
    return 0u;
}

typedef struct {
    uint64_t insn, fixup;
} bsp_ex_t;
extern const bsp_ex_t __start___ex_table[] __attribute__((weak));
extern const bsp_ex_t __stop___ex_table[] __attribute__((weak));
static uint64_t bsp_find_fixup(uint64_t pc) {
    for (const bsp_ex_t *e = __start___ex_table; e && e < __stop___ex_table; e++)
        if (e->insn == pc) return e->fixup;
    return 0;
}

#if defined(FASCON2_GC)
/* fascon-gc: タスクごとのシャドウスタック。切り替えで保存・復元し、回収は止まっているタスクの分も根として読む
 * fascon-gc: one shadow stack per task; saved and restored at switches, and the collector reads suspended tasks too */
void *fascon_gc_top_get(void);
void fascon_gc_top_set(void *p);
uint32_t fascon_plat_gc_task_tops(void **out, uint32_t max) {
    uint32_t n = 0;
    for (uint32_t c = 0; c < BSP_NCORE; c++) {
        const int cur = fascon_cpu_sched(c)->cur;
        for (int i = 0; i < (int)FASCON2_TASK_MAX && n < max; i++)
            if (bsp_tasks[c][i].alive && !(c == bsp_core() && i == cur) && bsp_tasks[c][i].gc_top != 0) out[n++] = bsp_tasks[c][i].gc_top;
    }
    return n;
}
#endif

static BspFrame *bsp_switch(int to, BspFrame *frame, int save) {
    const uint32_t c = bsp_core();
    f20_sched_t *S = fascon_cpu_sched(c);
    const int from = S->cur;
    bsp_check_canary(from);
    if (from >= 0 && save) {
        BspTask *f = &bsp_tasks[c][from];
        f->sp = frame;
        f->f_top = fascon__frame.top;
        f->f_end = fascon__frame.end;
        f->f_base = fascon__frame.base;
#if defined(FASCON2_GC)
        f->gc_top = fascon_gc_top_get();
#endif
    }
    BspTask *t = &bsp_tasks[c][to];
    S->cur = to;
    S->switches++;
    /* Ring 3 のタスク: そのアドレス空間へ（PCID があれば掃かない印つき）。カーネルのタスク: いま載っている空間のまま（カーネルの
     * 半分はどの空間でも同じで、ユーザーの半分は SMEP / SMAP が守る）。PCID の無い CPU でも、ユーザー → カーネル → 同じ
     * ユーザーの切り替えは CR3 を 1 度も書かない / a ring 3 task: its address space (with the no-flush bit when PCID exists).
     * A kernel task: whatever space is loaded stays (the kernel half is the same in every space and SMEP / SMAP guard the
     * user half), so even without PCID a switch user → kernel → the same user never writes CR3 */
    if (t->user) {
        fascon_plat_vm_switch(t->asid);
        bsp_tss[c].rsp0 = (uint64_t)(uintptr_t)(t->ktop - 16); /* Ring 3 から入るときのスタック / the stack used on entry from ring 3 */
    }
    fascon__frame.top = t->f_top;
    fascon__frame.end = t->f_end;
    fascon__frame.base = t->f_base;
#if defined(FASCON2_GC)
    fascon_gc_top_set(t->gc_top);
#if defined(FASCON2_SMP)
    /* 共有の gc_arena の回収中に切り替えたなら、新しいタスクを走らせる前にここで止まる（新しいタスクの鎖を公開する）。
     * 切り替えの記録（cur・gc_top）を書いてから回収中かを読むので、回収するコアは走っているタスクの鎖を歩かない
     * if a shared gc_arena is being collected, park here before the new task runs (publishing its chain). The switch
     * record (cur, gc_top) is written before the check, so the collecting core never walks the chain of a running task */
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
    if (__atomic_load_n(&fascon__gc_req, __ATOMIC_ACQUIRE) & 1u) f20_gc_park();
#endif
#endif
    return t->sp;
}

/* コア 0 を起こす（コアのタスクが尽きた・spawn_on の関数が終わった: コア 0 が全コアの終わりを確かめる）
 * wake core 0 (the tasks of a core ran out, or a spawn_on function returned: core 0 checks whether every core is done) */
static FASCON_MAYBE_UNUSED void bsp_wake_core0(void) {
#if defined(FASCON2_SMP)
    if (bsp_core() != 0u) {
        __atomic_thread_fence(__ATOMIC_SEQ_CST);
        lapic_send(bsp_apic_id[0], VEC_WAKE | (1u << 14));
    }
#endif
}

/* 割り込み（LAPIC タイマー・I/O APIC・IPI）。戻り値 = 復帰するフレーム / interrupts (LAPIC timer, I/O APIC, IPIs); returns
 * the frame to restore */
static BspFrame *bsp_irq(BspFrame *frame, uint32_t v) {
    const uint32_t me = bsp_core();
    if (v == VEC_TLB) {
        bsp_vm_serve(me);
        lapic_eoi();
        return frame;
    }
    if (v == VEC_WAKE) {
        lapic_eoi();
        return frame;
    }
    if (v >= 0x20u && v < 0x30u) return frame; /* 8259 のスプリアス（マスク済み。EOI は送らない） / 8259 spurious (masked; no EOI) */
    f20_sched_t *S = bsp_sched();
    uint32_t intid;
    if (v == VEC_TIMER) {
        intid = bsp_timer_intid;
        S->ticks++;
    } else if (v >= VEC_GSI0 && v < VEC_GSI0 + GSI_COUNT) {
        intid = 32u + (v - VEC_GSI0);
    } else {
        fascon_plat_kernel_panic(FASCON_PANIC_IRQ, "unexpected interrupt vector");
    }
    void (*h)(void) = 0;
    for (uint32_t i = 0; i < bsp_irq_count; i++) {
        if (bsp_irqs[i].intid == intid) h = bsp_irqs[i].handler;
    }
    if (h == 0) fascon_plat_kernel_panic(FASCON_PANIC_IRQ, "interrupt without an irq handler");
    S->in_irq = 1;
    h();
    S->in_irq = 0;
    lapic_eoi();
    if (S->pending >= 0) {
        const int to = S->pending;
        S->pending = -1;
        if (S->cur >= 0 && to != S->cur && bsp_tasks[me][to].alive) frame = bsp_switch(to, frame, 1);
    }
    return frame;
}

/* コアのタスクがすべて終わったときの文脈（コアごとの小さなスタックの上。割り込みは開く） / the context used when every task
 * of a core is gone (on a small per-core stack, interrupts open) */
#define BSP_IDLE (-2)
static uint8_t bsp_idle_stack[BSP_NCORE][16384] __attribute__((aligned(16)));
static void bsp_idle_loop(void);
static BspFrame *bsp_kframe(uint8_t *top, uint64_t rip) {
    BspFrame *f = (BspFrame *)(void *)(top - 16 - sizeof(BspFrame));
    __builtin_memset(f, 0, sizeof *f);
    bsp_fx_init(f);
    f->rip = rip;
    f->cs = SEL_KCODE;
    f->ss = SEL_KDATA;
    f->rflags = 0x202u;                     /* IF = 1 */
    f->rsp = (uint64_t)(uintptr_t)(top - 24); /* 呼ばれた直後と同じ（RSP + 8 が 16 の倍数） / as right after a call */
    return f;
}
static BspFrame *bsp_idle_frame(uint32_t c) { return bsp_kframe(bsp_idle_stack[c] + sizeof bsp_idle_stack[c], (uint64_t)(uintptr_t)bsp_idle_loop); }

/* int $0x81 = 自発的な切り替え / int $0x81 = a voluntary switch */
static BspFrame *bsp_svc(BspFrame *frame) {
    f20_sched_t *S = bsp_sched();
    const int to = S->svc_to;
    S->svc_to = -1;
    if (to == BSP_IDLE) {
        S->cur = -1;
        fascon_plat_vm_switch(0u);
        return bsp_idle_frame(bsp_core());
    }
    if (to < 0 || !bsp_tasks[bsp_core()][to].alive) return frame;
    return bsp_switch(to, frame, S->svc_save);
}

static void bsp_task_trampoline(void) {
    BspTask *t = bsp_cur_task();
    t->entry(t->arg);
    t->on_exit(); /* 入口から戻った = 終了 / returning from the entry = exit */
    for (;;) {
    }
}

bool fascon_plat_task_create(uint32_t slot, f20_id_t id, void (*entry)(uint64_t), uint64_t arg, void *stack, size_t stack_size, void (*on_exit)(void)) {
    if (slot >= FASCON2_TASK_MAX || stack_size < 4096u) return false;
    const uint32_t c = bsp_core();
    BspTask *t = &bsp_tasks[c][slot];
    const size_t fr = (stack_size / 4u) & ~(size_t)15u;
    uint8_t *base = (uint8_t *)stack;
    const uint64_t canary = FASCON_STACK_CANARY;
    __builtin_memcpy(base, &canary, sizeof canary);
    uint8_t *top = (uint8_t *)(((uintptr_t)base + stack_size) & ~(uintptr_t)15u);
    /* 一番上の 8 バイト: このコアの FS の土台（Ring 3 から入ったとき bios.S が FS へ戻す） / the top 8 bytes: the FS base of
     * this core (bios.S puts it back into FS on entry from ring 3) */
    const uint64_t tp = bsp_tp[c];
    __builtin_memcpy(top - 16, &tp, sizeof tp);
    t->sp = bsp_kframe(top, (uint64_t)(uintptr_t)bsp_task_trampoline);
    t->ktop = top;
    t->id = id;
    if (!t->alive) bsp_sched()->live++;
    t->alive = 1;
    t->entry = entry;
    t->arg = arg;
    t->on_exit = on_exit;
    t->stack = base;
    t->f_base = base + 16;
    t->f_top = t->f_base;
#if defined(FASCON2_GC)
    t->gc_top = 0;
#endif
    t->f_end = base + fr;
    t->user = 0;
    t->asid = 0;
    return true;
}

/* Ring 3 のタスク: フレームは Ring 3 のコードとデータのセグメント・IF = 1・全レジスタ 0。カーネルスタックの一番上（TSS の
 * RSP0 の直下）に置くので、Ring 3 からの割り込みはいつも同じ場所にフレームを積む。アドレス空間は空で始まり、ページは
 * 触れたときに写す / a ring 3 task: its frame has the ring 3 code and data segments, IF = 1 and every register zero. It
 * sits at the very top of the kernel stack (right below RSP0 of the TSS), so interrupts from ring 3 always push their frame
 * at the same place. The address space starts empty and pages are mapped when touched */
bool fascon_plat_task_create_user(uint32_t slot, f20_id_t id, uint64_t entry, uint64_t sp, void *stack, size_t stack_size, void (*on_exit)(void)) {
    if (bsp_umem == 0 || slot >= bsp_uslots) return false;
    if (!fascon_plat_task_create(slot, id, 0, 0, stack, stack_size, on_exit)) return false;
    const uint32_t c = bsp_core();
    BspTask *t = &bsp_tasks[c][slot];
    __builtin_memset(bsp_umem_of(c, slot), 0, (size_t)bsp_usize); /* 前のタスクの中身を残さない / nothing of an earlier task is left */
    t->asid = bsp_asid_of(c, slot);
    if (!fascon_plat_vm_space(t->asid)) return false;
    t->sp->rip = entry;
    t->sp->cs = SEL_UCODE;
    t->sp->ss = SEL_UDATA;
    t->sp->rsp = sp & ~15ull;
    t->sp->rflags = 0x202u;
    t->user = 1;
    return true;
}

/* Ring 3 の例外の形: AArch64 の ESR と同じ区分（bit 31:26 が種類、bit 25 = IL）で trap のハンドラへ渡す。同じ Fascon の
 * trap を両方のアーキテクチャで書ける / the shape of a ring 3 exception handed to the trap handler: the same classes as the
 * AArch64 ESR (bits 31:26 the class, bit 25 = IL), so one Fascon trap is written for both architectures
 *   #UD → 0x00（未定義命令、ISS 0） / 0x00 (undefined instruction, ISS 0)
 *   #PF → 0x24 データ / 0x20 命令の取り出し（DFSC 0x07 = 未変換、0x0F = 権限。書き込みは WnR） / 0x24 data, 0x20 instruction
 *         fetch (DFSC 0x07 = translation, 0x0F = permission; writes set WnR)
 *   #AC → 0x24（DFSC 0x21 = 整列） / 0x24 (DFSC 0x21 = alignment)、int3 → 0x3C（BRK）、#DB → 0x32
 *   #DE・#MF・#XM → 0x2C（算術の例外） / 0x2C (arithmetic exception)
 *   それ以外（#GP など: 特権命令・I/O ポート・不正なセグメント）→ 0x00、ISS = x86 のベクタ番号
 *   anything else (#GP and the like: privileged instructions, I/O ports, bad segments) → 0x00 with ISS = the x86 vector */
static uint64_t bsp_syndrome(uint64_t vec, uint64_t err) {
    const uint64_t il = 1ull << 25;
    switch (vec) {
    case 6u:
        return il;
    case 14u: {
        const uint64_t ec = (err & 0x10u) ? 0x20u : 0x24u;
        const uint64_t wnr = (ec == 0x24u && (err & 2u)) ? (1u << 6) : 0u;
        return (ec << 26) | il | wnr | ((err & 1u) ? 0x0Fu : 0x07u);
    }
    case 17u:
        return (0x24ull << 26) | il | 0x21u;
    case 3u:
        return (0x3Cull << 26) | il;
    case 1u:
        return (0x32ull << 26) | il;
    case 0u:
    case 16u:
    case 19u:
        return (0x2Cull << 26) | il;
    default:
        return il | (vec & 0xFFu);
    }
}

/* Ring 3 の int $0x80: 番号は rax、引数は rdi・rsi・rdx・r10・r8・r9・r12・r13（fascon_user_svc と同じ）。本体はタスクの
 * 文脈で動く（割り込み可・切り替え可）。フレームはこのタスクのスタックの上なので動かない
 * int $0x80 from ring 3: the number in rax, arguments in rdi, rsi, rdx, r10, r8, r9, r12, r13 (as fascon_user_svc). The body
 * runs in task context (interruptible, may switch); the frame stays put on the stack of this task */
static BspFrame *bsp_user_syscall(BspFrame *f) {
    const uint64_t nr = f->rax;
    int64_t r = -38; /* ENOSYS */
    if (nr < bsp_syscall_count && bsp_syscalls[nr] != 0) {
        const uint64_t a[8] = {f->rdi, f->rsi, f->rdx, f->r10, f->r8, f->r9, f->r12, f->r13};
        __asm__ __volatile__("sti" ::: "memory");
        r = bsp_syscalls[nr](a);
        __asm__ __volatile__("cli" ::: "memory");
    }
    f->rax = (uint64_t)r;
    return f;
}

static const char *const exc_names[32] = {
    "#DE divide error", "#DB debug", "NMI", "#BP breakpoint", "#OF overflow", "#BR bound", "#UD invalid opcode", "#NM no FPU",
    "#DF double fault", "coprocessor", "#TS invalid TSS", "#NP segment not present", "#SS stack fault", "#GP general protection",
    "#PF page fault", "reserved", "#MF x87 error", "#AC alignment check", "#MC machine check", "#XM SIMD error", "#VE virtualization",
    "#CP control protection", "reserved", "reserved", "reserved", "reserved", "reserved", "reserved", "#HV", "#VC", "#SX", "reserved",
};

static FASCON_NORETURN void bsp_fatal(const BspFrame *f, uint64_t cr2) {
    puts_raw("\nCPU EXCEPTION ");
    put_dec(f->vector);
    puts_raw(" (");
    puts_raw(exc_names[f->vector & 31u]);
    puts_raw(") err=");
    put_hex(f->error);
    puts_raw(" rip=");
    put_hex(f->rip);
    puts_raw(" cr2=");
    put_hex(cr2);
#if defined(FASCON2_SMP)
    puts_raw(" core=");
    put_dec(bsp_core());
#endif
    puts_raw("\n");
    power_off(71);
}

/* Ring 3 の例外: まだ写していないページは要求時に写してやり直し、それ以外はそのタスクの不正な動作（trap のハンドラを
 * 呼び、タスクを終わらせる） / an exception from ring 3: a page not mapped yet is mapped on demand and retried; anything
 * else is a fault of the task (the trap handler runs and the task ends) */
static BspFrame *bsp_user_fault(BspFrame *f) {
    uint64_t far = 0;
    if (f->vector == VEC_PF) {
        far = bsp_cr2();
        if (bsp_demand_map(far, f->error)) return f;
    }
    const uint64_t esr = bsp_syndrome(f->vector, f->error), pc = f->rip;
    if (bsp_trap != 0) {
        __asm__ __volatile__("sti" ::: "memory");
        bsp_trap(esr, far, pc);
        __asm__ __volatile__("cli" ::: "memory");
    } else {
        puts_raw("\n[bsp] user fault vector=");
        put_dec(f->vector);
        puts_raw(" err=");
        put_hex(f->error);
        puts_raw(" rip=");
        put_hex(pc);
        puts_raw(" cr2=");
        put_hex(far);
        puts_raw("\n");
    }
    /* ハンドラが戻った（または無い）: そのタスクだけを終わらせる / the handler returned (or there is none): end only that task */
    bsp_cur_task()->on_exit();
    fascon_plat_task_exit();
}

/* Ring 0 の例外: ユーザーメモリの写し（例外表のある命令）がまだ写していないページに触れたら要求時に写してやり直し、そうで
 * なければ例外表の飛び先へ（EFAULT の道）。それ以外は致命的 / an exception at ring 0: a user copy (an instruction with an
 * exception-table entry) touching a page not mapped yet is mapped on demand and retried, otherwise it goes to its fixup
 * (the EFAULT path); anything else is fatal */
static BspFrame *bsp_kernel_fault(BspFrame *f) {
    const uint64_t cr2 = f->vector == VEC_PF ? bsp_cr2() : 0u;
    if (f->vector == VEC_PF) {
        const uint64_t fx = bsp_find_fixup(f->rip);
        if (fx && bsp_demand_map(cr2, f->error)) return f;
        if (fx) {
            f->rip = fx;
            return f;
        }
    }
    bsp_fatal(f, cr2);
}

/* bios.S の isr_full から: 全ベクタの入口 / from isr_full in bios.S: the entry of every vector */
BspFrame *bsp_x86_trap(BspFrame *f);
BspFrame *bsp_x86_trap(BspFrame *f) {
    if (bsp_smap_on) __asm__ __volatile__("clac" ::: "memory"); /* Ring 3 が AC を立てていても、カーネルは SMAP の下で動く / the kernel runs under SMAP even if ring 3 set AC */
    const uint32_t v = (uint32_t)f->vector;
    const int from_user = (f->cs & 3u) != 0u;
    if (v == VEC_NMI) {
#if defined(FASCON2_SMP)
        if (bsp_panic_sent) bsp_panic_stop();
#endif
        return f; /* パニック以外の NMI は無視 / NMIs other than the panic stop are ignored */
    }
    if (v < 32u) return from_user ? bsp_user_fault(f) : bsp_kernel_fault(f);
    if (v == VEC_SYSCALL) {
        if (from_user) return bsp_user_syscall(f);
        bsp_fatal(f, 0u);
    }
    if (v == VEC_SVC) return bsp_svc(f);
    if (v == VEC_SPURIOUS) return f;
    return bsp_irq(f, v);
}

f20_id_t fascon_plat_task_current(void) {
    const BspTask *t = bsp_cur_task();
    return t ? t->id : 0u;
}

static inline void bsp_int81(void) { __asm__ __volatile__("int $0x81" ::: "memory"); }

void fascon_plat_task_yield(uint32_t slot, f20_id_t id) {
    f20_sched_t *S = bsp_sched();
    if (S->in_irq) fascon_plat_kernel_panic(FASCON_PANIC_TASK, "yield_to inside an interrupt handler");
    BspTask *t = &bsp_tasks[bsp_core()][slot % FASCON2_TASK_MAX];
    if (S->cur < 0 || slot >= FASCON2_TASK_MAX || (int)slot == S->cur || !t->alive || t->id != id) return;
    const uint64_t d = fascon_plat_irq_save();
    S->svc_to = (int)slot;
    S->svc_save = 1;
    bsp_int81();
    fascon_plat_irq_restore(d);
}

void fascon_plat_task_resume(uint32_t slot, f20_id_t id) {
    const BspTask *t = &bsp_tasks[bsp_core()][slot % FASCON2_TASK_MAX];
    if (slot < FASCON2_TASK_MAX && t->alive && t->id == id) bsp_sched()->pending = (int)slot;
}

FASCON_NORETURN void fascon_plat_task_enter(uint32_t slot, f20_id_t id) {
    const BspTask *t = &bsp_tasks[bsp_core()][slot % FASCON2_TASK_MAX];
    if (slot >= FASCON2_TASK_MAX || !t->alive || t->id != id) fascon_plat_kernel_panic(FASCON_PANIC_TASK, "enter: the task does not exist");
    (void)fascon_plat_irq_save();
    f20_sched_t *S = bsp_sched();
    S->svc_to = (int)slot;
    S->svc_save = 0;
    bsp_int81();
    fascon_plat_kernel_panic(FASCON_PANIC_TASK, "enter returned");
}

FASCON_NORETURN void fascon_plat_task_exit(void) {
    (void)fascon_plat_irq_save();
    f20_sched_t *S = bsp_sched();
    const uint32_t c = bsp_core();
    if (S->cur >= 0) {
        bsp_check_canary(S->cur);
        BspTask *t = &bsp_tasks[c][S->cur];
        t->alive = 0;
        if (S->live) S->live--;
        if (t->user) fascon_plat_vm_drop(t->asid); /* ページの写しを外し、その PCID だけを掃く / unmap and flush that PCID only */
        for (uint32_t k = 1; k <= FASCON2_TASK_MAX; k++) {
            const uint32_t i = ((uint32_t)S->cur + k) % FASCON2_TASK_MAX;
            if (bsp_tasks[c][i].alive) {
                S->svc_to = (int)i;
                S->svc_save = 0;
                bsp_int81();
            }
        }
    }
#if defined(FASCON2_SMP)
    /* マルチコア: このコアのタスクは尽きた。ほかのコアはまだ動いているかもしれないので、機械は止めずに休む（コア 0 は全コアの
     * タスクが尽きたら終わる） / multi-core: this core has no task left; other cores may still run, so the machine is not
     * stopped and the core idles (core 0 ends the run once every core is out of tasks) */
    bsp_wake_core0();
    S->svc_to = BSP_IDLE;
    S->svc_save = 0;
    bsp_int81();
    fascon_plat_kernel_panic(FASCON_PANIC_TASK, "idle returned");
#else
    fascon_plat_exit(0); /* 生きているタスクが無い / no live task is left */
#endif
}

/* ───────────── fw_cfg + ramfb ───────────── */

typedef struct {
    uint32_t control;
    uint32_t length;
    uint64_t address;
} __attribute__((packed)) FwCfgDma;

typedef struct {
    uint64_t addr;
    uint32_t fourcc;
    uint32_t flags;
    uint32_t width;
    uint32_t height;
    uint32_t stride;
} __attribute__((packed)) RamfbCfg;

#define FW_CFG_DMA_ERROR 0x01u
#define FW_CFG_DMA_READ 0x02u
#define FW_CFG_DMA_SELECT 0x08u
#define FW_CFG_DMA_WRITE 0x10u
#define FW_CFG_FILE_DIR 0x19u
#define FW_CFG_NB_CPUS 0x05u

static int fwcfg_dma(uint32_t control, void *buf, uint32_t len) {
    static volatile FwCfgDma d __attribute__((aligned(16)));
    d.control = __builtin_bswap32(control);
    d.length = __builtin_bswap32(len);
    d.address = __builtin_bswap64((uint64_t)(uintptr_t)buf);
    __asm__ __volatile__("mfence" ::: "memory");
    const uint64_t pa = (uint64_t)(uintptr_t)&d;
    /* DMA アドレスレジスタはビッグエンディアン 64 ビット。下位（0x518）を書いた瞬間に転送が始まる
     * the DMA address register is big-endian 64-bit; writing the low half (0x518) starts the transfer */
    fascon_outl(FWCFG_DMA_HI, __builtin_bswap32((uint32_t)(pa >> 32)));
    fascon_outl(FWCFG_DMA_LO, __builtin_bswap32((uint32_t)pa));
    for (int spin = 0; spin < 1000000; spin++) {
        const uint32_t c = __builtin_bswap32(d.control);
        if ((c & ~FW_CFG_DMA_ERROR) == 0) return (c & FW_CFG_DMA_ERROR) ? -1 : 0;
    }
    return -1;
}

static int fwcfg_signature(void) {
    fascon_outw(FWCFG_SEL, 0x0000u); /* 署名 / signature */
    char sig[4];
    for (int i = 0; i < 4; i++) sig[i] = (char)fascon_inb(FWCFG_DATA);
    return sig[0] == 'Q' && sig[1] == 'E' && sig[2] == 'M' && sig[3] == 'U';
}

static int fwcfg_present(void) {
    if (!fwcfg_signature()) return 0;
    fascon_outw(FWCFG_SEL, 0x0001u); /* 機能: bit 1 = DMA / features: bit 1 = DMA */
    uint32_t f = 0;
    for (int i = 0; i < 4; i++) f |= (uint32_t)fascon_inb(FWCFG_DATA) << (8 * i);
    return (f & 2u) != 0u;
}

static int fwcfg_find(const char *name, uint16_t *sel) {
    uint32_t count_be = 0;
    if (fwcfg_dma((FW_CFG_FILE_DIR << 16) | FW_CFG_DMA_SELECT | FW_CFG_DMA_READ, &count_be, 4) != 0) return 0;
    const uint32_t count = __builtin_bswap32(count_be);
    for (uint32_t i = 0; i < count && i < 512; i++) {
        struct {
            uint32_t size;
            uint16_t select;
            uint16_t reserved;
            char name[56];
        } __attribute__((packed)) f;
        if (fwcfg_dma(FW_CFG_DMA_READ, &f, sizeof f) != 0) return 0;
        int eq = 1;
        for (int k = 0; k < 56; k++) {
            if (f.name[k] != name[k]) {
                eq = 0;
                break;
            }
            if (name[k] == 0) break;
        }
        if (eq) {
            *sel = __builtin_bswap16(f.select);
            return 1;
        }
    }
    return 0;
}

static void setup_framebuffer(void) {
    const FasconMmioRegion *fb = NULL;
    for (uint32_t i = 0; i < fascon2_mmio_region_count; i++)
        if (fascon2_mmio_regions[i].flags & FASCON_MMIO_FRAMEBUFFER) fb = &fascon2_mmio_regions[i];
    if (fb == NULL) return;
    if (fb->bpp != 32) {
        puts_raw("[bsp] framebuffer must have 32-bit pixels (XRGB8888)\n");
        return;
    }
    uint16_t sel = 0;
    if (!fwcfg_present() || !fwcfg_find("etc/ramfb", &sel)) {
        puts_raw("[bsp] ramfb not found: start QEMU with -vga none -device ramfb to see the screen\n");
        return;
    }
    static RamfbCfg cfg __attribute__((aligned(16)));
    cfg.addr = __builtin_bswap64((uint64_t)fb->base);
    cfg.fourcc = __builtin_bswap32(0x34325258u); /* DRM_FORMAT_XRGB8888 = XR24 */
    cfg.flags = 0;
    cfg.width = __builtin_bswap32(fb->width);
    cfg.height = __builtin_bswap32(fb->height);
    cfg.stride = __builtin_bswap32(fb->width * 4u);
    if (fwcfg_dma(((uint32_t)sel << 16) | FW_CFG_DMA_SELECT | FW_CFG_DMA_WRITE, &cfg, sizeof cfg) == 0) {
        puts_raw("[bsp] ramfb: ");
        put_dec(fb->width);
        puts_raw("x");
        put_dec(fb->height);
        puts_raw(" XRGB8888 at ");
        put_hex(fb->base);
        puts_raw(" (");
        puts_raw(fb->name);
        puts_raw(")\n");
    } else {
        puts_raw("[bsp] ramfb configuration failed\n");
    }
}

/* アリーナがどの 2 MiB ページに乗っているかを表示 / print which 2 MiB pages the arenas occupy */
static void report_arenas(void) {
    const uint64_t s = (uint64_t)(uintptr_t)__arena_start, e = (uint64_t)(uintptr_t)__arena_end;
    if (fascon2_arena_region_count == 0 || e <= s) return;
    puts_raw("[bsp] arenas: ");
    put_dec(fascon2_arena_region_count);
    puts_raw(fascon2_arena_region_count == 1 ? " table, " : " tables, ");
    put_hex(s);
    puts_raw("..");
    put_hex(e);
    puts_raw(" (");
    put_dec((e - s) >> 10);
    puts_raw(" KiB) -> ");
    put_dec((e - 1) / 0x200000u - s / 0x200000u + 1u);
    puts_raw(" page(s) of 2 MiB\n");
}

/* ───────────── コアごとの準備 / per-core setup ─────────────
 * GDT・TSS・IDT → カーネルの表（CR3 = PCID 0）→ PGE・PCIDE・SMEP・SMAP・NXE → LAPIC → TLS（マルチコア）→ IST のスタック
 * GDT, TSS, IDT → the kernel tables (CR3 = PCID 0) → PGE, PCIDE, SMEP, SMAP, NXE → the LAPIC → TLS (multi-core) → IST stacks */
#if defined(FASCON2_SMP)
extern char __percpu_start[], __percpu_end[], __tls_memsz[], __tls_align[], __smp_frame_start[], __smp_stack_top[];

/* x86_64 の TLS（variant II）: このコアの TLS ブロックの終わりがスレッドポインタ（FS の土台）。%fs:0 には自分自身の番地を
 * 置く（変数の番地を取るコードが読む） / x86_64 TLS (variant II): the end of the TLS block of this core is the thread
 * pointer (the FS base); %fs:0 holds its own address (read by code that takes the address of a variable) */
static void bsp_tls_setup(uint32_t core) {
    const uint64_t memsz = (uint64_t)(uintptr_t)__tls_memsz;
    const uint64_t align = (uint64_t)(uintptr_t)__tls_align > 16u ? (uint64_t)(uintptr_t)__tls_align : 16u;
    const uint64_t stride = (uint64_t)(__percpu_end - __percpu_start) / 4u;
    uint8_t *block = (uint8_t *)__percpu_start + core * stride;
    const uint64_t tls = (memsz + align - 1u) & ~(align - 1u);
    for (uint64_t i = 0; i < tls + 64u; i++) block[i] = 0;
    const uint64_t tp = (uint64_t)(uintptr_t)block + tls;
    __builtin_memcpy((void *)(uintptr_t)tp, &tp, sizeof tp);
    bsp_wrmsr(0xC0000100u, tp); /* IA32_FS_BASE */
    bsp_tp[core] = tp;
    bsp_core_tls = core;
}
#endif

static void bsp_cpu_setup(uint32_t core) {
    bsp_load_tables(core);
    bsp_set_cr3((uint64_t)(uintptr_t)bsp_kpml4);
    uint64_t cr4 = bsp_cr4();
    if (bsp_has_pge) cr4 |= 1ull << 7;
    if (bsp_pcid_on) cr4 |= 1ull << 17;
    if (bsp_has_smep) cr4 |= 1ull << 20;
    if (bsp_smap_on) cr4 |= 1ull << 21;
    bsp_set_cr4(cr4);
    if (bsp_has_nx) bsp_wrmsr(0xC0000080u, bsp_rdmsr(0xC0000080u) | (1ull << 11)); /* EFER.NXE */
    bsp_cur_asid[core] = 0u;
    lapic_on(core);
#if defined(FASCON2_SMP)
    bsp_tls_setup(core);
#endif
    for (uint32_t k = 0; k < 2u; k++) {
        uint8_t *top = bsp_ist[core][k] + sizeof bsp_ist[core][k];
        __builtin_memcpy(top - 16, &bsp_tp[core], sizeof bsp_tp[core]); /* Ring 3 から来た NMI でも FS を戻せる / restore FS for NMIs from ring 3 too */
        bsp_tss[core].ist[k] = (uint64_t)(uintptr_t)(top - 16);
    }
    bsp_tss[core].rsp0 = (uint64_t)(uintptr_t)(bsp_ist[core][1] + sizeof bsp_ist[core][1] - 16); /* Ring 3 のタスクが走るまでの仮 / until a ring 3 task runs */
}

/* ───────────── マルチコア（SMP） / multi-core ─────────────
 * - コア 0 がトランポリンを 0x8000 へ写し、コアごとに INIT → 10 ms → SIPI（ベクタ 0x08）→ 200 µs → SIPI。コアの数は QEMU の
 *   fw_cfg（NB_CPUS）から読み、APIC ID は 0〜n-1（QEMU pc の並び）
 *   core 0 copies the trampoline to 0x8000 and sends every core INIT → 10 ms → SIPI (vector 0x08) → 200 us → SIPI. The
 *   number of cores comes from the fw_cfg of QEMU (NB_CPUS); APIC IDs are 0 to n-1 (the QEMU pc layout)
 * - どのコアも FS の土台を自分の TLS ブロックへ向ける。ふつうのアリーナ・フレームリージョン・own ヒープはコアごとの実体になる
 *   every core points its FS base at its own TLS block; plain arenas, the frame region and the own heap become per-core
 * - 起動したコアは仕事（spawn_on）を待つ。関数が戻ると、次の仕事を待つ
 *   a started core waits for work (spawn_on); when the function returns it waits for the next */
#if defined(FASCON2_SMP)
extern char ap_tramp_start[], ap_tramp_end[], ap_tramp_stack[], ap_tramp_entry[], ap_tramp_core[];
static struct {
    void (*fn)(uint64_t);
    uint64_t arg;
    uint32_t busy;
    uint32_t online;
    uint8_t pad_[128 - 24];
} bsp_cores[FASCON2_MAX_CPUS] __attribute__((aligned(128)));

static void bsp_core_memory(uint32_t core) {
    const size_t heap = (size_t)(__heap_end - __heap_start) / FASCON2_MAX_CPUS;
    fascon_heap_init(__heap_start + core * heap, heap);
    if (core == 0) {
        fascon_frame_init(__frame_start, (size_t)(__frame_end - __frame_start));
    } else {
        fascon_frame_init(__smp_frame_start + (core - 1u) * 0x100000u, 0x100000u);
    }
}

void bsp_x86_ap_main(uint32_t core);
void bsp_x86_ap_main(uint32_t core) {
    bsp_cpu_setup(core);
    fascon_smp_core_init(core);
    bsp_core_memory(core);
    /* TLB の撃ち落としとコア 0 を起こす IPI は受ける。プログラムの割り込みは、そのコアが irq_open / enter するまで来ない
     * （タイマーはまだ張られていない） / take TLB-shootdown and wake IPIs; the interrupts of the program do not arrive until
     * this core calls irq_open / enter (no timer is armed yet) */
    __asm__ __volatile__("sti" ::: "memory");
    __atomic_store_n(&bsp_cores[core].online, 1u, __ATOMIC_RELEASE);
    for (;;) {
        void (*fn)(uint64_t) = __atomic_load_n(&bsp_cores[core].fn, __ATOMIC_ACQUIRE);
        if (fn == 0) {
            fascon_smp_relax();
            continue;
        }
        fn(bsp_cores[core].arg);
        __atomic_store_n(&bsp_cores[core].fn, (void (*)(uint64_t))0, __ATOMIC_RELEASE);
        __atomic_store_n(&bsp_cores[core].busy, 0u, __ATOMIC_RELEASE);
        bsp_wake_core0();
    }
}

static uint32_t bsp_fwcfg_cpus(void) {
    if (!fwcfg_signature()) return FASCON2_MAX_CPUS;
    fascon_outw(FWCFG_SEL, FW_CFG_NB_CPUS);
    const uint32_t n = (uint32_t)fascon_inb(FWCFG_DATA) | ((uint32_t)fascon_inb(FWCFG_DATA) << 8);
    return n >= 1u && n <= FASCON2_MAX_CPUS ? n : FASCON2_MAX_CPUS;
}

static void bsp_smp_start(void) {
    const uint32_t want = bsp_fwcfg_cpus();
    const uint64_t len = (uint64_t)(ap_tramp_end - ap_tramp_start);
    volatile uint8_t *tramp = (volatile uint8_t *)(uintptr_t)0x8000u;
    for (uint64_t i = 0; i < len; i++) tramp[i] = (uint8_t)ap_tramp_start[i];
    volatile uint64_t *m_stack = (volatile uint64_t *)(uintptr_t)(0x8000u + (uint64_t)(ap_tramp_stack - ap_tramp_start));
    volatile uint64_t *m_entry = (volatile uint64_t *)(uintptr_t)(0x8000u + (uint64_t)(ap_tramp_entry - ap_tramp_start));
    volatile uint32_t *m_core = (volatile uint32_t *)(uintptr_t)(0x8000u + (uint64_t)(ap_tramp_core - ap_tramp_start));
    for (uint32_t c = 1; c < want; c++) {
        *m_stack = (uint64_t)(uintptr_t)(__smp_stack_top - (uint64_t)(c - 1u) * 0x20000u);
        *m_entry = (uint64_t)(uintptr_t)bsp_x86_ap_main;
        *m_core = c;
        __atomic_thread_fence(__ATOMIC_SEQ_CST);
        lapic_send(c, 0x4500u); /* INIT */
        bsp_delay_us(10000u);
        lapic_send(c, 0x4608u); /* SIPI: 0x08 << 12 = 0x8000 */
        bsp_delay_us(200u);
        if (!__atomic_load_n(&bsp_cores[c].online, __ATOMIC_ACQUIRE)) lapic_send(c, 0x4608u);
        const uint64_t f = fascon_plat_timer_freq(), t0 = fascon_plat_timer_now();
        while (!__atomic_load_n(&bsp_cores[c].online, __ATOMIC_ACQUIRE) && fascon_plat_timer_now() - t0 < f) FASCON_CPU_RELAX();
        if (!bsp_cores[c].online) break;
        bsp_cpu_count = c + 1u;
    }
    puts_raw("[bsp] SMP: ");
    put_dec(bsp_cpu_count);
    puts_raw(" cores online (INIT-SIPI-SIPI, trampoline at 0x8000), per-core stacks 128 KiB, frames 1024 KiB, own heaps ");
    put_dec((uint64_t)(__heap_end - __heap_start) / FASCON2_MAX_CPUS >> 10);
    puts_raw(" KiB, TLS block ");
    put_dec((uint64_t)(uintptr_t)__tls_memsz);
    puts_raw(" bytes per core (FS base)\n");
}

/* TLB の撃ち落としの自己試験: ほかの全コアへ IPI 0xF0 を送り、全部が答えること / a self-test of the TLB shootdown: IPI 0xF0
 * to every other core, and every one of them must answer */
static void bsp_tlb_selftest(void) {
    if (bsp_cpu_count <= 1u) return;
    const uint64_t before = __atomic_load_n(&bsp_vm_flush_served, __ATOMIC_ACQUIRE);
    fascon_plat_vm_flush(0u, UINT64_MAX, (1ull << bsp_cpu_count) - 1u);
    puts_raw("[bsp] TLB shootdown: IPI 0xf0 answered by ");
    put_dec(__atomic_load_n(&bsp_vm_flush_served, __ATOMIC_ACQUIRE) - before);
    puts_raw(" of ");
    put_dec(bsp_cpu_count - 1u);
    puts_raw(" other core(s) (self-test)\n");
}

uint32_t fascon_plat_cpu_count(void) { return bsp_cpu_count; }
uint32_t fascon_plat_cpu_id(void) { return bsp_core(); }

/* パニックの全コア停止: NMI を「自分以外の全コア」へ（ICR の略記 11）。NMI は割り込みを止めた区間にいるコアにも届く
 * the all-core stop on panic: an NMI to "every core but this one" (ICR shorthand 11); an NMI reaches cores inside masked
 * sections too */
void fascon_plat_panic_broadcast(void) {
    if (bsp_cpu_count <= 1u || bsp_panic_sent) return;
    bsp_panic_sent = 1u;
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
    lapic_send(0u, (3u << 18) | (1u << 14) | (4u << 8));
    for (uint64_t spin = 0; spin < 50000000ull && __atomic_load_n(&bsp_panic_acks, __ATOMIC_ACQUIRE) < bsp_cpu_count - 1u; spin++) FASCON_CPU_RELAX();
}

/* コア 0 の休み: 全コアのタスクが尽き、走っている spawn_on の関数も無ければ終わる / idling on core 0: ends the run once every
 * core is out of tasks and no spawn_on function is still running */
static int bsp_all_idle(void) {
    for (uint32_t c = 0; c < bsp_cpu_count; c++) {
        const f20_sched_t *q = fascon_cpu_sched(c);
        if (q->live != 0u) return 0;
        if (c != 0u && __atomic_load_n(&bsp_cores[c].busy, __ATOMIC_ACQUIRE) && q->switches == 0u) return 0;
    }
    return 1;
}

void fascon_plat_spawn_on(uint32_t core, void (*fn)(uint64_t), uint64_t arg) {
    uint32_t idle = 0u;
    if (core == 0u || core >= bsp_cpu_count) fascon_plat_kernel_panic(FASCON_PANIC_SMP, "spawn_on: no such core (cores 1 to cpu_count() - 1 can be started)");
    if (!__atomic_compare_exchange_n(&bsp_cores[core].busy, &idle, 1u, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) fascon_plat_kernel_panic(FASCON_PANIC_SMP, "spawn_on: the core is still running an earlier function");
    bsp_cores[core].arg = arg;
    __atomic_store_n(&bsp_cores[core].fn, fn, __ATOMIC_RELEASE);
}
static void bsp_idle_loop(void) {
    for (;;) {
        if (bsp_core() == 0u && bsp_all_idle()) fascon_plat_exit(0);
        __asm__ __volatile__("sti\n\thlt" ::: "memory");
    }
}
#else
static void bsp_idle_loop(void) {
    for (;;) __asm__ __volatile__("sti\n\thlt" ::: "memory");
}
uint32_t fascon_plat_cpu_count(void) { return 1u; }
uint32_t fascon_plat_cpu_id(void) { return 0u; }
void fascon_plat_spawn_on(uint32_t core, void (*fn)(uint64_t), uint64_t arg) {
    (void)core;
    (void)fn;
    (void)arg;
    fascon_plat_kernel_panic(FASCON_PANIC_SMP, "spawn_on: this program was built for one core (spawn_on makes the compiler build a multi-core program)");
}
#endif

/* ───────────── 入口 / entry ───────────── */

void bsp_main(void) {
    uart_init();
    bsp_detect();
    bsp_pcid_on = bsp_has_pcid;
    bsp_smap_on = bsp_has_smap;
    bsp_kernel_tables();
    bsp_tables_init();
    bsp_cpu_setup(0u);
#if defined(FASCON2_SMP)
    fascon_smp_core_init(0);
#endif
    pic_off();
    bsp_clock_init();
    puts_raw("[bsp] Fascon 1.0 bare-metal: QEMU x86_64 on the Fascon virtual BIOS (long mode, CPL 0, identity map of 4 GiB with global 2 MiB pages)\n");
    puts_raw("[bsp] x86_64: LAPIC timer = irq 27, clock ");
    if (bsp_hpet_hz) {
        puts_raw("HPET ");
        put_dec(bsp_hpet_hz / 1000000u);
    } else {
        puts_raw("TSC ");
        put_dec(bsp_tsc_hz / 1000000u);
    }
    puts_raw(" MHz, PCID ");
    puts_raw(bsp_pcid_on ? "on" : "off");
    puts_raw(bsp_has_invpcid ? " (INVPCID)" : "");
    puts_raw(", SMEP ");
    puts_raw(bsp_has_smep ? "on" : "off");
    puts_raw(", SMAP ");
    puts_raw(bsp_smap_on ? "on" : "off");
    puts_raw(", NX ");
    puts_raw(bsp_has_nx ? "on" : "off");
    puts_raw("\n");
#if defined(FASCON2_GENERIC_OS)
    /* generic-os のうち、どのアーキテクチャでも同じ部分（64 ビットの Id・コアごとのマガジン・RCU・共有の gc_arena）は
     * ランタイムが受け持つ。守りページ・仮想番地を予約したアリーナ・装置の高位の写し・PAN / PAC / BTI は AArch64 の BSP
     * （genos.c）の機能で、ここには無い（SMEP / SMAP / NX は上の行のとおり）
     * the parts of generic-os that are the same on every architecture (64-bit Ids, per-core magazines, RCU, shared
     * gc_arena) are handled by the runtime. Guard pages, VA-reserved arenas, the high device mirror and PAN / PAC / BTI are
     * features of the AArch64 BSP (genos.c) and are absent here (SMEP / SMAP / NX as on the line above) */
    puts_raw("[bsp] generic-os on x86_64: 64-bit Ids, per-core magazines, RCU and shared gc_arena from the runtime; guard pages, VA-reserved arenas and the device mirror are AArch64 BSP features\n");
#endif
    report_arenas();
    const uint64_t ks = (uint64_t)(uintptr_t)__kernel_start, ke = (uint64_t)(uintptr_t)__kernel_end;
    for (uint32_t i = 0; i < fascon2_mmio_region_count; i++) {
        const FasconMmioRegion *r = &fascon2_mmio_regions[i];
        if (r->base < ke && r->base + r->size > ks) fascon_plat_kernel_panic(0x2203u, "MMIO region overlaps the kernel image");
    }
    setup_framebuffer();
#if defined(FASCON2_SMP)
    bsp_core_memory(0);
    __asm__ __volatile__("sti" ::: "memory"); /* コア 0 も IPI を受ける / core 0 takes IPIs too */
    bsp_smp_start();
    bsp_tlb_selftest();
    __asm__ __volatile__("cli" ::: "memory");
#else
    fascon_heap_init(__heap_start, (size_t)(__heap_end - __heap_start));
    fascon_frame_init(__frame_start, (size_t)(__frame_end - __frame_start));
#endif
    puts_raw("[bsp] own heap ");
    put_dec((uint64_t)(__heap_end - __heap_start) >> 10);
    puts_raw(" KiB, own frame ");
    put_dec((uint64_t)(__frame_end - __frame_start) >> 10);
    puts_raw(" KiB, kernel image ");
    put_hex(ks);
    puts_raw("..");
    put_hex(ke);
    puts_raw("\n");
    fascon2_main();
    fascon_plat_exit(0);
}

/* 終わりの報告（ユーザー空間を使ったときだけ） / the closing report (only when user space was used) */
static void bsp_vm_report(void) {
    if (bsp_umem == 0) return;
    uint64_t faults = 0, sw = 0, ticks = 0;
    for (uint32_t c = 0; c < BSP_NCORE; c++) {
        faults += fascon_cpu_sched(c)->faults;
        sw += fascon_cpu_sched(c)->switches;
        ticks += fascon_cpu_sched(c)->ticks;
    }
    puts_raw("[bsp] vm: ");
    put_dec(faults);
    puts_raw(" demand-paged 4 KiB pages, ");
    put_dec(bsp_vm_maps);
    puts_raw(" mappings, ");
    put_dec(bsp_vm_drops);
    puts_raw(" address spaces dropped (TLB flush by address space: local ");
    put_dec(bsp_vm_flush_local);
    puts_raw(", IPI ");
    put_dec(bsp_vm_flush_ipi);
    puts_raw(" served ");
    put_dec(bsp_vm_flush_served);
    puts_raw("), task switches ");
    put_dec(sw);
    puts_raw(bsp_pcid_on ? " with no TLB flush" : " (no PCID: each CR3 load drops non-global entries)");
    puts_raw(", timer ticks ");
    put_dec(ticks);
    puts_raw(", page tables peak ");
    put_dec(bsp_pt_peak);
    puts_raw("\n");
}
