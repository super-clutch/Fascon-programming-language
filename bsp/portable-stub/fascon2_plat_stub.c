/*
 * fascon2_plat_stub.c — Fascon 1.0: 移植用の参照プラットフォーム層（標準 C11 + __atomic だけ。アセンブリなし）
 *                       a reference platform layer for porting (standard C11 + __atomic only, no assembly)
 *
 * コンパイラが生成する C は、アーキテクチャ固有の命令を 1 つも含まず、すべて fascon_plat_* の抽象 API を呼ぶ。このファイルは
 * その API を全部、どのアーキテクチャでも翻訳できる形で実装した「雛形」で、ARMv7 / MIPS64 / RISC-V64 などの新しい移植の
 * 出発点（tools/cross_check.py がこれで生成 C の翻訳とリンクを確かめる）。実機で動かすには、印の付いた所（★）を
 * そのボードの装置に置き換える: コンソール、割り込みの禁止、タイマー、タスクの文脈の切り替え、ページ表。
 * The C the compiler generates holds no architecture-specific instruction at all; everything goes through the abstract
 * fascon_plat_* API. This file implements the whole API in a form that compiles on any architecture: the starting point
 * of a new port to ARMv7, MIPS64, RISC-V64 and others (tools/cross_check.py uses it to compile and link generated C).
 * To run on hardware, replace the marked places (★) with the devices of the board: the console, interrupt masking, the timer,
 * task context switches and page tables.
 */
#include "fascon2_rt.h"

void fascon2_main(void);

/* ★ コンソール: ここでは RAM の輪のバッファ（デバッガで読める）。実機では UART へ / ★ console: a RAM ring buffer here
 * (readable from a debugger); a UART on hardware */
#ifndef FASCON_STUB_LOG
#define FASCON_STUB_LOG 4096u
#endif
static char stub_log[FASCON_STUB_LOG];
static uint32_t stub_log_pos;
void fascon_plat_print(const char *s, size_t len) {
    for (size_t i = 0; i < len; i++) {
        const uint32_t p = __atomic_fetch_add(&stub_log_pos, 1u, __ATOMIC_RELAXED);
        ((volatile char *)stub_log)[p % FASCON_STUB_LOG] = s[i];
    }
}

static volatile int stub_exit_code = -1;
FASCON_NORETURN void fascon_plat_exit(int code) {
    stub_exit_code = code;
    for (;;) FASCON_CPU_RELAX();
}
FASCON_NORETURN void fascon_plat_halt(void) {
    for (;;) FASCON_CPU_RELAX();
}
FASCON_NORETURN void fascon_plat_kernel_panic(uint32_t code, const char *msg) {
    static const char head[] = "\nFASCON PANIC ";
    fascon_plat_print(head, sizeof head - 1u);
    size_t n = 0;
    while (msg && msg[n]) n++;
    fascon_plat_print(msg, n);
    fascon_plat_print("\n", 1);
    fascon_plat_exit((int)(70u + 0u * code));
}

/* ★ コア: 1 つ。マルチコアの移植では、起こしたコアの数と、そのコアで fn(arg) を走らせる仕組みを置く
 * ★ cores: one; a multi-core port provides the count of started cores and a way to run fn(arg) on a core */
uint32_t fascon_plat_cpu_id(void) { return 0u; }
uint32_t fascon_plat_cpu_count(void) { return 1u; }
void fascon_plat_spawn_on(uint32_t core, void (*fn)(uint64_t), uint64_t arg) {
    (void)core;
    (void)fn;
    (void)arg;
    fascon_plat_kernel_panic(FASCON_PANIC_SMP, "spawn_on: this platform layer runs one core");
}
/* パニックの全コア停止: 1 コアなので何もしない（★ マルチコアでは IPI を送る） / the all-core stop on panic: nothing to do on
 * one core (★ send an IPI on multi-core) */
void fascon_plat_panic_broadcast(void) {}

/* ★ 割り込み: 1 つのフラグ（実機では割り込みマスクのレジスタを退避して禁止） / ★ interrupts: one flag (save and set the
 * interrupt mask register on hardware) */
