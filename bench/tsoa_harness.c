/*
 * tsoa_harness.c — 完全列化（True SoA）の計測ハーネス: Fascon（改修前の写しの列 / 改修後の完全列化）と、手書きの C（AoS:
 * 同じ 32 バイトの構造体の配列）で同じ読み出しと書き込みを測る
 * timing harness for True SoA: the same reads and writes in Fascon (before: mirrored columns / after: True SoA) and in
 * hand-written C (AoS: an array of the same 32-byte struct)
 *
 *   bench/tsoa_bench.sh が作る / built by bench/tsoa_bench.sh:
 *   cc -O2 -std=c11 [-march=native] -DNT=<タスク数 / tasks> -I<生成 C のフォルダ / generated C dir> bench/tsoa_harness.c
 *
 * 正直な注意 / honest caveats:
 *  - C の比較相手は素直な手書きのループ（あふれの検査なし・-O2 の自動ベクトル化まかせ）。Fascon は和のあふれを検査する
 *    the C baseline is plain hand-written loops (no overflow checks, auto-vectorized at -O2); Fascon checks sums for overflow
 *  - 1 台の仮想機械の結果。CPU・キャッシュ・メモリ・OS が変われば数字も変わる
 *    results from one virtual machine; numbers change with the CPU, caches, memory and OS
 *  - 測る関数は volatile な関数ポインタで呼ぶ（インライン展開と、同じ上書きの繰り返しの畳み込みを防ぐ）
 *    the timed functions are called through volatile function pointers (no inlining, no folding of repeated overwrites)
 */
#define _POSIX_C_SOURCE 200809L
#define _DEFAULT_SOURCE 1
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "tsoa_bench.fascon.c"
#if defined(__linux__)
#include <sys/mman.h>
#endif

#ifndef NT
#error "define NT (the task count)"
#endif

/* ───── プラットフォーム層（PC） / platform layer (PC) ───── */
void fascon_plat_print(const char *s, size_t len) { (void)s; (void)len; }
FASCON_NORETURN void fascon_plat_kernel_panic(uint32_t code, const char *msg) {
    fprintf(stderr, "FASCON PANIC 0x%04x %s\n", (unsigned)code, msg ? msg : "");
    exit(70);
}
FASCON_NORETURN void fascon_plat_exit(int code) { exit(code); }
FASCON_NORETURN void fascon_plat_halt(void) { exit(0); }
uint32_t fascon_plat_cpu_id(void) { return 0; }

/* ───── 手書きの C（AoS） / hand-written C (AoS) ───── */
typedef struct {
    uint32_t pid, prio;
    uint64_t vruntime;
    uint32_t parent, next, files, state;
} CTask;
_Static_assert(sizeof(CTask) == 32, "CTask is 32 bytes");
static CTask c_tasks[NT] __attribute__((aligned(2097152)));

static void c_build(void) {
    for (uint64_t k = 0; k < NT; k++) {
        c_tasks[k].pid = (uint32_t)k;
        c_tasks[k].prio = (uint32_t)(k % 40u);
        c_tasks[k].vruntime = k * 3u;
        c_tasks[k].parent = c_tasks[k].next = c_tasks[k].files = 0;
        c_tasks[k].state = (uint32_t)(((k * 2654435761u) >> 13) % 4u);
    }
}
static uint64_t c_total(void) {
    uint64_t s = 0;
    for (uint32_t i = 0; i < NT; i++) s += c_tasks[i].vruntime;
    return s;
}
static uint64_t c_load(void) {
    uint64_t s = 0;
    for (uint32_t i = 0; i < NT; i++)
        if (c_tasks[i].state == 1u) s += c_tasks[i].vruntime;
    return s;
}
static uint64_t c_count(void) {
    uint64_t n = 0;
    for (uint32_t i = 0; i < NT; i++) n += c_tasks[i].state == 1u;
    return n;
}
static uint64_t c_pick(void) {
    uint64_t best = UINT64_MAX, pick = UINT64_MAX;
    for (uint32_t i = 0; i < NT; i++)
        if (c_tasks[i].state == 1u && c_tasks[i].vruntime < best) {
            best = c_tasks[i].vruntime;
            pick = c_tasks[i].pid;
        }
    return pick;
}
static uint64_t c_charge(void) {
    for (uint32_t i = 0; i < NT; i++)
        if (c_tasks[i].state == 1u) c_tasks[i].vruntime += 1u;
    return 0;
}
static uint64_t c_age(void) {
    for (uint32_t i = 0; i < NT; i++) c_tasks[i].vruntime += 1u;
    return 0;
}
static uint64_t c_reset_x;
static uint64_t c_reset(void) {
    const uint64_t x = c_reset_x;
    for (uint32_t i = 0; i < NT; i++) c_tasks[i].vruntime = x;
    return 0;
}

