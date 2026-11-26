/*
 * bsp.c — Fascon 1.0 BSP: QEMU virt (AArch64)。BIOS もファームウェアも使わない。
 *         Fascon 1.0 BSP for QEMU virt (AArch64), with no BIOS and no firmware.
 *
 * 起動順 / boot sequence (CPU0):
 *   1. MMU とキャッシュを有効化（恒等マップ: 0〜1 GiB = デバイス、1〜2 GiB = RAM、フレームバッファは非キャッシュ）。
 *      4 KiB ページは使わない: RAM は画面がなければ 1 GiB ブロック 1 枚、あれば 2 MiB ブロック + 連続ビット。
 *      ページテーブルは起動時に 1 回書くだけで、実行中に書き換えることはない
 *      enable the MMU and caches (identity map: 0-1 GiB device, 1-2 GiB RAM, framebuffers non-cacheable). No 4 KiB
 *      pages: RAM is one 1 GiB block without a framebuffer, otherwise 2 MiB blocks with the contiguous hint. The tables
 *      are written once at boot and never rewritten while running
 *   2. 生成 C の MMIO リージョン表を検査（カーネル像・スタック・ヒープと重ならないこと）
 *      check the generated MMIO region table (no overlap with the kernel image, stack or heap)
 *   3. #[framebuffer] リージョンがあれば、QEMU の ramfb を fw_cfg DMA で設定して画面に出す
 *      if a #[framebuffer] region exists, configure QEMU's ramfb through fw_cfg DMA so it is shown on screen
 *   4. own ヒープとフレームリージョンを登録して fascon2_main() を呼ぶ / register the own heap and the frame region, call fascon2_main()
 *
 * 必要な QEMU の起動オプション / QEMU options:
 *   qemu-system-aarch64 -M virt -cpu cortex-a72 -m 256M -semihosting -kernel app.elf -serial stdio [-device ramfb]
 */
#include "fascon2_rt.h"

#ifndef FASCON_BSP_SEMIHOSTING
#define FASCON_BSP_SEMIHOSTING 1
#endif

#if defined(FASCON2_GENERIC_OS)
/* generic-os の拡張（genos.c。このファイルの後ろで取り込む） / the generic-os extension (genos.c, included further down) */
static void genos_sgi(uint32_t intid);
static size_t genos_task_layout(uint8_t *base, size_t stack_size);
static void genos_report(void);
static void genos_mmu_setup(void);
#endif

#define PTE_VALID 0x1ull
#define PTE_TABLE 0x3ull
#define PTE_AF (1ull << 10)
#define PTE_SH_INNER (3ull << 8)
#define PTE_SH_OUTER (2ull << 8)
#define PTE_ATTR(i) ((uint64_t)(i) << 2)
#define PTE_CONT (1ull << 52) /* 連続ビット / Contiguous */
#define PTE_PXN (1ull << 53)
#define PTE_UXN (1ull << 54)
#define RAM_BASE 0x40000000ull
#define BLOCK_2M 0x200000ull
#define CONT_BLOCKS 16u /* 4 KiB 粒度・レベル 2 の連続範囲 = 16 × 2 MiB = 32 MiB / contiguous range at level 2 */

#define UART0 0x09000000ul
#define FWCFG 0x09020000ul

void fascon2_main(void);
extern char __kernel_start[], __kernel_end[], __heap_start[], __heap_end[], __frame_start[], __frame_end[], __arena_start[], __arena_end[];

/* ───────────── PL011 UART ───────────── */