static volatile uint64_t stub_masked = 1u;
static const FasconIrqEntry *stub_irqs;
static uint32_t stub_irq_count;
void fascon_plat_irq_register(const FasconIrqEntry *table, uint32_t count) {
    stub_irqs = table;
    stub_irq_count = count;
}
uint64_t fascon_plat_irq_save(void) {
    const uint64_t prev = stub_masked;
    stub_masked = 1u;
    __atomic_signal_fence(__ATOMIC_SEQ_CST);
    return prev;
}
void fascon_plat_irq_restore(uint64_t state) {
    __atomic_signal_fence(__ATOMIC_SEQ_CST);
    stub_masked = state;
}
void fascon_plat_irq_open(void) { stub_masked = 0u; }

/* ★ タイマー: 呼ぶたびに進む数（実機ではコアごとのカウンタと比較レジスタ） / ★ timer: a count advanced on every call (a per-core
 * counter and compare register on hardware) */
static volatile uint64_t stub_now, stub_deadline;
static volatile int stub_armed;
uint64_t fascon_plat_timer_freq(void) { return 1000000u; }
uint64_t fascon_plat_timer_now(void) { return ++stub_now; }
void fascon_plat_timer_arm(uint64_t ticks) {
    stub_deadline = stub_now + ticks;
    stub_armed = 1;
}
/* 待つ: 期限が来ていればタイマーのハンドラ（INTID 27 / 30 = コアごとのタイマー）を呼ぶ / wait: call the timer handler
 * (INTID 27 / 30 = the per-core timer) when the deadline has passed */
void fascon_plat_wait_irq(void) {
    if (!stub_armed) fascon_plat_exit(0);
    stub_now = stub_deadline;
    stub_armed = 0;
    for (uint32_t i = 0; i < stub_irq_count; i++)
        if (stub_irqs[i].intid == 27u || stub_irqs[i].intid == 30u) stub_irqs[i].handler();
}

/* ★ タスク: 文脈の切り替えはアーキテクチャの仕事（レジスタの保存）なので、この雛形は「1 本ずつ最後まで走らせる」だけ。
 * 入口から戻ったタスクは on_exit で終わり、次の生きているタスクへ進む
 * ★ tasks: a context switch is the job of the architecture (saving registers), so this template runs one task at a time to the
 * end; a task returning from its entry ends through on_exit and the next live task runs */
typedef struct {
    f20_id_t id;
    int alive;
    void (*entry)(uint64_t);
    uint64_t arg;
    void (*on_exit)(void);
} StubTask;
static StubTask stub_tasks[FASCON2_TASK_MAX];
bool fascon_plat_task_create(uint32_t slot, f20_id_t id, void (*entry)(uint64_t), uint64_t arg, void *stack, size_t stack_size, void (*on_exit)(void)) {
    if (slot >= FASCON2_TASK_MAX || stack_size < 4096u) return false;
    (void)stack;
    stub_tasks[slot] = (StubTask){id, 1, entry, arg, on_exit};
    fascon_cpu_sched(0u)->live++;
    return true;
}
bool fascon_plat_task_create_user(uint32_t slot, f20_id_t id, uint64_t entry, uint64_t sp, void *stack, size_t stack_size, void (*on_exit)(void)) {
    (void)slot; (void)id; (void)entry; (void)sp; (void)stack; (void)stack_size; (void)on_exit;
    return false; /* ★ EL0 / Ring 3 は移植の仕事 / ★ user mode is part of a port */
}
f20_id_t fascon_plat_task_current(void) {
    const f20_sched_t *S = fascon_cpu_sched(0u);
    return S->cur >= 0 ? stub_tasks[S->cur].id : 0u;
}
void fascon_plat_task_yield(uint32_t slot, f20_id_t id) {
    (void)slot;
    (void)id;
}
void fascon_plat_task_resume(uint32_t slot, f20_id_t id) {
    if (slot < FASCON2_TASK_MAX && stub_tasks[slot].alive && stub_tasks[slot].id == id) fascon_cpu_sched(0u)->pending = (int32_t)slot;
}
static FASCON_NORETURN void stub_run_from(uint32_t slot) {
    f20_sched_t *S = fascon_cpu_sched(0u);
    for (uint32_t k = 0; k < FASCON2_TASK_MAX; k++) {
        const uint32_t i = (slot + k) % FASCON2_TASK_MAX;
        if (!stub_tasks[i].alive) continue;
        S->cur = (int32_t)i;
        S->switches++;
        stub_tasks[i].entry(stub_tasks[i].arg);
        stub_tasks[i].on_exit();
    }
    fascon_plat_exit(0);
}
FASCON_NORETURN void fascon_plat_task_enter(uint32_t slot, f20_id_t id) {
    if (slot >= FASCON2_TASK_MAX || !stub_tasks[slot].alive || stub_tasks[slot].id != id) fascon_plat_kernel_panic(FASCON_PANIC_TASK, "enter: the task does not exist");
    stub_masked = 0u;
    stub_run_from(slot);
}
FASCON_NORETURN void fascon_plat_task_exit(void) {
    f20_sched_t *S = fascon_cpu_sched(0u);
    if (S->cur >= 0) {
        stub_tasks[S->cur].alive = 0;
        if (S->live) S->live--;
        stub_run_from(((uint32_t)S->cur + 1u) % FASCON2_TASK_MAX);
    }
    fascon_plat_exit(0);
}

