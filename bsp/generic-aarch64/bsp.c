/* bsp.c — 汎用 AArch64 BSP: board.toml（fascon_board.h）だけで実機の像を作る / the generic AArch64 BSP: a real-hardware
 * image made from board.toml (fascon_board.h) alone
 *
 *   コンソール: pl011 / ns16550（board.toml の [uart]）/ console: pl011 / ns16550 ([uart] of board.toml)
 *   MMU: 恒等写像。[memory] の RAM は Normal WB、それ以外は Device（2 MiB ブロック）/ identity map: [memory] RAM is Normal
 *        write-back, everything else Device (2 MiB blocks)
 *   タイマー: 汎用タイマー（CNTFRQ が 0 なら [timer] hz）/ timer: the generic timer ([timer] hz when CNTFRQ is 0)
 *   SMP: [boot] smp = "spin-table"（release_addrs へ入口を書いて SEV）/ "psci-hvc" / "psci-smc"（CPU_ON）/ "none"
 *   DMA: [dma] coherent = false → submit_dma / complete_dma でキャッシュラインの掃き出し・無効化（fascon2_rt.h）
 *   終了: [boot] exit = "semihosting" / "psci" / "loop"
 *   割り込み・タスク・EL0 はこの雛形には無い（呼ぶと分かる理由で止まる）。qemu-virt-aarch64 の BSP を手本に足す
 *   interrupts, tasks and EL0 are not in this template (calling them stops with a clear reason); add them following the
 *   qemu-virt-aarch64 BSP */
#include "fascon2_rt.h"
#include "fascon_board.h"

#define MMIO32(a) (*(volatile uint32_t *)(uintptr_t)(a))
#define MMIO8(a) (*(volatile uint8_t *)(uintptr_t)(a))

extern char __heap_start[], __heap_end[], __frame_start[], __frame_end[], __kernel_start[], __kernel_end[];
extern char __percpu_start[], __percpu_end[], __tls_size[], __tls_align[];
extern char _start[], bsp_vectors[];
void fascon2_main(void);

/* ───── コンソール / console ───── */
static void uart_putc(char c) {
#if defined(FASCON_BOARD_UART_PL011)
    while (MMIO32(FASCON_BOARD_UART_BASE + 0x18) & (1u << 5)) {
    }
    MMIO32(FASCON_BOARD_UART_BASE) = (uint32_t)(uint8_t)c;
#elif defined(FASCON_BOARD_UART_NS16550) || defined(FASCON_BOARD_UART_8250)
    while (!(MMIO8(FASCON_BOARD_UART_BASE + (5u << FASCON_BOARD_UART_SHIFT)) & 0x20u)) {
    }
    MMIO8(FASCON_BOARD_UART_BASE) = (uint8_t)c;
#else
    (void)c;
#endif
}
static volatile uint32_t bsp_console_lock;
void fascon_plat_print(const char *s, size_t len) {
    while (__atomic_exchange_n(&bsp_console_lock, 1u, __ATOMIC_ACQUIRE)) {
    }
    for (size_t i = 0; i < len; i++) {
        if (s[i] == '\n') uart_putc('\r');
        uart_putc(s[i]);
    }
    __atomic_store_n(&bsp_console_lock, 0u, __ATOMIC_RELEASE);
}
static void puts_raw(const char *s) {
    size_t n = 0;
    while (s[n]) n++;
    fascon_plat_print(s, n);
}
static void put_hex(uint64_t v) {
    char b[18] = "0x";
    for (int i = 0; i < 16; i++) b[2 + i] = "0123456789abcdef"[(v >> (60 - 4 * i)) & 15u];
    fascon_plat_print(b, 18);
}
static void put_dec(uint64_t v) {
    char b[21];
    int n = 0;
    do {
        b[20 - n++] = (char)('0' + v % 10u);
        v /= 10u;
    } while (v);
    fascon_plat_print(&b[21 - n], (size_t)n);
}