void fascon_plat_print(const char *s, size_t len) {
    for (size_t i = 0; i < len; i++) {
        if (s[i] == '\n') {
            while (fascon_mmio_read32(UART0 + 0x18) & (1u << 5)) /* TXFF */
                FASCON_CPU_RELAX();
            fascon_mmio_write32(UART0, '\r');
        }
        while (fascon_mmio_read32(UART0 + 0x18) & (1u << 5))
            FASCON_CPU_RELAX();
        fascon_mmio_write32(UART0, (uint32_t)(unsigned char)s[i]);
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

/* ───────────── 終了・停止 / exit and halt ───────────── */

#if defined(FASCON2_SMP)
static uint32_t bsp_panic_sent;
static volatile uint32_t bsp_panic_acks;
#endif

static FASCON_NORETURN void power_off(int code) {
#if FASCON_BSP_SEMIHOSTING
    /* セミホスティング SYS_EXIT_EXTENDED: QEMU が終了コード付きで終わる / QEMU exits with this code */
    static uint64_t block[2];
    block[0] = 0x20026; /* ADP_Stopped_ApplicationExit */
    block[1] = (uint64_t)(int64_t)code;
    register uint64_t x0 __asm__("x0") = 0x20;
    register uint64_t x1 __asm__("x1") = (uint64_t)block;
    __asm__ __volatile__("hlt #0xf000" : : "r"(x0), "r"(x1) : "memory");
#else
    (void)code;
    /* PSCI SYSTEM_OFF */
    register uint64_t x0 __asm__("x0") = 0x84000008ull;
    __asm__ __volatile__("hvc #0" : "+r"(x0) : : "memory");
#endif
    for (;;) __asm__ __volatile__("wfi");
}

static void bsp_vm_report(void);
FASCON_NORETURN void fascon_plat_exit(int code) {
#if defined(FASCON2_GENERIC_OS)
    genos_report();
#endif
    bsp_vm_report();
    puts_raw("[bsp] exit code ");
    put_dec((uint64_t)(code < 0 ? -code : code));
    puts_raw("\n");
    power_off(code);
}

FASCON_NORETURN void fascon_plat_halt(void) {
    /* 画面を表示したまま止まる（割り込みは使わない） / stop while keeping the screen (no interrupts) */
    puts_raw("[bsp] halted (screen kept)\n");
    __asm__ __volatile__("msr daifset, #0xf" ::: "memory");
    for (;;) __asm__ __volatile__("wfi");
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
        puts_raw("[bsp] panic broadcast (SGI 2): ");
        put_dec(__atomic_load_n(&bsp_panic_acks, __ATOMIC_ACQUIRE));
        puts_raw(" other core(s) stopped\n");
    }
#endif
    power_off(70);
}

void bsp_sync_exception(uint64_t esr, uint64_t elr, uint64_t far) {
    puts_raw("\nSYNC EXCEPTION esr=");
    put_hex(esr);
    puts_raw(" elr=");
    put_hex(elr);
    puts_raw(" far=");
    put_hex(far);
    puts_raw("\n");
    power_off(71);
}

/* ───────────── カーネル拡張: GICv2・Generic Timer・タスクの切り替え（コアごと）・タスクごとのアドレス空間
 *                kernel extensions: GICv2, the generic timer, task switching (per core) and per-task address spaces ─────────────
 *
 * - GIC は Fascon のソースからは見えない。irq 項目の表（fascon_plat_irq_register）を読んで線を許可し、割り込みごとに
 *   ACK（IAR）→ ハンドラ → EOI（EOIR）を行う。SPI（INTID 32 以上）は CPU0 へ、PPI（16〜31: タイマーなど）はコアごとに
 *   そのコアが割り込みを開いたとき（irq_open / enter / wait_irq）に許可する。SGI は BSP の仕事: 1 = TLB の撃ち落とし
 *   （generic-os）、2 = パニックの全コア停止、3 = アドレス空間の TLB の撃ち落とし（vm_flush）
 *   the GIC is invisible to Fascon source: the table of irq items enables the lines, and every interrupt goes
 *   acknowledge (IAR) → handler → end of interrupt (EOIR). SPIs (32 and up) go to CPU0; PPIs (16-31: the timer, ...)
 *   are enabled per core when that core opens interrupts (irq_open / enter / wait_irq). SGIs belong to the BSP:
 *   1 = TLB shootdown (generic-os), 2 = the all-core stop on panic, 3 = address-space TLB shootdown (vm_flush)
 * - タイマーは仮想タイマー（CNTV、INTID 27。コアごとに 1 つ）。INTID 30 のハンドラだけがあれば物理タイマー（CNTP）。
 *   レベル割り込みなので、ハンドラの前にタイマー側でマスクし、timer_arm() が解除する
 *   the timer is the virtual timer (CNTV, INTID 27, one per core), or the physical one (CNTP) when only INTID 30 has a
 *   handler. It is level-triggered, so it is masked at the timer before the handler and unmasked by timer_arm()
 * - タスクの表・いまのタスク・実行キューはコアごと（fascon_cpu_sched）。タスクの文脈は、そのタスクのカーネルスタックに
 *   積んだ保存フレームの番地 1 つで、切り替えは start.S の出口へ別のフレームの番地を返すだけ
 *   the task table, the current task and the run queue are per core (fascon_cpu_sched). The context of a task is one
 *   address: its saved frame on its own kernel stack; switching returns the address of another frame to the exit path of start.S
 * - EL0 のタスクはそれぞれ自分のアドレス空間（ASID 付きの TTBR0）を持つ。ユーザーメモリは 4 KiB のページで、初めて触れた
 *   ときに写す（要求時ページング）。切り替えは TTBR0 を書くだけで TLB は掃かない。掃くのはアドレス空間を捨てるとき
 *   （その ASID だけ、使ったコアへだけ）
 *   every EL0 task has its own address space (TTBR0 with an ASID). User memory is mapped in 4 KiB pages on first touch
 *   (demand paging). A switch only writes TTBR0 and never flushes the TLB; flushes happen when an address space is
 *   dropped (that ASID only, on the cores that used it) */
#define GICD 0x08000000ul
#define GICC 0x08010000ul
#define MMIO32(a) (*(volatile uint32_t *)(uintptr_t)(a))
#define MMIO8(a) (*(volatile uint8_t *)(uintptr_t)(a))
#define BSP_SGI_PANIC 2u
#define BSP_SGI_VM 3u

#if defined(FASCON2_SMP)
#define BSP_NCORE FASCON2_MAX_CPUS
static inline uint32_t bsp_core(void) {
    uint64_t m;
    __asm__ __volatile__("mrs %0, mpidr_el1" : "=r"(m));
    return (uint32_t)(m & 0xffu) % FASCON2_MAX_CPUS;
}
#else
#define BSP_NCORE 1u
static inline uint32_t bsp_core(void) { return 0u; }
#endif

typedef struct {
    f20_trapframe_t *sp;
    f20_id_t id;
    int alive;
    void (*entry)(uint64_t);
    uint64_t arg;
    void (*on_exit)(void);
    uint8_t *stack;
    uint8_t *f_top, *f_end, *f_base;
    int user;      /* EL0 のタスク（自分のアドレス空間を持つ） / an EL0 task (it has its own address space) */
    uint32_t asid; /* EL0 のタスクの ASID（カーネルのタスクは 0 = カーネルの表） / the ASID of an EL0 task (0 = the kernel table) */
#if defined(FASCON2_GC)
    void *gc_top; /* fascon-gc: このタスクのシャドウスタックの先頭 / this task's shadow-stack top */
#endif
} BspTask;

static BspTask bsp_tasks[BSP_NCORE][FASCON2_TASK_MAX];
static const FasconIrqEntry *bsp_irqs = 0;
static uint32_t bsp_irq_count = 0;
static uint32_t bsp_timer_intid = 27u;
static volatile uint32_t bsp_irq_ready = 0; /* 表が登録された / the table has been registered */

static inline f20_sched_t *bsp_sched(void) { return fascon_cpu_sched(bsp_core()); }
static inline BspTask *bsp_cur_task(void) {
    const f20_sched_t *S = bsp_sched();
    return S->cur >= 0 ? &bsp_tasks[bsp_core()][S->cur] : (BspTask *)0;
}

/* このコアの GIC の窓口（GICC）と、BSP の SGI を開く（コアごとに 1 回） / open the GIC interface (GICC) of this core and the
 * SGIs of the BSP (once per core) */
static void bsp_gic_core_on(void) {
    f20_sched_t *S = bsp_sched();
    if (S->gic_on & 1u) return;
    S->gic_on |= 1u;
    MMIO8(GICD + 0x400 + BSP_SGI_PANIC) = 0x00u; /* 最高の優先度 / the highest priority */
    MMIO8(GICD + 0x400 + BSP_SGI_VM) = 0x80u;
    MMIO32(GICD + 0x100) = (1u << BSP_SGI_PANIC) | (1u << BSP_SGI_VM); /* SGI はコアごとの写し / SGIs are banked */
    MMIO32(GICC + 0x004) = 0xFFu;
    MMIO32(GICC + 0x000) = 1u;
    MMIO32(GICD + 0x000) = 1u;
}

/* このコアで、irq の表の PPI（16〜31。タイマーなど、コアごとの写し）を許可する / enable the PPIs of the table (16-31, banked per core:
 * the timer and others) on this core */
static void bsp_gic_ppis_on(void) {
    f20_sched_t *S = bsp_sched();
    if ((S->gic_on & 2u) || !__atomic_load_n(&bsp_irq_ready, __ATOMIC_ACQUIRE)) return;
    bsp_gic_core_on();
    S->gic_on |= 2u;
    for (uint32_t i = 0; i < bsp_irq_count; i++) {
        const uint32_t n = bsp_irqs[i].intid;
        if (n < 16u || n >= 32u) continue;
        MMIO8(GICD + 0x400 + n) = 0xA0u;
        MMIO32(GICD + 0x100) = 1u << n;
    }
}

void fascon_plat_irq_register(const FasconIrqEntry *table, uint32_t count) {
    bsp_irqs = table;
    bsp_irq_count = count;
    int has27 = 0, has30 = 0;
    MMIO32(GICD + 0x000) = 0u;
    for (uint32_t i = 0; i < count; i++) {
        const uint32_t n = table[i].intid;
        if (n == 27u) has27 = 1;
        if (n == 30u) has30 = 1;
        MMIO8(GICD + 0x400 + n) = 0xA0u;                      /* 優先度 / priority */
        if (n >= 32u) MMIO8(GICD + 0x800 + n) = 0x01u;        /* SPI は CPU0 へ / SPIs go to CPU0 */
        MMIO32(GICD + 0x100 + 4u * (n / 32u)) = 1u << (n % 32u); /* 許可（PPI はこのコアの写し） / enable (PPIs: the bank of this core) */
    }
    if (has30 && !has27) bsp_timer_intid = 30u;
    MMIO32(GICC + 0x004) = 0xFFu; /* 優先度マスク: すべて通す / priority mask: let everything through */
    MMIO32(GICC + 0x000) = 1u;
    MMIO32(GICD + 0x000) = 1u;
    fascon_cpu_sched(bsp_core())->gic_on |= 3u;
    __atomic_store_n(&bsp_irq_ready, 1u, __ATOMIC_RELEASE);
}

uint64_t fascon_plat_irq_save(void) {
    uint64_t d;
    __asm__ __volatile__("mrs %0, daif\n msr daifset, #2" : "=r"(d));
    return d;
}

void fascon_plat_irq_restore(uint64_t state) { __asm__ __volatile__("msr daif, %0" : : "r"(state)); }

void fascon_plat_irq_open(void) {
    bsp_gic_ppis_on();
    __asm__ __volatile__("msr daifclr, #2");
}

/* 割り込みを禁止したまま wfi（禁止中でも保留の割り込みで起きる）→ 許可して受け取る → 元の状態へ。確認と待ちの間の
 * 割り込みを取りこぼさない
 * wfi with interrupts masked (a pending interrupt still wakes it) → unmask to take it → back to the previous state; an
 * interrupt between the check and the wait is never lost */
void fascon_plat_wait_irq(void) {
    bsp_gic_ppis_on();
    const uint64_t d = fascon_plat_irq_save();
    __asm__ __volatile__("wfi\n msr daifclr, #2\n isb");
    fascon_plat_irq_restore(d);
}

uint64_t fascon_plat_timer_freq(void) {
    uint64_t v;
    __asm__ __volatile__("mrs %0, cntfrq_el0" : "=r"(v));
    return v;
}

uint64_t fascon_plat_timer_now(void) {
    uint64_t v;
    if (bsp_timer_intid == 30u) {
        __asm__ __volatile__("isb\n mrs %0, cntpct_el0" : "=r"(v));
    } else {
        __asm__ __volatile__("isb\n mrs %0, cntvct_el0" : "=r"(v));
    }
    return v;
}

void fascon_plat_timer_arm(uint64_t ticks) {
    const uint64_t on = 1u;
    if (bsp_timer_intid == 30u) {
        __asm__ __volatile__("msr cntp_tval_el0, %0\n msr cntp_ctl_el0, %1\n isb" : : "r"(ticks), "r"(on));
    } else {
        __asm__ __volatile__("msr cntv_tval_el0, %0\n msr cntv_ctl_el0, %1\n isb" : : "r"(ticks), "r"(on));
    }
}

static void bsp_timer_mask(void) {
    const uint64_t masked = 3u; /* ENABLE | IMASK */
    if (bsp_timer_intid == 30u) {
        __asm__ __volatile__("msr cntp_ctl_el0, %0\n isb" : : "r"(masked));
    } else {
        __asm__ __volatile__("msr cntv_ctl_el0, %0\n isb" : : "r"(masked));
    }
}

static void bsp_check_canary(int t) {
    if (t >= 0 && bsp_tasks[bsp_core()][t].stack != 0) {
        uint64_t c;
        __builtin_memcpy(&c, bsp_tasks[bsp_core()][t].stack, sizeof c);
        if (c != FASCON_STACK_CANARY) fascon_plat_kernel_panic(FASCON_PANIC_STACK, "task stack overflow (the canary at the bottom of the stack was overwritten)");
    }
}

/* ───── 4 KiB のページとタスクごとのアドレス空間（fascon_plat_vm_*） / 4 KiB pages and per-task address spaces ─────
 * 表はカーネルの L1（恒等写像・装置・generic-os の写し）を写したタスクごとの L1 と、その下の L2・L3（ページ表の置き場から
 * 取る）。カーネルの項目は大域（nG = 0）なので、ASID を替えても TLB に残る。ユーザーのページは nG = 1・PXN（カーネルは
 * 実行しない）・EL0 から読み書き。ASID は 16 ビット（ID_AA64MMFR0_EL1.ASIDBits = 2 のとき TCR_EL1.AS = 1）
 * per-task L1 tables copy the L1 of the kernel (identity map, devices, generic-os mirrors); L2 and L3 below come from the page
 * table pool. Kernel entries are global (nG = 0), so they stay in the TLB across ASIDs. User pages are nG = 1, PXN (the
 * kernel never executes them) and read/write from EL0. ASIDs are 16-bit (TCR_EL1.AS = 1 when ID_AA64MMFR0_EL1.ASIDBits = 2) */
#ifndef BSP_PT_PAGES
#define BSP_PT_PAGES 384u
#endif
#define BSP_SPACES (BSP_NCORE * FASCON2_TASK_MAX + 1u)
#define PTE_NG (1ull << 11)
#define PTE_AP_EL0RW (1ull << 6)
#define PTE_L3_PAGE 0x3ull
#define PTE_ADDR_MASK 0x0000FFFFFFFFF000ull
static uint64_t bsp_pt_pool[BSP_PT_PAGES][512] __attribute__((aligned(4096)));
static uint32_t bsp_pt_used[(BSP_PT_PAGES + 31u) / 32u];
static volatile uint32_t bsp_pt_lock;
static uint32_t bsp_pt_peak, bsp_pt_now;
typedef struct {
    uint64_t *l1;
    uint64_t cpus; /* この ASID を TTBR0 に載せたコア / cores that have loaded this ASID into TTBR0 */
    uint32_t pages;
} BspSpace;
static BspSpace bsp_spaces[BSP_SPACES];
static uint32_t bsp_asid16;
static uint64_t bsp_vm_maps, bsp_vm_flush_local, bsp_vm_flush_ipi, bsp_vm_flush_served, bsp_vm_drops;
static uint64_t l1_table[512] __attribute__((aligned(4096)));

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
    const uint64_t a = e & PTE_ADDR_MASK;
    return (e & 3u) == 3u && a >= (uint64_t)(uintptr_t)bsp_pt_pool && a < (uint64_t)(uintptr_t)bsp_pt_pool + sizeof bsp_pt_pool;
}