/* ───── Fascon の関数を同じ形で / the Fascon functions in the same shape ───── */
static uint64_t f_total(void) { return fsc_total_vruntime(); }
static uint64_t f_load(void) { return fsc_load_running(); }
static uint64_t f_count(void) { return fsc_nr_running(); }
static uint64_t f_pick(void) { return fsc_pick_next_pid(); }
static uint64_t f_charge(void) { fsc_charge_running(); return 0; }
static uint64_t f_age(void) { fsc_age_all(); return 0; }
static uint64_t f_reset(void) { fsc_reset_vruntime(c_reset_x); return 0; }

/* ───── 計測 / timing ───── */
static double now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec * 1e9 + (double)ts.tv_nsec;
}
static int cmp_d(const void *a, const void *b) {
    double x = *(const double *)a, y = *(const double *)b;
    return (x > y) - (x < y);
}
#if NT >= 1000000
#define REPS 9
#define INNER 3
#else
#define REPS 15
#define INNER 20
#endif
static volatile uint64_t sink;
typedef uint64_t (*fn_t)(void);

/* 1 タスクあたりの ns の中央値。上書きは回ごとに値を変える / median ns per task; overwrites change the value every round */
static double measure(fn_t f0) {
    volatile fn_t f = f0;
    double v[REPS];
    for (int r = 0; r < REPS; r++) {
        double t0 = now_ns();
        for (int i = 0; i < INNER; i++) {
            c_reset_x = (uint64_t)(r * INNER + i) * 7u + 1u;
            sink += f();
        }
        v[r] = (now_ns() - t0) / ((double)INNER * NT);
    }
    qsort(v, REPS, sizeof(double), cmp_d);
    return v[REPS / 2];
}

/* コアのクロック: 依存のある 64 ビット乗算の鎖（IMUL の待ち時間 3 クロック） / the core clock: a chain of dependent 64-bit
 * multiplies (IMUL latency: 3 cycles) */
static double measure_ghz(void) {
#if defined(__x86_64__)
    double best = 0.0;
    for (int r = 0; r < 5; r++) {
        uint64_t x = 3u;
        const uint64_t n = 100000000u;
        double t0 = now_ns();
        for (uint64_t i = 0; i < n; i++) __asm__ volatile("imul %0, %0" : "+r"(x));
        double g = 3.0 * (double)n / (now_ns() - t0);
        sink += x;
        if (g > best) best = g;
    }
    return best;
#else
    return 0.0;
#endif
}

static const char *pages(void) {
#if defined(F20_HUGE_MIN) && defined(__linux__) && defined(MADV_HUGEPAGE)
    if (getenv("FASCON_NO_HUGEPAGE")) return "4 KiB";
    for (uint32_t i = 0; i < fascon2_arena_region_count; i++) {
        const FasconArenaRegion *r = &fascon2_arena_regions[i];
        if (r->size < F20_HUGE_MIN) continue;
        uintptr_t b = (uintptr_t)r->base & ~(uintptr_t)4095u, e = ((uintptr_t)r->base + r->size + 4095u) & ~(uintptr_t)4095u;
        if (madvise((void *)b, (size_t)(e - b), MADV_HUGEPAGE) != 0) return "madvise failed";
    }
    if (madvise((void *)c_tasks, sizeof c_tasks, MADV_HUGEPAGE) != 0) return "madvise failed";
    return "2 MiB (MADV_HUGEPAGE)";
#else
    return "4 KiB";
#endif
}

static int check(const char *what) {
    const uint64_t a[4] = {f_total(), f_load(), f_count(), f_pick()}, b[4] = {c_total(), c_load(), c_count(), c_pick()};
    for (int i = 0; i < 4; i++)
        if (a[i] != b[i]) {
            printf("checksum mismatch after %s (%d): fascon %llu, C %llu\n", what, i, (unsigned long long)a[i], (unsigned long long)b[i]);
            return 1;
        }
    return 0;
}

int main(int argc, char **argv) {
    const char *label = argc > 1 ? argv[1] : "fascon";
    const char *hp = pages();
    if (fsc_build() != NT) {
        printf("build failed\n");
        return 1;
    }
    c_build();
    if (check("build")) return 1;
    const double ghz = measure_ghz();
    static const char *names[7] = {"read: sum of vruntime (vruntime_scan)", "read: vruntime sum of runnable tasks", "read: count of runnable tasks", "read: pick_next (min vruntime, runnable)", "write: vruntime += 1, runnable tasks", "write: vruntime += 1, every task", "write: vruntime = x, every task"};
    const fn_t fs[7] = {f_total, f_load, f_count, f_pick, f_charge, f_age, f_reset};
    const fn_t cs[7] = {c_total, c_load, c_count, c_pick, c_charge, c_age, c_reset};
    printf("# %u tasks, pages %s, core clock %.2f GHz (IMUL chain), Fascon table %zu B\n", (unsigned)NT, hp, ghz, sizeof arena_task);
    for (int k = 0; k < 7; k++) {
        const double fv = measure(fs[k]);
        const double cv = measure(cs[k]);
        printf("%s\t%s\t%.3f\t%.3f\t%.2f\t%.2f\n", label, names[k], fv, cv, fv * ghz, cv * ghz);
        if (k >= 4 && check(names[k])) return 1;
    }
    return 0;
}