/* ───── 終了 / exit ───── */
static int64_t bsp_psci(uint64_t fn, uint64_t a1, uint64_t a2, uint64_t a3) {
    register uint64_t x0 __asm__("x0") = fn;
    register uint64_t x1 __asm__("x1") = a1;
    register uint64_t x2 __asm__("x2") = a2;
    register uint64_t x3 __asm__("x3") = a3;
#if defined(FASCON_BOARD_SMP_PSCI_SMC) || defined(FASCON_BOARD_PSCI_SMC)
    __asm__ __volatile__("smc #0" : "+r"(x0), "+r"(x1), "+r"(x2), "+r"(x3) : : "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13", "x14", "x15", "x16", "x17", "memory");
#else
    __asm__ __volatile__("hvc #0" : "+r"(x0), "+r"(x1), "+r"(x2), "+r"(x3) : : "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13", "x14", "x15", "x16", "x17", "memory");
#endif
    return (int64_t)x0;
}
FASCON_NORETURN void fascon_plat_exit(int code) {
    puts_raw("[board] exit code ");
    put_dec((uint64_t)(int64_t)code);
    puts_raw("\n");
#if defined(FASCON_BOARD_EXIT_SEMIHOSTING)
    /* SYS_EXIT（ADP_Stopped_ApplicationExit と終了コード） / SYS_EXIT with ADP_Stopped_ApplicationExit and the code */
    static volatile uint64_t blk[2];
    blk[0] = 0x20026u;
    blk[1] = (uint64_t)(int64_t)code;
    register uint64_t x0 __asm__("x0") = 0x18u;
    register uint64_t x1 __asm__("x1") = (uint64_t)(uintptr_t)blk;
    __asm__ __volatile__("hlt #0xf000" : : "r"(x0), "r"(x1) : "memory");
#elif defined(FASCON_BOARD_EXIT_PSCI)
    (void)bsp_psci(0x84000008u, 0, 0, 0); /* SYSTEM_OFF */
#endif
    for (;;) __asm__ __volatile__("wfe");
}
FASCON_NORETURN void fascon_plat_halt(void) { fascon_plat_exit(0); }
FASCON_NORETURN void fascon_plat_kernel_panic(uint32_t code, const char *msg) {
    puts_raw("\nFASCON PANIC ");
    put_hex(code);
    puts_raw(" ");
    puts_raw(msg);
    puts_raw("\n");
    fascon_plat_exit(70);
}
void bsp_exception(uint64_t esr, uint64_t elr, uint64_t far) {
    puts_raw("\nSYNC/ASYNC EXCEPTION esr=");
    put_hex(esr);
    puts_raw(" elr=");
    put_hex(elr);
    puts_raw(" far=");
    put_hex(far);
    puts_raw("\n");
    fascon_plat_exit(71);
}