bool fascon_plat_vm_space(uint32_t asid) {
    if (asid == 0u || asid >= BSP_SPACES) return false;
    if (bsp_spaces[asid].l1) fascon_plat_vm_drop(asid);
    uint64_t *l1 = bsp_pt_alloc();
    for (uint32_t k = 0; k < 512u; k++) l1[k] = l1_table[k]; /* カーネルの項目（大域） / the kernel entries (global) */
    __asm__ __volatile__("dsb ishst" ::: "memory");
    bsp_spaces[asid].l1 = l1;
    bsp_spaces[asid].cpus = 0u;
    bsp_spaces[asid].pages = 0u;
    return true;
}

/* カーネルの L1 の項目が増えたら（generic-os のアリーナの予約域など）、すべてのアドレス空間へ写す / when a kernel L1 entry
 * appears (generic-os arena ranges and the like), copy it into every address space */
static FASCON_MAYBE_UNUSED void bsp_vm_sync_l1(uint32_t idx) {
    for (uint32_t a = 1; a < BSP_SPACES; a++)
        if (bsp_spaces[a].l1 && !bsp_pt_owned(bsp_spaces[a].l1[idx])) bsp_spaces[a].l1[idx] = l1_table[idx];
    __asm__ __volatile__("dsb ishst" ::: "memory");
}

bool fascon_plat_vm_map(uint32_t asid, uint64_t va, uint64_t pa, uint32_t flags) {
    if (asid == 0u || asid >= BSP_SPACES || !bsp_spaces[asid].l1 || ((va | pa) & 0xFFFu) != 0u || (va >> 39) != 0u) return false;
    uint64_t *l1e = &bsp_spaces[asid].l1[(va >> 30) & 511u];
    if (*l1e != 0u && !bsp_pt_owned(*l1e)) return false; /* カーネルの範囲には写さない / never map over a kernel range */
    if (*l1e == 0u) *l1e = (uint64_t)(uintptr_t)bsp_pt_alloc() | PTE_TABLE;
    uint64_t *l2 = (uint64_t *)(uintptr_t)(*l1e & PTE_ADDR_MASK);
    uint64_t *l2e = &l2[(va >> 21) & 511u];
    if (*l2e == 0u) *l2e = (uint64_t)(uintptr_t)bsp_pt_alloc() | PTE_TABLE;
    uint64_t *l3 = (uint64_t *)(uintptr_t)(*l2e & PTE_ADDR_MASK);
    uint64_t e = pa | PTE_L3_PAGE | PTE_ATTR(1) | PTE_AF | PTE_SH_INNER | PTE_NG | PTE_PXN;
    if (flags & F20_VM_USER) e |= PTE_AP_EL0RW;
    if (!(flags & F20_VM_WRITE)) e |= (2ull << 6); /* AP[2] = 読み取りのみ / read-only */
    if (!(flags & F20_VM_EXEC)) e |= PTE_UXN;
    const int fresh = (l3[(va >> 12) & 511u] & PTE_VALID) == 0u;
    l3[(va >> 12) & 511u] = e;
    __asm__ __volatile__("dsb ishst\n isb" ::: "memory"); /* 無効 → 有効は TLB を掃かなくてよい / invalid → valid needs no TLBI */
    if (fresh) bsp_spaces[asid].pages++;
    bsp_vm_maps++;
    return true;
}

/* ───── TLB の撃ち落とし（SGI 3）: 自分のコアは局所の TLBI（Non-shareable）、ほかのコアは郵便受けと SGI ─────
 *       TLB shootdown (SGI 3): a local TLBI (non-shareable) on this core, a mailbox and an SGI for other cores */
static struct {
    volatile uint64_t arg; /* (ASID << 48) | (va >> 12)、va 全体なら bit 63 / bit 63 = the whole ASID */
    volatile uint32_t req, ack;
    uint8_t pad_[128 - 16];
} bsp_vm_mbox[BSP_NCORE] __attribute__((aligned(128)));
static inline void bsp_tlbi_local(uint64_t arg) {
    if (arg >> 63) {
        __asm__ __volatile__("dsb nshst\n tlbi aside1, %0\n dsb nsh\n isb" : : "r"(arg & 0xFFFF000000000000ull) : "memory");
    } else {
        __asm__ __volatile__("dsb nshst\n tlbi vae1, %0\n dsb nsh\n isb" : : "r"(arg) : "memory");
    }
}
static void bsp_vm_serve(uint32_t me) {
    const uint32_t r = __atomic_load_n(&bsp_vm_mbox[me].req, __ATOMIC_ACQUIRE);
    if (r != bsp_vm_mbox[me].ack) {
        bsp_tlbi_local(bsp_vm_mbox[me].arg);
        __atomic_fetch_add(&bsp_vm_flush_served, 1u, __ATOMIC_RELAXED);
        __atomic_store_n(&bsp_vm_mbox[me].ack, r, __ATOMIC_RELEASE);
    }
}
#if defined(FASCON2_SMP)
static uint32_t bsp_cpu_count;
#endif
/* 送り手は 1 度に 1 つ（全体のロック。割り込みを止めて取る）。郵便受けの引数は 1 つなので、2 つのコアが同じコアへ同時に
 * 頼むと片方の引数が上書きされて掃き残しが出る。ロックや返事を待つ間も自分の郵便受けを読むので、頼み合っても止まらない
 * one sender at a time (a global lock taken with interrupts masked). A mailbox holds one operand, so two cores asking the
 * same core at once would overwrite one of them and leave a stale entry. While waiting for the lock or for answers a core
 * keeps serving its own mailbox, so cores asking each other never deadlock */
static FASCON_MAYBE_UNUSED volatile uint32_t bsp_vm_lock;
void fascon_plat_vm_flush(uint32_t asid, uint64_t va, uint64_t cpu_mask) {
    const uint64_t arg = va == UINT64_MAX ? ((uint64_t)asid << 48) | (1ull << 63) : ((uint64_t)asid << 48) | ((va >> 12) & 0xFFFFFFFFFFull);
    const uint64_t im = fascon_plat_irq_save();
    const uint32_t me = bsp_core();
    if (cpu_mask & (1ull << me)) {
        bsp_tlbi_local(arg);
        bsp_vm_flush_local++;
    }
#if defined(FASCON2_SMP)
    const uint64_t others = cpu_mask & ~(1ull << me) & ((1ull << bsp_cpu_count) - 1u);
    if (others == 0u) {
        fascon_plat_irq_restore(im);
        return;
    }
    while (__atomic_exchange_n(&bsp_vm_lock, 1u, __ATOMIC_ACQUIRE) != 0u) {
        bsp_vm_serve(me);
        FASCON_CPU_RELAX();
    }
    uint32_t want[BSP_NCORE] = {0};
    for (uint32_t c = 0; c < BSP_NCORE; c++) {
        if (!(others & (1ull << c))) continue;
        bsp_vm_mbox[c].arg = arg;
        want[c] = __atomic_add_fetch(&bsp_vm_mbox[c].req, 1u, __ATOMIC_ACQ_REL);
        __atomic_fetch_add(&bsp_vm_flush_ipi, 1u, __ATOMIC_RELAXED);
    }
    __asm__ __volatile__("dsb ish" ::: "memory");
    MMIO32(GICD + 0xF00) = (uint32_t)((others & 0xFFu) << 16) | BSP_SGI_VM; /* GICD_SGIR: ターゲットの一覧へだけ / to the targets only */
    for (uint32_t c = 0; c < BSP_NCORE; c++) {
        if (!(others & (1ull << c))) continue;
        uint64_t spins = 0;
        while ((int32_t)(__atomic_load_n(&bsp_vm_mbox[c].ack, __ATOMIC_ACQUIRE) - want[c]) < 0) {
            bsp_vm_serve(me); /* 互いに頼み合っても止まらない / no deadlock when two cores ask each other */
            FASCON_CPU_RELAX();
            if (++spins == 400000000ull) fascon_plat_kernel_panic(FASCON_PANIC_SMP, "vm: TLB shootdown: a core did not answer SGI 3");
        }
    }
    __atomic_store_n(&bsp_vm_lock, 0u, __ATOMIC_RELEASE);
#else
    (void)cpu_mask;
#endif
    fascon_plat_irq_restore(im);
}

static inline uint64_t bsp_ttbr0_now(void) {
    uint64_t v;
    __asm__ __volatile__("mrs %0, ttbr0_el1" : "=r"(v));
    return v;
}

void fascon_plat_vm_switch(uint32_t asid) {
    uint64_t t = (uint64_t)(uintptr_t)l1_table;
    if (asid != 0u && asid < BSP_SPACES && bsp_spaces[asid].l1) {
        t = (uint64_t)(uintptr_t)bsp_spaces[asid].l1 | ((uint64_t)asid << 48);
        __atomic_fetch_or(&bsp_spaces[asid].cpus, 1ull << bsp_core(), __ATOMIC_RELAXED);
    }
    if (bsp_ttbr0_now() != t) __asm__ __volatile__("msr ttbr0_el1, %0\n isb" : : "r"(t) : "memory"); /* TLB は掃かない / no TLB flush */
}