/* ★ ユーザー空間・システムコール・トラップ: 登録だけ（EL0 / Ring 3 / U モードと SVC / int / ecall は移植の仕事）
 * ★ user space, system calls and traps: registration only (EL0 / ring 3 / U-mode and SVC / int / ecall are part of a port) */
void fascon_plat_user_register(uint8_t *mem, uint64_t size, uint64_t va, uint32_t slots) {
    (void)mem; (void)size; (void)va; (void)slots;
}
void fascon_plat_syscall_register(const FasconSyscallFn *table, uint32_t count) {
    (void)table;
    (void)count;
}
void fascon_plat_trap_register(void (*handler)(uint64_t, uint64_t, uint64_t)) { (void)handler; }

/* ★ 4 KiB のページとアドレス空間: 表を持たない雛形（写せない = false）。実機ではページ表と ASID / PCID / satp.ASID
 * ★ 4 KiB pages and address spaces: a template without tables (nothing maps = false); page tables and ASID / PCID /
 * satp.ASID on hardware */
bool fascon_plat_vm_space(uint32_t asid) { return asid != 0u && false; }
bool fascon_plat_vm_map(uint32_t asid, uint64_t va, uint64_t pa, uint32_t flags) {
    (void)asid; (void)va; (void)pa; (void)flags;
    return false;
}
uint64_t fascon_plat_vm_unmap(uint32_t asid, uint64_t va) {
    (void)asid;
    (void)va;
    return 0u;
}
void fascon_plat_vm_switch(uint32_t asid) { (void)asid; }
void fascon_plat_vm_flush(uint32_t asid, uint64_t va, uint64_t cpu_mask) {
    (void)asid; (void)va; (void)cpu_mask;
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
}
void fascon_plat_vm_drop(uint32_t asid) { (void)asid; }

/* 起動: ★ 実機ではスタックを置いて .bss を 0 にしてからここへ / start-up: ★ on hardware set the stack and zero .bss first */
void fascon_stub_start(void) {
#if defined(FASCON2_SMP)
    fascon_smp_core_init(0u);
#endif
    static uint8_t heap[256u << 10] __attribute__((aligned(16)));
    static uint8_t frame[64u << 10] __attribute__((aligned(16)));
    fascon_heap_init(heap, sizeof heap);
    fascon_frame_init(frame, sizeof frame);
    fascon2_main();
    fascon_plat_exit(0);
}

/* ───── コンパイラの補助関数（libgcc / compiler-rt を持たない環境のための最小限）/ compiler helper routines (a minimal set
 * for environments without libgcc / compiler-rt) ───── */