/* ───── 割り込み状態・タイマー / interrupt state and timer ───── */
uint64_t fascon_plat_irq_save(void) {
    uint64_t d;
    __asm__ __volatile__("mrs %0, daif\n msr daifset, #2" : "=r"(d) : : "memory");
    return d;
}
void fascon_plat_irq_restore(uint64_t state) { __asm__ __volatile__("msr daif, %0" : : "r"(state) : "memory"); }
void fascon_plat_irq_open(void) {}
void fascon_plat_wait_irq(void) { __asm__ __volatile__("wfe"); }
uint64_t fascon_plat_timer_freq(void) {
    uint64_t f;
    __asm__ __volatile__("mrs %0, cntfrq_el0" : "=r"(f));
#if defined(FASCON_BOARD_TIMER_HZ)
    if (f == 0u) f = FASCON_BOARD_TIMER_HZ;
#endif
    return f;
}
uint64_t fascon_plat_timer_now(void) {
    uint64_t t;
    __asm__ __volatile__("isb\n mrs %0, cntpct_el0" : "=r"(t));
    return t;
}
static FASCON_NORETURN void bsp_unsupported(const char *what) {
    puts_raw("[board] ");
    puts_raw(what);
    puts_raw(": this board package has no interrupt controller / task driver (add it following bsp/qemu-virt-aarch64)\n");
    fascon_plat_kernel_panic(0x0013u, "unsupported on this board package");
}
void fascon_plat_timer_arm(uint64_t ticks) {
    (void)ticks;
    bsp_unsupported("timer interrupt");
}
void fascon_plat_irq_register(const FasconIrqEntry *table, uint32_t count) {
    (void)table;
    if (count != 0u) bsp_unsupported("interrupt handlers");
}
void fascon_plat_syscall_register(const FasconSyscallFn *table, uint32_t count) {
    (void)table;
    (void)count;
    bsp_unsupported("system calls");
}
void fascon_plat_trap_register(void (*handler)(uint64_t, uint64_t, uint64_t)) {
    (void)handler;
    bsp_unsupported("EL0 traps");
}
void fascon_plat_user_register(uint8_t *mem, uint64_t size, uint64_t va, uint32_t slots) {
    (void)mem;
    (void)size;
    (void)va;
    (void)slots;
    bsp_unsupported("user memory");
}
bool fascon_plat_user_window(uint64_t addr, uint64_t len, f20_slice_t *out) {
    (void)addr;
    (void)len;
    (void)out;
    return false;
}
bool fascon_plat_task_create(uint32_t slot, f20_id_t id, void (*entry)(uint64_t), uint64_t arg, void *stack, size_t stack_size, void (*on_exit)(void)) {
    (void)slot, (void)id, (void)entry, (void)arg, (void)stack, (void)stack_size, (void)on_exit;
    bsp_unsupported("tasks");
}
bool fascon_plat_task_create_user(uint32_t slot, f20_id_t id, uint64_t entry, uint64_t sp, void *stack, size_t stack_size, void (*on_exit)(void)) {
    (void)slot, (void)id, (void)entry, (void)sp, (void)stack, (void)stack_size, (void)on_exit;
    bsp_unsupported("EL0 tasks");
}
f20_id_t fascon_plat_task_current(void) { return 0u; }
void fascon_plat_task_yield(uint32_t slot, f20_id_t id) {
    (void)slot;
    (void)id;
}
void fascon_plat_task_resume(uint32_t slot, f20_id_t id) {
    (void)slot;
    (void)id;
}
FASCON_NORETURN void fascon_plat_task_enter(uint32_t slot, f20_id_t id) {
    (void)slot;
    (void)id;
    bsp_unsupported("tasks");
}
FASCON_NORETURN void fascon_plat_task_exit(void) { fascon_plat_exit(0); }

/* ───── MMU: 恒等写像（4 GiB まで。2 MiB ブロック）/ MMU: identity map (up to 4 GiB, 2 MiB blocks) ───── */
static uint64_t bsp_l1[512] __attribute__((aligned(4096)));
static uint64_t bsp_l2[4][512] __attribute__((aligned(4096)));
static void mmu_on(void) {
    __asm__ __volatile__("msr mair_el1, %0" : : "r"(0x0000000000ff0004ull)); /* 0 = Device-nGnRE, 1 = Normal WB */
    const uint64_t tcr = 25ull | (1ull << 8) | (1ull << 10) | (3ull << 12) | (2ull << 32);
    __asm__ __volatile__("msr tcr_el1, %0\n msr ttbr0_el1, %1\n isb" : : "r"(tcr), "r"((uint64_t)(uintptr_t)bsp_l1));
    __asm__ __volatile__("dsb ish\n tlbi vmalle1\n dsb ish\n isb" ::: "memory");
    uint64_t sctlr;
    __asm__ __volatile__("mrs %0, sctlr_el1" : "=r"(sctlr));
    sctlr |= (1u << 0) | (1u << 2) | (1u << 12); /* M, C, I */
    sctlr &= ~(1ull << 1);                       /* A = 0 */
    __asm__ __volatile__("msr sctlr_el1, %0\n isb" : : "r"(sctlr) : "memory");
}
static void mmu_setup(void) {
    for (uint64_t g = 0; g < 4; g++) {
        for (uint64_t i = 0; i < 512; i++) {
            const uint64_t pa = (g << 30) | (i << 21);
            const int ram = pa >= FASCON_BOARD_RAM_BASE && pa < FASCON_BOARD_RAM_BASE + FASCON_BOARD_RAM_SIZE;
            bsp_l2[g][i] = pa | 1u | (1u << 10) | (ram ? ((1u << 2) | (3u << 8)) : ((0u << 2) | (1ull << 53) | (1ull << 54)));
        }
        bsp_l1[g] = (uint64_t)(uintptr_t)bsp_l2[g] | 3u;
    }
    __asm__ __volatile__("dsb ish" ::: "memory");
}