uint64_t fascon_plat_vm_unmap(uint32_t asid, uint64_t va) {
    if (asid == 0u || asid >= BSP_SPACES || !bsp_spaces[asid].l1) return 0u;
    const uint64_t l1e = bsp_spaces[asid].l1[(va >> 30) & 511u];
    if (!bsp_pt_owned(l1e)) return 0u;
    const uint64_t l2e = ((uint64_t *)(uintptr_t)(l1e & PTE_ADDR_MASK))[(va >> 21) & 511u];
    if ((l2e & 3u) != 3u) return 0u;
    uint64_t *e = &((uint64_t *)(uintptr_t)(l2e & PTE_ADDR_MASK))[(va >> 12) & 511u];
    if (!(*e & PTE_VALID)) return 0u;
    const uint64_t pa = *e & PTE_ADDR_MASK;
    *e = 0u; /* 有効 → 無効（BBM 不要） / valid → invalid (no break-before-make needed) */
    bsp_spaces[asid].pages--;
    fascon_plat_vm_flush(asid, va & ~0xFFFull, bsp_spaces[asid].cpus); /* 使ったコアへだけ / only the cores that used it */
    return pa;
}

void fascon_plat_vm_drop(uint32_t asid) {
    if (asid == 0u || asid >= BSP_SPACES || !bsp_spaces[asid].l1) return;
    if ((bsp_ttbr0_now() & PTE_ADDR_MASK) == (uint64_t)(uintptr_t)bsp_spaces[asid].l1) fascon_plat_vm_switch(0u); /* 先にカーネルの表へ / to the kernel table first */
    uint64_t *l1 = bsp_spaces[asid].l1;
    bsp_spaces[asid].l1 = 0;
    for (uint32_t i = 0; i < 512u; i++) {
        if (!bsp_pt_owned(l1[i])) continue;
        uint64_t *l2 = (uint64_t *)(uintptr_t)(l1[i] & PTE_ADDR_MASK);
        for (uint32_t j = 0; j < 512u; j++)
            if ((l2[j] & 3u) == 3u) bsp_pt_free((uint64_t *)(uintptr_t)(l2[j] & PTE_ADDR_MASK));
        bsp_pt_free(l2);
    }
    bsp_pt_free(l1);
    /* その ASID の TLB の項目を、使ったコアだけで消す（同じ ASID を次のタスクが使う前に）/ remove the TLB entries of that ASID on the
     * cores that used it, before the next task reuses the ASID */
    fascon_plat_vm_flush(asid, UINT64_MAX, bsp_spaces[asid].cpus);
    bsp_spaces[asid].cpus = 0u;
    bsp_spaces[asid].pages = 0u;
    bsp_vm_drops++;
}

/* ───── ユーザー空間（EL0）─────
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

static inline uint32_t bsp_asid_of(uint32_t core, uint32_t slot) { return bsp_asid16 ? 1u + core * FASCON2_TASK_MAX + slot : 1u + ((core * 64u + slot) % 255u); }
static inline uint8_t *bsp_umem_of(uint32_t core, uint32_t slot) { return bsp_umem + ((uint64_t)core * bsp_uslots + slot) * bsp_usize; }

void fascon_plat_user_register(uint8_t *mem, uint64_t size, uint64_t va, uint32_t slots) {
    if (((uint64_t)(uintptr_t)mem & 0xFFFull) != 0 || size == 0 || (size & 0xFFFull) != 0 || (va & 0xFFFull) != 0 || va < 0x80000000ull || (va >> 30) >= 512u || (va >> 30) != ((va + size - 1u) >> 30))
        fascon_plat_kernel_panic(FASCON_PANIC_TASK, "user_mem: bad placement");
    if (l1_table[va >> 30] != 0u) fascon_plat_kernel_panic(FASCON_PANIC_TASK, "user_mem: the virtual range collides with a kernel mapping");
    bsp_umem = mem;
    bsp_usize = size;
    bsp_uva = va;
    bsp_uslots = slots;
    /* EL0 から見えるもの: カウンタの読み出しだけ（CNTKCTL_EL1）。DC ZVA・CTR_EL0・キャッシュ操作は EL0 に許さない
     * what EL0 may touch: reading the counters only (CNTKCTL_EL1); no DC ZVA, CTR_EL0 or cache maintenance at EL0 */
    uint64_t sctlr;
    __asm__ __volatile__("mrs %0, sctlr_el1" : "=r"(sctlr));
    sctlr &= ~((1ull << 26) | (1ull << 15) | (1ull << 14)); /* UCI, UCT, DZE */
    __asm__ __volatile__("msr sctlr_el1, %0; isb" : : "r"(sctlr) : "memory");
    __asm__ __volatile__("msr cntkctl_el1, %0; isb" : : "r"((uint64_t)3u));
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
    puts_raw(", 4 KiB pages on demand, one address space per task (");
    puts_raw(bsp_asid16 ? "16" : "8");
    puts_raw("-bit ASIDs), no TLB flush on task switches\n");
}

/* いまのタスクのユーザーメモリの中か（番地の形だけ。中身は読まない） / whether a range lies in the user memory of the current task
 * (addresses only; nothing is read) */
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

/* 要求時ページング: いまの EL0 のタスクのユーザーメモリの中の、まだ写していないページなら 4 KiB を写す（データは
 * そのスロットの物理ページ。像を書いた直後のページは命令キャッシュも合わせる）
 * demand paging: a page of the user memory of the current EL0 task that is not mapped yet gets 4 KiB mapped (backed by the
 * physical page of the slot; the instruction cache is brought in line for a page the image was just written to) */
static int bsp_demand_map(uint64_t far, uint64_t esr) {
    const uint32_t fsc = (uint32_t)(esr & 0x3Fu);
    if (fsc < 0x04u || fsc > 0x07u) return 0; /* 変換フォールトだけ / translation faults only */
    BspTask *t = bsp_cur_task();
    if (!t || !t->user || bsp_umem == 0 || far < bsp_uva || far - bsp_uva >= bsp_usize) return 0;
    const uint64_t va = far & ~0xFFFull;
    const uint64_t pa = (uint64_t)(uintptr_t)bsp_umem_of(bsp_core(), (uint32_t)bsp_sched()->cur) + (va - bsp_uva);
    for (uint64_t a = pa; a < pa + 4096u; a += 64u) __asm__ __volatile__("dc cvau, %0" : : "r"(a) : "memory");
    __asm__ __volatile__("dsb ish\n ic iallu\n dsb nsh\n isb" ::: "memory");
    if (!fascon_plat_vm_map(t->asid, va, pa, F20_VM_READ | F20_VM_WRITE | F20_VM_EXEC | F20_VM_USER)) return 0;
    bsp_sched()->faults++;
    return 1;
}

/* ユーザーメモリの写し: LDTRB / STTRB（EL0 の権限で読み書き。カーネルの番地は権限フォールト）と例外表。まだ写していない
 * ページは EL1 のデータアボートで要求時に写して命令をやり直す。ほかのフォールトは例外表の飛び先が err = 1 にする
 * user copies: LDTRB / STTRB (accesses with EL0 permissions; kernel addresses give permission faults) plus the exception
 * table. A page not mapped yet is mapped on demand by the EL1 data abort and the instruction is retried; any other fault
 * makes the exception-table fixup set err = 1 */