#if defined(__arm__)
/* ARM EABI のメモリの補助関数（Clang が構造体の 0 埋め・写しに使う）。memset は (先, 値, 長さ)、__aeabi_memset は
 * (先, 長さ, 値) の順なので注意 / ARM EABI memory helpers (Clang uses them to zero and copy structs). Note that
 * __aeabi_memset takes (dest, n, value), unlike memset, which takes (dest, value, n) */
void *memcpy(void *d, const void *s, size_t n);
void *memmove(void *d, const void *s, size_t n);
void *memset(void *d, int c, size_t n);
void __aeabi_memcpy(void *d, const void *s, size_t n);
void __aeabi_memcpy4(void *d, const void *s, size_t n);
void __aeabi_memcpy8(void *d, const void *s, size_t n);
void __aeabi_memmove(void *d, const void *s, size_t n);
void __aeabi_memmove4(void *d, const void *s, size_t n);
void __aeabi_memmove8(void *d, const void *s, size_t n);
void __aeabi_memset(void *d, size_t n, int c);
void __aeabi_memset4(void *d, size_t n, int c);
void __aeabi_memset8(void *d, size_t n, int c);
void __aeabi_memclr(void *d, size_t n);
void __aeabi_memclr4(void *d, size_t n);
void __aeabi_memclr8(void *d, size_t n);
void __aeabi_memcpy(void *d, const void *s, size_t n) { (void)memcpy(d, s, n); }
void __aeabi_memcpy4(void *d, const void *s, size_t n) { (void)memcpy(d, s, n); }
void __aeabi_memcpy8(void *d, const void *s, size_t n) { (void)memcpy(d, s, n); }
void __aeabi_memmove(void *d, const void *s, size_t n) { (void)memmove(d, s, n); }
void __aeabi_memmove4(void *d, const void *s, size_t n) { (void)memmove(d, s, n); }
void __aeabi_memmove8(void *d, const void *s, size_t n) { (void)memmove(d, s, n); }
void __aeabi_memset(void *d, size_t n, int c) { (void)memset(d, c, n); }
void __aeabi_memset4(void *d, size_t n, int c) { (void)memset(d, c, n); }
void __aeabi_memset8(void *d, size_t n, int c) { (void)memset(d, c, n); }
void __aeabi_memclr(void *d, size_t n) { (void)memset(d, 0, n); }
void __aeabi_memclr4(void *d, size_t n) { (void)memset(d, 0, n); }
void __aeabi_memclr8(void *d, size_t n) { (void)memset(d, 0, n); }

/* 64 ビットの割り算（ARM EABI: 商は r0:r1、余りは r2:r3 で返る。stub_divmod_arm.S が包む）/ 64-bit division (ARM EABI
 * returns the quotient in r0:r1 and the remainder in r2:r3; wrapped by stub_divmod_arm.S) */
uint64_t __udivmoddi4(uint64_t a, uint64_t b, uint64_t *rem);
uint64_t __udivmoddi4(uint64_t a, uint64_t b, uint64_t *rem) {
    if (b == 0u) fascon_plat_kernel_panic(FASCON_PANIC_ARITH, "division by zero");
    uint64_t q = 0u, r = 0u;
    for (int i = 63; i >= 0; i--) {
        r = (r << 1) | ((a >> i) & 1u);
        if (r >= b) {
            r -= b;
            q |= (uint64_t)1u << i;
        }
    }
    if (rem) *rem = r;
    return q;
}
int64_t __divmoddi4(int64_t a, int64_t b, int64_t *rem);
int64_t __divmoddi4(int64_t a, int64_t b, int64_t *rem) {
    const int neg_q = (a < 0) != (b < 0), neg_r = a < 0;
    uint64_t r;
    uint64_t q = __udivmoddi4(a < 0 ? (uint64_t)0 - (uint64_t)a : (uint64_t)a, b < 0 ? (uint64_t)0 - (uint64_t)b : (uint64_t)b, &r);
    if (rem) *rem = neg_r ? -(int64_t)r : (int64_t)r;
    return neg_q ? -(int64_t)q : (int64_t)q;
}
#endif