/* ───── TLS・コアごとのメモリ / TLS and per-core memory ───── */
static void bsp_tls_setup(uint32_t core) {
    const uint64_t size = (uint64_t)(uintptr_t)__tls_size, align = (uint64_t)(uintptr_t)__tls_align > 16u ? (uint64_t)(uintptr_t)__tls_align : 16u;
    const uint64_t stride = (uint64_t)(__percpu_end - __percpu_start) / FASCON_BOARD_CPUS;
    uint8_t *block = (uint8_t *)__percpu_start + core * stride;
    for (uint64_t i = 0; i < size; i++) block[i] = 0;
    const uint64_t tp = (uint64_t)(uintptr_t)block - ((16u + align - 1u) & ~(align - 1u));
    __asm__ __volatile__("msr tpidr_el0, %0; isb" : : "r"(tp));
}
static void bsp_core_memory(uint32_t core) {
    const size_t heap = (size_t)(__heap_end - __heap_start) / FASCON_BOARD_CPUS;
    const size_t frame = (size_t)(__frame_end - __frame_start) / FASCON_BOARD_CPUS;
    fascon_heap_init(__heap_start + core * heap, heap);
    fascon_frame_init(__frame_start + core * frame, frame);
}

/* ───── SMP: 2 番目以降のコアの起こし方は board.toml の [boot] smp / SMP: how secondaries wake is [boot] smp ───── */
static struct {
    void (*fn)(uint64_t);
    uint64_t arg;
    uint32_t busy;
    uint32_t online;
    uint8_t pad_[128 - 24];
} bsp_cores[FASCON_BOARD_CPUS] __attribute__((aligned(128)));
static uint32_t bsp_cpu_count = 1;

uint32_t fascon_plat_cpu_id(void) {
    uint64_t m;
    __asm__ __volatile__("mrs %0, mpidr_el1" : "=r"(m));
    return (uint32_t)(m & 0xffu);
}
uint32_t fascon_plat_cpu_count(void) { return bsp_cpu_count; }

void bsp_secondary_main(uint64_t core) {
    __asm__ __volatile__("msr vbar_el1, %0\n isb" : : "r"((uint64_t)(uintptr_t)bsp_vectors));
    mmu_on();
#if defined(FASCON2_SMP)
    bsp_tls_setup((uint32_t)core);
    fascon_smp_core_init((uint32_t)core);
    bsp_core_memory((uint32_t)core);
#endif
    if (core >= FASCON_BOARD_CPUS) {
        for (;;) __asm__ __volatile__("wfe");
    }
    __atomic_store_n(&bsp_cores[core].online, 1u, __ATOMIC_RELEASE);
    __asm__ __volatile__("sev");
    for (;;) {
        void (*fn)(uint64_t) = __atomic_load_n(&bsp_cores[core].fn, __ATOMIC_ACQUIRE);
        if (fn == 0) {
#if defined(FASCON2_SMP)
            fascon_smp_relax();
#else
            __asm__ __volatile__("wfe");
#endif
            continue;
        }
        fn(bsp_cores[core].arg);
        __atomic_store_n(&bsp_cores[core].fn, (void (*)(uint64_t))0, __ATOMIC_RELEASE);
        __atomic_store_n(&bsp_cores[core].busy, 0u, __ATOMIC_RELEASE);
    }
}