static inline uint32_t bsp_ldtrb(uint64_t ua, uint8_t *out) {
    uint64_t v = 0;
    uint32_t err = 0;
    __asm__ __volatile__("1: ldtrb %w0, [%2]\n"
                         "2:\n"
                         ".pushsection __ex_table, \"a\"\n.balign 8\n.quad 1b, 3f\n.popsection\n"
                         ".pushsection .text.fixup, \"ax\"\n3: mov %w1, #1\n b 2b\n.popsection\n"
                         : "+r"(v), "+r"(err)
                         : "r"(ua)
                         : "memory");
    *out = (uint8_t)v;
    return err;
}
static inline uint32_t bsp_sttrb(uint64_t ua, uint8_t v) {
    uint32_t err = 0;
    __asm__ __volatile__("1: sttrb %w1, [%2]\n"
                         "2:\n"
                         ".pushsection __ex_table, \"a\"\n.balign 8\n.quad 1b, 3f\n.popsection\n"
                         ".pushsection .text.fixup, \"ax\"\n3: mov %w0, #1\n b 2b\n.popsection\n"
                         : "+r"(err)
                         : "r"((uint64_t)v), "r"(ua)
                         : "memory");
    return err;
}
uint64_t fascon_plat_copy_from_user(void *dst, uint64_t uaddr, uint64_t n) {
    if (!bsp_user_range(uaddr, n)) return n;
    for (uint64_t i = 0; i < n; i++)
        if (bsp_ldtrb(uaddr + i, (uint8_t *)dst + i)) return n - i;
    return 0u;
}
uint64_t fascon_plat_copy_to_user(uint64_t uaddr, const void *src, uint64_t n) {
    if (!bsp_user_range(uaddr, n)) return n;
    for (uint64_t i = 0; i < n; i++)
        if (bsp_sttrb(uaddr + i, ((const uint8_t *)src)[i])) return n - i;
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
 * （マルチコアでは、ほかのコアの止まっているタスクも共有の gc_arena の根になる）
 * fascon-gc: one shadow stack per task; saved and restored at switches, and the collector reads suspended tasks too
 * (on multi-core, suspended tasks of other cores are roots of shared gc arenas as well) */
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

static f20_trapframe_t *bsp_switch(int to, f20_trapframe_t *frame, int save) {
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
    fascon_plat_vm_switch(t->user ? t->asid : 0u); /* TTBR0 を書くだけ（TLB は掃かない） / just TTBR0 (no TLB flush) */
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

/* パニックの全コア停止（SGI 2 を受けたコア）: すべての例外を止めて眠る / the all-core stop on panic (a core that took SGI 2):
 * mask every exception and sleep */
#if !defined(FASCON2_SMP)
static volatile uint32_t bsp_panic_acks;
#endif
static FASCON_NORETURN void bsp_panic_stop(void) {
    __asm__ __volatile__("msr daifset, #0xf" ::: "memory");
    __atomic_fetch_add(&bsp_panic_acks, 1u, __ATOMIC_RELEASE);
    for (;;) __asm__ __volatile__("wfi");
}
/* ランタイムの待ちループでパニックに気づいたコアも、ここで止まって数えられる（すべての例外を止めるので、SGI 2 で 2 度は
 * 数えない） / a core that noticed the panic in a waiting loop of the runtime stops and is counted here too (every
 * exception is masked, so SGI 2 never counts it twice) */
FASCON_NORETURN void fascon_plat_core_parked(void) { bsp_panic_stop(); }

/* IRQ のディスパッチ（start.S の el1_irq から）。戻り値 = 復帰するフレーム
 * IRQ dispatch (from el1_irq in start.S); returns the frame to restore */
f20_trapframe_t *bsp_irq(f20_trapframe_t *frame) {
    const uint32_t iar = MMIO32(GICC + 0x00C);
    const uint32_t intid = iar & 0x3FFu;
    if (intid >= 1020u) return frame; /* スプリアス / spurious */
    if (intid < 16u) { /* SGI: BSP の仕事 / an SGI: business of the BSP itself */
        MMIO32(GICC + 0x010) = iar;
        if (intid == BSP_SGI_PANIC) bsp_panic_stop();
        if (intid == BSP_SGI_VM) bsp_vm_serve(bsp_core());
#if defined(FASCON2_GENERIC_OS)
        genos_sgi(intid);
#endif
        return frame;
    }
    f20_sched_t *S = bsp_sched();
    if (intid == bsp_timer_intid) {
        bsp_timer_mask();
        S->ticks++;
    }
    void (*h)(void) = 0;
    for (uint32_t i = 0; i < bsp_irq_count; i++) {
        if (bsp_irqs[i].intid == intid) h = bsp_irqs[i].handler;
    }
    if (h == 0) fascon_plat_kernel_panic(FASCON_PANIC_IRQ, "interrupt without an irq handler");
    S->in_irq = 1;
    h();
    S->in_irq = 0;
    MMIO32(GICC + 0x010) = iar; /* EOI */
    if (S->pending >= 0) {
        const int to = S->pending;
        S->pending = -1;
        if (S->cur >= 0 && to != S->cur && bsp_tasks[bsp_core()][to].alive) frame = bsp_switch(to, frame, 1);
    }
    return frame;
}

/* コアのタスクがすべて終わったときの文脈（コアごとの小さなスタックの上。割り込みは開く） / the context used when every task
 * of a core is gone (on a small per-core stack, interrupts open) */
#define BSP_IDLE (-2)
static uint8_t bsp_idle_stack[BSP_NCORE][4096] __attribute__((aligned(16)));
static void bsp_idle_loop(void);
static f20_trapframe_t *bsp_idle_frame(uint32_t c) {
    f20_trapframe_t *f = (f20_trapframe_t *)(void *)(bsp_idle_stack[c] + sizeof bsp_idle_stack[c] - sizeof(f20_trapframe_t));
    __builtin_memset(f, 0, sizeof *f);
    f->elr = (uint64_t)(uintptr_t)bsp_idle_loop;
    f->spsr = 0x345u; /* EL1h、IRQ だけ許可 / EL1h with only IRQ unmasked */
    return f;
}

/* EL1 からの svc = 自発的な切り替え（start.S の el1_sync から） / an svc at EL1 = a voluntary switch */
f20_trapframe_t *bsp_el1_svc(f20_trapframe_t *frame) {
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
    BspTask *t = &bsp_tasks[bsp_core()][slot];
    const size_t fr = (stack_size / 4u) & ~(size_t)15u;
    uint8_t *base = (uint8_t *)stack;
    const uint64_t canary = FASCON_STACK_CANARY;
    __builtin_memcpy(base, &canary, sizeof canary);
    f20_trapframe_t *f = (f20_trapframe_t *)(void *)(base + stack_size - sizeof(f20_trapframe_t));
    __builtin_memset(f, 0, sizeof *f);
    f->elr = (uint64_t)(uintptr_t)bsp_task_trampoline;
    f->spsr = 0x345u; /* EL1h、IRQ だけ許可（D・A・F は起動時と同じく禁止） / EL1h with only IRQ unmasked */
    t->sp = f;
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
#if defined(FASCON2_GENERIC_OS)
    /* フレームリージョン | 守りページ | スタック。PAN = 1 のまま始める / frame region | guard page | stack; start with PAN = 1 */
    const size_t gfr = genos_task_layout(base, stack_size);
    if (gfr != 0u) t->f_end = base + gfr;
    f->spsr |= 1u << 22;
#endif
    return true;
}

/* EL0 のタスク: フレームは EL0t・割り込み許可・全レジスタ 0。カーネルスタックの一番上に置くので、EL0 からの例外は
 * いつも同じ場所にフレームを積む。アドレス空間は空で始まり、ページは触れたときに写す
 * an EL0 task: its frame is EL0t with interrupts enabled and every register zero. It sits at the very top of the kernel
 * stack, so exceptions from EL0 always push their frame at the same place. The address space starts empty and pages are
 * mapped when touched */
bool fascon_plat_task_create_user(uint32_t slot, f20_id_t id, uint64_t entry, uint64_t sp, void *stack, size_t stack_size, void (*on_exit)(void)) {
    if (bsp_umem == 0 || slot >= bsp_uslots) return false;
    if (!fascon_plat_task_create(slot, id, 0, 0, stack, stack_size, on_exit)) return false;
    const uint32_t c = bsp_core();
    BspTask *t = &bsp_tasks[c][slot];
    __builtin_memset(bsp_umem_of(c, slot), 0, (size_t)bsp_usize); /* 前のタスクの中身を残さない / nothing of an earlier task is left */
    t->asid = bsp_asid_of(c, slot);
    if (!fascon_plat_vm_space(t->asid)) return false;
    t->sp->elr = entry;
    t->sp->sp_el0 = sp & ~15ull;
    t->sp->spsr = 0x0u; /* EL0t、D・A・I・F すべて許可 / EL0t with nothing masked */
    t->user = 1;
    return true;
}

/* EL0 からの同期例外（start.S の el0_sync から）: svc はシステムコール、まだ写していないページは要求時に写してやり直し、
 * それ以外はそのタスクの不正な動作
 * a synchronous exception from EL0 (from el0_sync in start.S): an svc is a system call, a page not mapped yet is mapped
 * on demand and retried, anything else is a fault of the task */
f20_trapframe_t *bsp_el0_sync(f20_trapframe_t *frame) {
    uint64_t esr, far;
    __asm__ __volatile__("mrs %0, esr_el1" : "=r"(esr));
    __asm__ __volatile__("mrs %0, far_el1" : "=r"(far));
    const uint64_t ec = esr >> 26;
    if (ec == 0x15u) {
        const uint64_t nr = frame->x[8];
        int64_t r = -38; /* ENOSYS */
        if (nr < bsp_syscall_count && bsp_syscalls[nr] != 0) {
            /* 本体はタスクの文脈で動く（割り込み可・切り替え可）。フレームはこのタスクのスタックの上なので動かない
             * the body runs in task context (interruptible, may switch); the frame stays put on this task's stack */
            __asm__ __volatile__("msr daifclr, #2");
            r = bsp_syscalls[nr](frame->x);
            __asm__ __volatile__("msr daifset, #2");
        }
        frame->x[0] = (uint64_t)r;
        return frame;
    }
    if ((ec == 0x20u || ec == 0x24u) && bsp_demand_map(far, esr)) return frame; /* 要求時ページング / demand paging */
    const uint64_t pc = frame->elr;
    if (bsp_trap != 0) {
        __asm__ __volatile__("msr daifclr, #2");
        bsp_trap(esr, far, pc);
        __asm__ __volatile__("msr daifset, #2");
    } else {
        puts_raw("\n[bsp] user fault esr=");
        put_hex(esr);
        puts_raw(" far=");
        put_hex(far);
        puts_raw(" pc=");
        put_hex(pc);
        puts_raw("\n");
    }
    /* ハンドラが戻った（または無い）: そのタスクだけを終わらせる / the handler returned (or there is none): end only that task */
    bsp_cur_task()->on_exit();
    fascon_plat_task_exit();
}

#if defined(FASCON2_GENERIC_OS)
static f20_trapframe_t *genos_el1_fault(f20_trapframe_t *frame, uint64_t esr, uint64_t far);
#endif
/* EL1 の同期例外（SVC 以外）: ユーザーメモリの写し（LDTRB / STTRB）がまだ写していないページに触れたら要求時に写して
 * やり直す。例外表の飛び先があればそこへ（EFAULT の道）。それ以外は致命的
 * a synchronous EL1 exception (not an SVC): a user copy (LDTRB / STTRB) touching a page not mapped yet is mapped on demand
 * and retried; an exception-table fixup redirects to the EFAULT path; anything else is fatal */
f20_trapframe_t *bsp_el1_fault(f20_trapframe_t *frame) {
    uint64_t esr, far;
    __asm__ __volatile__("mrs %0, esr_el1\n mrs %1, far_el1" : "=r"(esr), "=r"(far));
    const uint64_t ec = esr >> 26;
    if (ec == 0x25u || ec == 0x24u) {
        const uint64_t fx = bsp_find_fixup(frame->elr);
        if (fx && bsp_demand_map(far, esr)) return frame; /* 写してやり直す / map and retry */
        if (fx) {
            frame->elr = fx; /* ユーザーメモリの写しの失敗 → EFAULT の道へ / a failed user copy → the EFAULT path */
            return frame;
        }
    }
#if defined(FASCON2_GENERIC_OS)
    return genos_el1_fault(frame, esr, far);
#else
    bsp_sync_exception(esr, frame->elr, far);
    return frame;
#endif
}

f20_id_t fascon_plat_task_current(void) {
    const BspTask *t = bsp_cur_task();
    return t ? t->id : 0u;
}

void fascon_plat_task_yield(uint32_t slot, f20_id_t id) {
    f20_sched_t *S = bsp_sched();
    if (S->in_irq) fascon_plat_kernel_panic(FASCON_PANIC_TASK, "yield_to inside an interrupt handler");
    BspTask *t = &bsp_tasks[bsp_core()][slot % FASCON2_TASK_MAX];
    if (S->cur < 0 || slot >= FASCON2_TASK_MAX || (int)slot == S->cur || !t->alive || t->id != id) return;
    const uint64_t d = fascon_plat_irq_save();
    S->svc_to = (int)slot;
    S->svc_save = 1;
    __asm__ __volatile__("svc #0");
    fascon_plat_irq_restore(d);
}

void fascon_plat_task_resume(uint32_t slot, f20_id_t id) {
    const BspTask *t = &bsp_tasks[bsp_core()][slot % FASCON2_TASK_MAX];
    if (slot < FASCON2_TASK_MAX && t->alive && t->id == id) bsp_sched()->pending = (int)slot;
}

FASCON_NORETURN void fascon_plat_task_enter(uint32_t slot, f20_id_t id) {
    const BspTask *t = &bsp_tasks[bsp_core()][slot % FASCON2_TASK_MAX];
    if (slot >= FASCON2_TASK_MAX || !t->alive || t->id != id) fascon_plat_kernel_panic(FASCON_PANIC_TASK, "enter: the task does not exist");
    bsp_gic_ppis_on();
    (void)fascon_plat_irq_save();
    f20_sched_t *S = bsp_sched();
    S->svc_to = (int)slot;
    S->svc_save = 0;
    __asm__ __volatile__("svc #0");
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
        if (t->user) fascon_plat_vm_drop(t->asid); /* ページの写しを外し、その ASID だけを掃く / unmap and flush that ASID only */
        for (uint32_t k = 1; k <= FASCON2_TASK_MAX; k++) {
            const uint32_t i = ((uint32_t)S->cur + k) % FASCON2_TASK_MAX;
            if (bsp_tasks[c][i].alive) {
                S->svc_to = (int)i;
                S->svc_save = 0;
                __asm__ __volatile__("svc #0");
            }
        }
    }
#if defined(FASCON2_SMP)
    /* マルチコア: このコアのタスクは尽きた。ほかのコアはまだ動いているかもしれないので、機械は止めずに休む（CPU0 は全コアの
     * タスクが尽きたら終わる）
     * multi-core: this core has no task left; other cores may still run, so the machine is not stopped and the core
     * idles (CPU0 ends the run once every core is out of tasks) */
    S->svc_to = BSP_IDLE;
    S->svc_save = 0;
    __asm__ __volatile__("svc #0");
    fascon_plat_kernel_panic(FASCON_PANIC_TASK, "idle returned");
#else
    fascon_plat_exit(0); /* 生きているタスクが無い / no live task is left */
#endif
}

uint32_t fascon_plat_cpu_id(void) {
    uint64_t m;
    __asm__ __volatile__("mrs %0, mpidr_el1" : "=r"(m));
    return (uint32_t)(m & 0xff);
}

/* ───────────── MMU（恒等マップ） / MMU (identity map) ─────────────
 * 仮想アドレス 39 ビット（T0SZ = 25）・4 KiB 粒度なので、表の引きはレベル 1（1 エントリ 1 GiB）から始まる
 * 39-bit VA (T0SZ = 25) with the 4 KiB granule, so a table walk starts at level 1 (1 GiB per entry)
 *
 *   L1[0] = 0〜1 GiB: デバイス（UART・fw_cfg など）の 1 GiB ブロック / a 1 GiB device block (UART, fw_cfg ...)
 *   L1[1] = 1〜2 GiB: RAM
 *     - RAM にフレームバッファがない → 1 GiB ブロック 1 枚（Normal WB）。引きはレベル 1 で終わる（記述子の読み出し 1 回）。
 *       カーネル・アリーナ・ヒープ・フレームが 1 つの変換に入る
 *       no framebuffer in RAM → a single 1 GiB block (Normal WB); the walk ends at level 1 (one descriptor read), and the
 *       kernel, arenas, heap and frame all sit under one translation
 *     - フレームバッファがある → 2 MiB ブロックのレベル 2 表（画面のブロックだけ Normal 非キャッシュ）。属性のそろった
 *       16 ブロック（32 MiB 境界）には連続ビット（bit 52）を立て、TLB の 1 エントリで 32 MiB を覆えるようにする
 *       with a framebuffer → a level-2 table of 2 MiB blocks (only the screen's blocks are Normal non-cacheable); every
 *       32 MiB-aligned group of 16 blocks with the same attributes gets the Contiguous bit (bit 52), so one TLB entry may
 *       cover 32 MiB
 *   4 KiB のページ（レベル 3）はどこにもない。表は起動時に 1 回書くだけ / there are no 4 KiB (level-3) pages anywhere,
 *   and the tables are written once at boot
 *   1 GiB ブロック・連続ビットの範囲を TLB の 1 エントリで持つかどうかは CPU の実装次第（持たない CPU でも正しく動き、
 *   引きの短さは変わらない） / whether a 1 GiB block or a contiguous range is held in one TLB entry is up to the CPU
 *   (CPUs that do not still run correctly, and the shorter walk is the same)
 * MAIR: attr0 = Device-nGnRnE, attr1 = Normal WB（RAM）, attr2 = Normal Non-cacheable（フレームバッファ / framebuffer） */
static uint64_t l1_table[512] __attribute__((aligned(4096)));
static uint64_t l2_ram[512] __attribute__((aligned(4096)));

static int in_framebuffer(uint64_t addr) {
    for (uint32_t i = 0; i < fascon2_mmio_region_count; i++) {
        const FasconMmioRegion *r = &fascon2_mmio_regions[i];
        if ((r->flags & FASCON_MMIO_FRAMEBUFFER) && addr < r->base + r->size && addr + BLOCK_2M > r->base) return 1;
    }
    return 0;
}

static void mmu_on(void);
static int mmu_ram_1g;      /* RAM を 1 GiB ブロックにしたか / RAM mapped as one 1 GiB block */
static uint32_t mmu_cont;   /* 連続ビットを立てた 32 MiB の組の数 / 32 MiB groups with the contiguous hint */

static void mmu_enable(void) {
#if defined(FASCON2_GENERIC_OS)
    genos_mmu_setup();
    mmu_on();
    return;
#endif
    l1_table[0] = 0x00000000ull | PTE_VALID | PTE_ATTR(0) | PTE_AF | PTE_PXN | PTE_UXN;
    int fb = 0;
    for (uint64_t i = 0; i < 512; i++)
        if (in_framebuffer(RAM_BASE + i * BLOCK_2M)) fb = 1;
    if (!fb) {
        /* レベル 1 のブロック記述子: [47:30] が出力アドレス / a level-1 block descriptor: output address in [47:30] */
        l1_table[1] = RAM_BASE | PTE_VALID | PTE_ATTR(1) | PTE_AF | PTE_SH_INNER | PTE_UXN; /* EL0 は実行しない / never executed at EL0 */
        mmu_ram_1g = 1;
    } else {
        l1_table[1] = (uint64_t)(uintptr_t)l2_ram | PTE_TABLE;
        for (uint64_t i = 0; i < 512; i++) {
            uint64_t pa = RAM_BASE + i * BLOCK_2M;
            if (in_framebuffer(pa)) {
                l2_ram[i] = pa | PTE_VALID | PTE_ATTR(2) | PTE_AF | PTE_SH_OUTER | PTE_UXN;
            } else {
                l2_ram[i] = pa | PTE_VALID | PTE_ATTR(1) | PTE_AF | PTE_SH_INNER | PTE_UXN;
            }
        }
        /* 16 個の属性がそろった組（出力アドレスは恒等マップなので連続・32 MiB 境界）に連続ビット
         * the contiguous hint on every group of 16 with identical attributes (identity map: addresses are contiguous and
         * 32 MiB aligned) */
        const uint64_t attr_mask = ~0x0000FFFFFFE00000ull; /* 出力アドレス以外のビット / everything but the output address */
        for (uint32_t g = 0; g < 512u / CONT_BLOCKS; g++) {
            const uint64_t a0 = l2_ram[g * CONT_BLOCKS] & attr_mask;
            int same = 1;
            for (uint32_t k = 1; k < CONT_BLOCKS; k++)
                if ((l2_ram[g * CONT_BLOCKS + k] & attr_mask) != a0) same = 0;
            if (same) {
                for (uint32_t k = 0; k < CONT_BLOCKS; k++) l2_ram[g * CONT_BLOCKS + k] |= PTE_CONT;
                mmu_cont++;
            }
        }
    }
    mmu_on();
}

/* レジスタの設定と MMU の有効化（どのコアも同じ表・同じ属性。RAM は inner shareable なので、コアの間でキャッシュの
 * 一貫性が保たれる）
 * program the registers and turn the MMU on (every core uses the same tables and attributes; RAM is inner shareable,
 * so caches stay coherent between cores) */
static void mmu_on(void) {
    uint64_t mair = 0x00ull | (0xFFull << 8) | (0x44ull << 16);
    /* ASID の幅: ID_AA64MMFR0_EL1.ASIDBits = 2 なら 16 ビット（TCR_EL1.AS = 1）/ ASID width: 16 bits when ASIDBits = 2 */
    uint64_t mmfr0;
    __asm__ __volatile__("mrs %0, id_aa64mmfr0_el1" : "=r"(mmfr0));
    bsp_asid16 = ((mmfr0 >> 4) & 0xFu) == 2u;
    uint64_t tcr = 25ull                 /* T0SZ: 39 ビットの仮想アドレス / 39-bit VA */
                   | (bsp_asid16 ? (1ull << 36) : 0ull) /* AS: 16 ビットの ASID / 16-bit ASIDs */
                   | (1ull << 8)         /* IRGN0 = WB WA */
                   | (1ull << 10)        /* ORGN0 = WB WA */
                   | (3ull << 12)        /* SH0 = inner shareable */
                   | (1ull << 23)        /* EPD1: TTBR1 は使わない / TTBR1 unused */
#if defined(FASCON2_GENERIC_OS)
                   | (2ull << 32);       /* IPS = 40 ビット（高位の PCIe ECAM のため）/ 40-bit PA (for the high PCIe ECAM) */
#else
                   | (1ull << 32);       /* IPS = 36 ビット / 36-bit PA */
#endif
    __asm__ __volatile__("dsb ish" ::: "memory");
    __asm__ __volatile__("msr mair_el1, %0" : : "r"(mair));
    __asm__ __volatile__("msr tcr_el1, %0" : : "r"(tcr));
    __asm__ __volatile__("msr ttbr0_el1, %0" : : "r"((uint64_t)(uintptr_t)l1_table));
    __asm__ __volatile__("isb; tlbi vmalle1; dsb ish; isb" ::: "memory");
    uint64_t sctlr;
    __asm__ __volatile__("mrs %0, sctlr_el1" : "=r"(sctlr));
    sctlr |= (1ull << 0) | (1ull << 2) | (1ull << 12); /* M, C, I */
    sctlr &= ~(1ull << 1);                              /* A: 非整列アクセスを許す / allow unaligned access */
    __asm__ __volatile__("msr sctlr_el1, %0; isb" : : "r"(sctlr) : "memory");
}

#if defined(FASCON2_GENERIC_OS)
#include "genos.c"
#endif

/* アリーナがどの 2 MiB ブロックに乗っているかを表示 / print which 2 MiB blocks the arenas occupy */
static void report_arenas(void) {
    const uint64_t s = (uint64_t)(uintptr_t)__arena_start, e = (uint64_t)(uintptr_t)__arena_end;
    if (fascon2_arena_region_count == 0 || e <= s) return;
    const uint64_t b0 = s / BLOCK_2M, b1 = (e - 1) / BLOCK_2M;
    puts_raw("[bsp] arenas: ");
    put_dec(fascon2_arena_region_count);
    puts_raw(fascon2_arena_region_count == 1 ? " table, " : " tables, ");
    put_hex(s);
    puts_raw("..");
    put_hex(e);
    puts_raw(" (");
    put_dec((e - s) >> 10);
    puts_raw(" KiB, 2 MiB aligned) -> ");
    if (mmu_ram_1g) {
        puts_raw("inside the 1 GiB RAM block (1 TLB entry for all RAM)\n");
    } else {
        put_dec(b1 - b0 + 1);
        puts_raw(" block(s) of 2 MiB");
        const uint64_t g0 = (b0 * BLOCK_2M - RAM_BASE) / (BLOCK_2M * CONT_BLOCKS), g1 = (b1 * BLOCK_2M - RAM_BASE) / (BLOCK_2M * CONT_BLOCKS);
        if ((l2_ram[g0 * CONT_BLOCKS] & PTE_CONT) && (l2_ram[g1 * CONT_BLOCKS] & PTE_CONT)) {
            puts_raw(" in ");
            put_dec(g1 - g0 + 1);
            puts_raw(" contiguous 32 MiB range(s)");
        }
        puts_raw("\n");
    }
}

/* ───────────── fw_cfg + ramfb（QEMU の簡易フレームバッファ） / fw_cfg + ramfb (QEMU's simple framebuffer) ───────────── */

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

static int fwcfg_dma(uint32_t control, void *buf, uint32_t len) {
    static volatile FwCfgDma d __attribute__((aligned(16)));
    d.control = __builtin_bswap32(control);
    d.length = __builtin_bswap32(len);
    d.address = __builtin_bswap64((uint64_t)(uintptr_t)buf);
    __asm__ __volatile__("dsb sy" ::: "memory");
    uint64_t pa = (uint64_t)(uintptr_t)&d;
    /* DMA アドレスレジスタはビッグエンディアン 64 ビット。下位を書いた瞬間に転送が始まる
     * the DMA address register is big-endian 64-bit; writing the low half starts the transfer */
    fascon_mmio_write32(FWCFG + 0x10, __builtin_bswap32((uint32_t)(pa >> 32)));
    fascon_mmio_write32(FWCFG + 0x14, __builtin_bswap32((uint32_t)pa));
    for (int spin = 0; spin < 1000000; spin++) {
        uint32_t c = __builtin_bswap32(d.control);
        if ((c & ~FW_CFG_DMA_ERROR) == 0) return (c & FW_CFG_DMA_ERROR) ? -1 : 0;
    }
    return -1;
}

static int fwcfg_present(void) {
    /* 選択子 0（署名）を選んで "QEMU" を読む / select key 0 (signature) and read "QEMU" */
    fascon_mmio_write16(FWCFG + 0x08, __builtin_bswap16(0x0000));
    char sig[4];
    for (int i = 0; i < 4; i++) sig[i] = (char)fascon_mmio_read8(FWCFG + 0x00);
    return sig[0] == 'Q' && sig[1] == 'E' && sig[2] == 'M' && sig[3] == 'U';
}

static int fwcfg_find(const char *name, uint16_t *sel) {
    uint32_t count_be = 0;
    if (fwcfg_dma((FW_CFG_FILE_DIR << 16) | FW_CFG_DMA_SELECT | FW_CFG_DMA_READ, &count_be, 4) != 0) return 0;
    uint32_t count = __builtin_bswap32(count_be);
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
        puts_raw("[bsp] ramfb not found: start QEMU with -device ramfb to see the screen\n");
        return;
    }
    static RamfbCfg cfg __attribute__((aligned(16)));
    cfg.addr = __builtin_bswap64((uint64_t)fb->base);
    cfg.fourcc = __builtin_bswap32(0x34325258u); /* DRM_FORMAT_XRGB8888 = 'XR24' */
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

/* ───────────── 入口 / entry ───────────── */

/* firmware.S（-bios の像）から起動したら 1。-kernel のときは 0 のまま（.data なので start.S の .bss の 0 埋めでは消えない）
 * 1 when booted through firmware.S (the -bios image); stays 0 with -kernel (in .data, so start.S's .bss zeroing keeps it) */
volatile uint32_t bsp_fw_boot __attribute__((section(".data"))) = 0;

/* ───────────── マルチコア（SMP） / multi-core ─────────────
 * - CPU0 が PSCI の CPU_ON（0xC400_0003。EL1 で起動したなら HVC、EL2 から降りたなら SMC）でコア 1〜3 を起動する。
 *   仮想 BIOS（-bios）からの起動では PSCI が無いので、コアは 1 つのまま
 *   CPU0 starts cores 1 to 3 with PSCI CPU_ON (0xC400_0003; HVC when booted at EL1, SMC after dropping from EL2). Under
 *   the virtual BIOS (-bios) there is no PSCI, so the machine stays single-core
 * - どのコアも、TPIDR_EL0 を自分の TLS ブロック（.percpu の中）へ向ける。ふつうのアリーナ・フレームリージョン・own ヒープは
 *   コアごとの実体になる
 *   every core points TPIDR_EL0 at its own TLS block (inside .percpu); plain arenas, the frame region and the own heap
 *   become per-core instances
 * - 起動したコアは仕事（spawn_on）を待つ。関数が戻ると、次の仕事を待つ
 *   a started core waits for work (spawn_on); when the function returns it waits for the next */
#if defined(FASCON2_SMP)
extern char __percpu_start[], __percpu_end[], __tls_size[], __tls_align[], __smp_frame_start[];
void _secondary_start(void);
static struct {
    void (*fn)(uint64_t);
    uint64_t arg;
    uint32_t busy;
    uint32_t online;
    uint8_t pad_[128 - 24];
} bsp_cores[FASCON2_MAX_CPUS] __attribute__((aligned(128)));
static uint32_t bsp_cpu_count = 1;
static uint32_t bsp_psci_smc = 0;
volatile uint32_t bsp_boot_el2 __attribute__((section(".data"))) = 0; /* start.S が立てる / set by start.S */

static void bsp_tls_setup(uint32_t core) {
    const uint64_t size = (uint64_t)(uintptr_t)__tls_size, align = (uint64_t)(uintptr_t)__tls_align > 16u ? (uint64_t)(uintptr_t)__tls_align : 16u;
    const uint64_t stride = (uint64_t)(__percpu_end - __percpu_start) / 4u;
    uint8_t *block = (uint8_t *)__percpu_start + core * stride;
    for (uint64_t i = 0; i < size; i++) block[i] = 0;
    /* AArch64 の TLS: 変数の番地 = TP + (16 を整列まで切り上げた値) + ブロック内の位置
     * AArch64 TLS: a variable lives at TP + (16 rounded up to the alignment) + its offset in the block */
    const uint64_t tp = (uint64_t)(uintptr_t)block - ((16u + align - 1u) & ~(align - 1u));
    __asm__ __volatile__("msr tpidr_el0, %0; isb" : : "r"(tp));
}

static void bsp_core_memory(uint32_t core) {
    const size_t heap = (size_t)(__heap_end - __heap_start) / FASCON2_MAX_CPUS;
    fascon_heap_init(__heap_start + core * heap, heap);
    if (core == 0) {
        fascon_frame_init(__frame_start, (size_t)(__frame_end - __frame_start));
    } else {
        fascon_frame_init(__smp_frame_start + (core - 1u) * 0x100000u, 0x100000u);
    }
}

void fascon2_secondary_main(uint64_t core) {
    mmu_on();
#if defined(FASCON2_GENERIC_OS)
    genos_secondary((uint32_t)core);
#endif
    bsp_tls_setup((uint32_t)core);
    fascon_smp_core_init((uint32_t)core);
    bsp_core_memory((uint32_t)core);
    /* GIC の窓口と BSP の SGI（パニックの停止・TLB の撃ち落とし）を開き、IRQ を許可する。プログラムの割り込み（PPI）は、
     * そのコアが irq_open / enter するまで許可しない（それまでコアごとの実体は起動時のまま）
     * open the GIC interface and the SGIs of the BSP (the panic stop, TLB shootdowns) and unmask IRQs. The interrupts
     * of the program (PPIs) stay disabled until this core calls irq_open / enter (its per-core instances are still at boot) */
    bsp_gic_core_on();
    __asm__ __volatile__("msr daifclr, #2" ::: "memory");
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
    }
}

