/* bsp.c — QEMU virt RISC-V 64 のボードのパッケージ（M モード・MMU なし） / the QEMU virt RISC-V 64 board package (M-mode, no MMU)
 *   コンソール ns16550・タイマー CLINT の mtime・IPI で 2 番目以降のハートを起こす・終わりは SiFive test（QEMU）か止まる
 *   console ns16550, timer CLINT mtime, secondaries woken by IPI, ends through SiFive test (QEMU) or stops */
#include "fascon2_rt.h"
#include "fascon_board.h"

#define CLINT 0x02000000ul
#define MMIO32(a) (*(volatile uint32_t *)(uintptr_t)(a))
#define MMIO8(a) (*(volatile uint8_t *)(uintptr_t)(a))
extern char __heap_start[], __heap_end[], __frame_start[], __frame_end[], __kernel_start[], __kernel_end[];
extern char __percpu_start[], __percpu_end[], __tls_size[], __tls_align[];
void fascon2_main(void);

static volatile uint32_t bsp_console_lock;
void fascon_plat_print(const char *s, size_t len) {
    while (__atomic_exchange_n(&bsp_console_lock, 1u, __ATOMIC_ACQUIRE)) {
    }
    for (size_t i = 0; i < len; i++) {
        while (!(MMIO8(FASCON_BOARD_UART_BASE + (5u << FASCON_BOARD_UART_SHIFT)) & 0x20u)) {
        }
        MMIO8(FASCON_BOARD_UART_BASE) = (uint8_t)s[i];
    }
    __atomic_store_n(&bsp_console_lock, 0u, __ATOMIC_RELEASE);
}
static void puts_raw(const char *s) {
    size_t n = 0;
    while (s[n]) n++;
    fascon_plat_print(s, n);
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
FASCON_NORETURN void fascon_plat_exit(int code) {
    puts_raw("[board] exit code ");
    put_dec((uint64_t)(int64_t)code);
    puts_raw("\n");
#if defined(FASCON_BOARD_EXIT_SEMIHOSTING)
    MMIO32(0x100000u) = code == 0 ? 0x5555u : (((uint32_t)code << 16) | 0x3333u); /* QEMU virt の SiFive test / SiFive test */
#endif
    for (;;) __asm__ __volatile__("wfi");
}
FASCON_NORETURN void fascon_plat_halt(void) { fascon_plat_exit(0); }
FASCON_NORETURN void fascon_plat_kernel_panic(uint32_t code, const char *msg) {
    puts_raw("\nFASCON PANIC ");
    put_dec(code);
    puts_raw(" ");
    puts_raw(msg);
    puts_raw("\n");
    fascon_plat_exit(70);
}
void bsp_exception(uint64_t cause, uint64_t epc, uint64_t tval) {
    puts_raw("\nTRAP mcause=");
    put_dec(cause);
    puts_raw(" mepc=");
    put_dec(epc);
    puts_raw(" mtval=");
    put_dec(tval);
    puts_raw("\n");
    fascon_plat_exit(71);
}
uint64_t fascon_plat_irq_save(void) {
    uint64_t m;
    __asm__ __volatile__("csrrci %0, mstatus, 8" : "=r"(m) : : "memory");
    return m & 8u;
}
void fascon_plat_irq_restore(uint64_t state) {
    if (state & 8u) __asm__ __volatile__("csrsi mstatus, 8" ::: "memory");
}
void fascon_plat_irq_open(void) {}
void fascon_plat_wait_irq(void) { __asm__ __volatile__("wfi"); }
uint64_t fascon_plat_timer_freq(void) { return FASCON_BOARD_TIMER_HZ; }
uint64_t fascon_plat_timer_now(void) { return *(volatile uint64_t *)(uintptr_t)(CLINT + 0xBFF8u); }
static FASCON_NORETURN void bsp_unsupported(const char *what) {
    puts_raw("[board] ");
    puts_raw(what);
    puts_raw(": not in this board package\n");
    fascon_plat_kernel_panic(0x13u, "unsupported on this board package");
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
    bsp_unsupported("traps");
}
void fascon_plat_user_register(uint8_t *mem, uint64_t size, uint64_t va, uint32_t slots) {
    (void)mem, (void)size, (void)va, (void)slots;
    bsp_unsupported("user memory");
}
bool fascon_plat_user_window(uint64_t addr, uint64_t len, f20_slice_t *out) {
    (void)addr, (void)len, (void)out;
    return false;
}
bool fascon_plat_task_create(uint32_t slot, f20_id_t id, void (*entry)(uint64_t), uint64_t arg, void *stack, size_t stack_size, void (*on_exit)(void)) {
    (void)slot, (void)id, (void)entry, (void)arg, (void)stack, (void)stack_size, (void)on_exit;
    bsp_unsupported("tasks");
}
bool fascon_plat_task_create_user(uint32_t slot, f20_id_t id, uint64_t entry, uint64_t sp, void *stack, size_t stack_size, void (*on_exit)(void)) {
    (void)slot, (void)id, (void)entry, (void)sp, (void)stack, (void)stack_size, (void)on_exit;
    bsp_unsupported("user tasks");
}
f20_id_t fascon_plat_task_current(void) { return 0u; }
void fascon_plat_task_yield(uint32_t slot, f20_id_t id) { (void)slot, (void)id; }
void fascon_plat_task_resume(uint32_t slot, f20_id_t id) { (void)slot, (void)id; }
FASCON_NORETURN void fascon_plat_task_enter(uint32_t slot, f20_id_t id) {
    (void)slot, (void)id;
    bsp_unsupported("tasks");
}
FASCON_NORETURN void fascon_plat_task_exit(void) { fascon_plat_exit(0); }

static void bsp_tls_setup(uint32_t hart) {
    const uint64_t size = (uint64_t)(uintptr_t)__tls_size;
    const uint64_t stride = (uint64_t)(__percpu_end - __percpu_start) / FASCON_BOARD_CPUS;
    uint8_t *block = (uint8_t *)__percpu_start + hart * stride;
    for (uint64_t i = 0; i < size; i++) block[i] = 0;
    __asm__ __volatile__("mv tp, %0" : : "r"(block)); /* RISC-V: TLS は tp から（TCB 無し）/ TLS starts at tp (no TCB) */
}
static void bsp_core_memory(uint32_t hart) {
    const size_t heap = (size_t)(__heap_end - __heap_start) / FASCON_BOARD_CPUS;
    const size_t frame = (size_t)(__frame_end - __frame_start) / FASCON_BOARD_CPUS;
    fascon_heap_init(__heap_start + hart * heap, heap);
    fascon_frame_init(__frame_start + hart * frame, frame);
}
static struct {
    void (*fn)(uint64_t);
    uint64_t arg;
    uint32_t busy, online;
    uint8_t pad_[64 - 24];
} bsp_cores[FASCON_BOARD_CPUS] __attribute__((aligned(64)));
static uint32_t bsp_cpu_count = 1;
uint32_t fascon_plat_cpu_id(void) {
    uint64_t h;
    __asm__ __volatile__("csrr %0, mhartid" : "=r"(h));
    return (uint32_t)h;
}
uint32_t fascon_plat_cpu_count(void) { return bsp_cpu_count; }
void bsp_secondary_main(uint64_t hart) {
    MMIO32(CLINT + 4u * hart) = 0u; /* IPI を受け取った / acknowledge the IPI */
#if defined(FASCON2_SMP)
    bsp_tls_setup((uint32_t)hart);
    fascon_smp_core_init((uint32_t)hart);
    bsp_core_memory((uint32_t)hart);
#endif
    if (hart >= FASCON_BOARD_CPUS) {
        for (;;) __asm__ __volatile__("wfi");
    }
    __atomic_store_n(&bsp_cores[hart].online, 1u, __ATOMIC_RELEASE);
    for (;;) {
        void (*fn)(uint64_t) = __atomic_load_n(&bsp_cores[hart].fn, __ATOMIC_ACQUIRE);
        if (fn == 0) {
#if defined(FASCON2_SMP)
            fascon_smp_relax();
#endif
            continue;
        }
        fn(bsp_cores[hart].arg);
        __atomic_store_n(&bsp_cores[hart].fn, (void (*)(uint64_t))0, __ATOMIC_RELEASE);
        __atomic_store_n(&bsp_cores[hart].busy, 0u, __ATOMIC_RELEASE);
    }
}
void fascon_plat_spawn_on(uint32_t core, void (*fn)(uint64_t), uint64_t arg) {
    uint32_t idle = 0u;
    if (core == 0u || core >= bsp_cpu_count) fascon_plat_kernel_panic(FASCON_PANIC_SMP, "spawn_on: no such hart");
    if (!__atomic_compare_exchange_n(&bsp_cores[core].busy, &idle, 1u, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) fascon_plat_kernel_panic(FASCON_PANIC_SMP, "spawn_on: the hart is busy");
    bsp_cores[core].arg = arg;
    __atomic_store_n(&bsp_cores[core].fn, fn, __ATOMIC_RELEASE);
}
void bsp_main(void) {
#if defined(FASCON2_SMP)
    bsp_tls_setup(0);
    fascon_smp_core_init(0);
#endif
    bsp_core_memory(0);
    puts_raw("[board] Fascon 1.0 on " FASCON_BOARD_NAME " (RISC-V M-mode)\n");
#if defined(FASCON2_SMP) && FASCON_BOARD_CPUS > 1
    /* IPI による起動: CLINT の msip に 1 を書く / wake-up by IPI: write 1 to the CLINT msip */
    for (uint32_t h = 1; h < FASCON_BOARD_CPUS; h++) MMIO32(CLINT + 4u * h) = 1u;
    for (uint64_t spin = 0; spin < 50000000ull; spin++) {
        uint32_t n = 1;
        for (uint32_t c = 1; c < FASCON_BOARD_CPUS; c++) n += __atomic_load_n(&bsp_cores[c].online, __ATOMIC_ACQUIRE);
        bsp_cpu_count = n;
        if (n == FASCON_BOARD_CPUS) break;
    }
    puts_raw("[board] SMP: ");
    put_dec(bsp_cpu_count);
    puts_raw(" hart(s) online (ipi)\n");
#endif
    fascon2_main();
    fascon_plat_exit(0);
}