void fascon_plat_spawn_on(uint32_t core, void (*fn)(uint64_t), uint64_t arg) {
    uint32_t idle = 0u;
    if (core == 0u || core >= bsp_cpu_count) fascon_plat_kernel_panic(FASCON_PANIC_SMP, "spawn_on: no such core");
    if (!__atomic_compare_exchange_n(&bsp_cores[core].busy, &idle, 1u, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) fascon_plat_kernel_panic(FASCON_PANIC_SMP, "spawn_on: the core is busy");
    bsp_cores[core].arg = arg;
    __atomic_store_n(&bsp_cores[core].fn, fn, __ATOMIC_RELEASE);
    __asm__ __volatile__("dsb ish\n sev" ::: "memory");
}

static void bsp_smp_release(void) {
#if defined(FASCON2_SMP) && FASCON_BOARD_CPUS > 1
#if defined(FASCON_BOARD_SMP_SPIN_TABLE)
    /* スピンテーブル: 各コアの解放番地へ入口を書き、キャッシュから掃き出して SEV / spin table: write the entry to each
     * core's release address, clean it out of the cache and SEV */
    static const uint64_t rel[] = {FASCON_BOARD_RELEASE_ADDRS};
    for (uint32_t c = 1; c < FASCON_BOARD_CPUS && c - 1u < sizeof rel / sizeof rel[0]; c++) {
        *(volatile uint64_t *)(uintptr_t)rel[c - 1u] = (uint64_t)(uintptr_t)_start;
        __asm__ __volatile__("dc civac, %0\n dsb sy" : : "r"(rel[c - 1u]) : "memory");
    }
    __asm__ __volatile__("sev");
#elif defined(FASCON_BOARD_SMP_PSCI_HVC) || defined(FASCON_BOARD_SMP_PSCI_SMC)
    for (uint64_t c = 1; c < FASCON_BOARD_CPUS; c++) (void)bsp_psci(0xC4000003u, c, (uint64_t)(uintptr_t)_start, 0); /* CPU_ON */
#endif
    /* 起きたコアを数える（戻ってこないコアは数えない） / count the cores that came up (ones that never answer are not counted) */
    for (uint64_t spin = 0; spin < 50000000ull; spin++) {
        uint32_t n = 1;
        for (uint32_t c = 1; c < FASCON_BOARD_CPUS; c++) n += __atomic_load_n(&bsp_cores[c].online, __ATOMIC_ACQUIRE);
        bsp_cpu_count = n;
        if (n == FASCON_BOARD_CPUS) break;
    }
#endif
}

void bsp_main(void) {
    __asm__ __volatile__("msr vbar_el1, %0\n isb" : : "r"((uint64_t)(uintptr_t)bsp_vectors));
    mmu_setup();
    mmu_on();
#if defined(FASCON2_SMP)
    bsp_tls_setup(0);
    fascon_smp_core_init(0);
#endif
    bsp_core_memory(0);
    uint64_t el;
    __asm__ __volatile__("mrs %0, CurrentEL" : "=r"(el));
    puts_raw("[board] Fascon 1.0 on " FASCON_BOARD_NAME ": image ");
    put_hex((uint64_t)(uintptr_t)__kernel_start);
    puts_raw("..");
    put_hex((uint64_t)(uintptr_t)__kernel_end);
    puts_raw(", EL");
    put_dec((el >> 2) & 3u);
    puts_raw(", timer ");
    put_dec(fascon_plat_timer_freq());
    puts_raw(" Hz");
#if defined(FASCON2_DMA_NONCOHERENT)
    puts_raw(", DMA non-coherent (cache line ");
    put_dec(FASCON2_CACHE_LINE);
    puts_raw(" B)");
#endif
    puts_raw("\n");
    bsp_smp_release();
#if defined(FASCON2_SMP)
    puts_raw("[board] SMP: ");
    put_dec(bsp_cpu_count);
    puts_raw(" core(s) online (" FASCON_BOARD_SMP_NAME ")\n");
#endif
    fascon2_main();
    fascon_plat_exit(0);
}