static int64_t bsp_psci(uint64_t fn, uint64_t a1, uint64_t a2, uint64_t a3) {
    register uint64_t x0 __asm__("x0") = fn;
    register uint64_t x1 __asm__("x1") = a1;
    register uint64_t x2 __asm__("x2") = a2;
    register uint64_t x3 __asm__("x3") = a3;
    if (bsp_psci_smc) {
        __asm__ __volatile__("smc #0" : "+r"(x0), "+r"(x1), "+r"(x2), "+r"(x3) : : "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13", "x14", "x15", "x16", "x17", "memory");
    } else {
        __asm__ __volatile__("hvc #0" : "+r"(x0), "+r"(x1), "+r"(x2), "+r"(x3) : : "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13", "x14", "x15", "x16", "x17", "memory");
    }
    return (int64_t)x0;
}

static void bsp_smp_start(void) {
    bsp_psci_smc = bsp_boot_el2;
    if (bsp_fw_boot) {
        puts_raw("[bsp] SMP: booted by the virtual BIOS (no PSCI) -> 1 core\n");
        return;
    }
    for (uint32_t c = 1; c < FASCON2_MAX_CPUS; c++) {
        if (bsp_psci(0xC4000003ull, c, (uint64_t)(uintptr_t)_secondary_start, c) != 0) break; /* そのコアは無い / no such core */
        for (uint32_t spin = 0; spin < 200000000u && !__atomic_load_n(&bsp_cores[c].online, __ATOMIC_ACQUIRE); spin++) FASCON_CPU_RELAX();
        if (!bsp_cores[c].online) break;
        bsp_cpu_count = c + 1u;
    }
    puts_raw("[bsp] SMP: ");
    put_dec(bsp_cpu_count);
    puts_raw(" cores online (PSCI CPU_ON via ");
    puts_raw(bsp_psci_smc ? "SMC" : "HVC");
    puts_raw("), per-core stacks 128 KiB, frames 1024 KiB, own heaps ");
    put_dec((uint64_t)(__heap_end - __heap_start) / FASCON2_MAX_CPUS >> 10);
    puts_raw(" KiB, TLS block ");
    put_dec((uint64_t)(uintptr_t)__tls_size >> 10);
    puts_raw(" KiB per core\n");
}

uint32_t fascon_plat_cpu_count(void) { return bsp_cpu_count; }

/* TLB の撃ち落としの自己試験: ほかの全コアへ SGI 3 を送り、全部が答えること（タスクはコアを移らないので、ふだんの
 * アドレス空間の掃除は自分のコアだけで済み、SGI 3 の道はここで確かめる）
 * a self-test of the TLB shootdown: SGI 3 to every other core, and every one of them must answer (tasks never move
 * between cores, so ordinary address-space flushes stay on the owning core; the SGI 3 path is checked here) */
static void bsp_tlb_selftest(void) {
    if (bsp_cpu_count <= 1u) return;
    const uint64_t before = __atomic_load_n(&bsp_vm_flush_served, __ATOMIC_ACQUIRE);
    fascon_plat_vm_flush(0u, UINT64_MAX, (1ull << bsp_cpu_count) - 1u);
    puts_raw("[bsp] TLB shootdown: SGI 3 answered by ");
    put_dec(__atomic_load_n(&bsp_vm_flush_served, __ATOMIC_ACQUIRE) - before);
    puts_raw(" of ");
    put_dec(bsp_cpu_count - 1u);
    puts_raw(" other core(s) (self-test)\n");
}

/* パニックの全コア停止: SGI 2 を「自分以外の全コア」へ（GICD_SGIR の TargetListFilter = 01）。受けたコアはすべての例外を
 * 止めて眠る。割り込みを止めた区間にいるコアも、区間を出た瞬間に受ける（それまでは待ちループと出力の入口で旗を見る）
 * the all-core stop on panic: SGI 2 to "every core but this one" (TargetListFilter = 01 of GICD_SGIR). A core that takes
 * it masks every exception and sleeps; a core inside a masked section takes it the moment it leaves (until then it
 * watches the flag at waiting loops and the console) */
void fascon_plat_panic_broadcast(void) {
    if (bsp_cpu_count <= 1u || bsp_panic_sent) return;
    bsp_panic_sent = 1u;
    __asm__ __volatile__("dsb ish" ::: "memory");
    MMIO32(GICD + 0xF00) = (1u << 24) | BSP_SGI_PANIC;
    for (uint64_t spin = 0; spin < 50000000ull && __atomic_load_n(&bsp_panic_acks, __ATOMIC_ACQUIRE) < bsp_cpu_count - 1u; spin++) FASCON_CPU_RELAX();
}

/* CPU0 の休み: 全コアのタスクが尽き、走っている spawn_on の関数も無ければ終わる / CPU0 idling: ends the run once every core
 * is out of tasks and no spawn_on function is still running */
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
        __asm__ __volatile__("wfi");
    }
}
#else
static void bsp_idle_loop(void) {
    for (;;) __asm__ __volatile__("wfi");
}
volatile uint32_t bsp_boot_el2 __attribute__((section(".data"))) = 0;
uint32_t fascon_plat_cpu_count(void) { return 1u; }
void fascon_plat_spawn_on(uint32_t core, void (*fn)(uint64_t), uint64_t arg) {
    (void)core;
    (void)fn;
    (void)arg;
    fascon_plat_kernel_panic(FASCON_PANIC_SMP, "spawn_on: this boot path runs one core only (use `fascon qemu` without --target)");
}
#endif

void bsp_main(void) {
    if (bsp_fw_boot) {
        puts_raw("[bsp] Fascon 1.0 bare-metal: QEMU virt AArch64, booted by the Fascon virtual BIOS (flash at 0x0 -> RAM 0x40080000, EL");
    } else {
        puts_raw("[bsp] Fascon 1.0 bare-metal: QEMU virt AArch64, no BIOS / no firmware (EL");
    }
    uint64_t el;
    __asm__ __volatile__("mrs %0, CurrentEL" : "=r"(el));
    put_dec((el >> 2) & 3);
    puts_raw(")\n");
    mmu_enable();
#if defined(FASCON2_SMP)
    bsp_tls_setup(0); /* コアごとの実体（アリーナ・フレーム）に触る前に / before anything per-core (arenas, the frame) is touched */
    fascon_smp_core_init(0);
    bsp_gic_core_on(); /* SGI（パニックの停止・TLB の撃ち落とし）を受けられるように / so this core can take SGIs */
#endif
#if defined(FASCON2_GENERIC_OS)
    genos_boot();
#else
    if (mmu_ram_1g) {
        puts_raw("[bsp] MMU + caches on (identity map, no 4 KiB pages: RAM = one 1 GiB block, 1-level walk)\n");
    } else {
        puts_raw("[bsp] MMU + caches on (identity map, no 4 KiB pages: RAM in 2 MiB blocks, ");
        put_dec(mmu_cont);
        puts_raw(" of 32 groups contiguous = 32 MiB TLB entries; framebuffer pages are Normal non-cacheable)\n");
    }
#endif
    report_arenas();
    /* MMIO リージョンがカーネル像・スタック・ヒープと重なっていないか / MMIO regions must not overlap the kernel */
    uint64_t ks = (uint64_t)(uintptr_t)__kernel_start, ke = (uint64_t)(uintptr_t)__kernel_end;
    for (uint32_t i = 0; i < fascon2_mmio_region_count; i++) {
        const FasconMmioRegion *r = &fascon2_mmio_regions[i];
        if (r->base < ke && r->base + r->size > ks) fascon_plat_kernel_panic(0x2203u, "MMIO region overlaps the kernel image");
    }
    setup_framebuffer();
#if defined(FASCON2_SMP)
    bsp_core_memory(0);
    bsp_smp_start();
    bsp_tlb_selftest();
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
    puts_raw(" address spaces dropped (TLB flush by ASID: local ");
    put_dec(bsp_vm_flush_local);
    puts_raw(", IPI ");
    put_dec(bsp_vm_flush_ipi);
    puts_raw(" served ");
    put_dec(bsp_vm_flush_served);
    puts_raw("), task switches ");
    put_dec(sw);
    puts_raw(" with no TLB flush, timer ticks ");
    put_dec(ticks);
    puts_raw(", page tables peak ");
    put_dec(bsp_pt_peak);
    puts_raw("\n");
}
