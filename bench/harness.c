/*
 * harness.c — 走査ベンチマークの計測ハーネス（PC 用） / timing harness for the traversal benchmark (PC)
 *
 *   fascon build bench/sched_bench.fsc --out-dir bench/build
 *   cc -O2 -std=c11 -Ibench/build bench/harness.c -o bench/build/harness
 *   bench/build/harness
 *
 * Fascon の生成 C（sched_bench.fascon.c）を同じ翻訳単位に取り込み、生成された関数をそのまま呼ぶ。
 * 比較相手は同じ Task を malloc で確保し、生ポインタでつないだ普通の C。
 * The generated C (sched_bench.fascon.c) is included into this translation unit and its functions are called as is.
 * The baseline is ordinary C: the same Task allocated with malloc and linked with raw pointers.
 *
 * 正直な注意 / honest caveats:
 *  - 1 台の機械・1 回の計測の結果で、CPU・キャッシュ・メモリ・OS が変われば数字も変わる
 *    results from one machine; numbers change with the CPU, caches, memory and OS
 *  - 速さの差は「添字だから」ではなく、密度（32 B の詰まったスロット vs 48 B 以上の malloc ブロック）と
 *    配置（表の順走査ができる）から来る。ポインタ版を連続確保・作成順でつなげば差は小さい
 *    the difference comes from density (packed 32-byte slots vs 48+ byte malloc blocks) and layout (the table can be
 *    scanned in order), not from "indices" as such; a pointer list allocated contiguously and linked in creation
 *    order is close
 *  - 1 歩ごとの世代検査はコストがゼロではない（比較 1 回と gen[] の読み出し）
 *    the per-step generation check is not free (one compare and a load from gen[])
 */
#define _POSIX_C_SOURCE 200809L
#define _DEFAULT_SOURCE 1
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "sched_bench.fascon.c"
#if defined(__linux__)
#include <sys/mman.h>
#endif

/* 1.0（2026-11 版）のランタイムなら、大きなアリーナに 2 MiB ページを頼む（PC 上の fascon run と同じ）
 * with the 1.0 (Nov 2026) runtime, ask for 2 MiB pages for large arenas (the same as fascon run on a PC) */
static const char *hugepage_hint(void) {
#if defined(F20_HUGE_MIN) && defined(__linux__) && defined(MADV_HUGEPAGE)
    if (getenv("FASCON_NO_HUGEPAGE")) return "off (FASCON_NO_HUGEPAGE)";
    for (uint32_t i = 0; i < fascon2_arena_region_count; i++) {
        const FasconArenaRegion *r = &fascon2_arena_regions[i];
        if (r->size < F20_HUGE_MIN) continue;
        uintptr_t b = (uintptr_t)r->base & ~(uintptr_t)4095u, e = ((uintptr_t)r->base + r->size + 4095u) & ~(uintptr_t)4095u;
        if (madvise((void *)b, (size_t)(e - b), MADV_HUGEPAGE) != 0) return "madvise failed";
    }
    return "MADV_HUGEPAGE";
#else
    return "none";
#endif
}

/* 完全列化（True SoA）の表では vruntime はスロットになく、列 ts_vruntime が唯一の正本（コンパイラが ARENA_task_TS_vruntime を
 * 定義する） / in a True-SoA table vruntime is not in the slot; the column ts_vruntime is its only copy (the compiler
 * defines ARENA_task_TS_vruntime) */
#if defined(ARENA_task_TS_vruntime)
#define REF_VRUNTIME(i) (arena_task.col.ts_vruntime[(i)])
#else
#define REF_VRUNTIME(i) (arena_task.slot[(i)].v.vruntime)
#endif

/* 素朴な参照（生存印を見る） / plain references (read the live marks) */
static uint64_t ref_nr_running(void) {
    uint64_t n = 0;
    for (uint32_t i = 0; i < arena_task.st.next_unused; i++)
        if (arena_task.link[i] == F20_LIVE && arena_task.slot[i].v.state == 1u) n++;
    return n;
}
static uint64_t ref_load_running(void) {
    uint64_t s = 0;
    for (uint32_t i = 0; i < arena_task.st.next_unused; i++)
        if (arena_task.link[i] == F20_LIVE && arena_task.slot[i].v.state == 1u) s += REF_VRUNTIME(i);
    return s;
}
static fsc_Id_task ref_pick_next(void) {
    uint64_t best = UINT64_MAX;
    fsc_Id_task id = 0u;
    for (uint32_t i = 0; i < arena_task.st.next_unused; i++)
        if (arena_task.link[i] == F20_LIVE && arena_task.slot[i].v.state == 1u && REF_VRUNTIME(i) < best) {
            best = REF_VRUNTIME(i);
            id = F20_ID_MAKE(arena_task.gen[i], i, ARENA_task_K);
        }
    return id;
}

/* ───── プラットフォーム層（PC） / platform layer (PC) ───── */
void fascon_plat_print(const char *s, size_t len) { fwrite(s, 1, len, stdout); }
FASCON_NORETURN void fascon_plat_kernel_panic(uint32_t code, const char *msg) {
    fprintf(stderr, "FASCON PANIC 0x%04x %s\n", (unsigned)code, msg ? msg : "");
    exit(70);
}
FASCON_NORETURN void fascon_plat_exit(int code) { exit(code); }
FASCON_NORETURN void fascon_plat_halt(void) { exit(0); }
uint32_t fascon_plat_cpu_id(void) { return 0; }

/* ───── 比較相手: 生ポインタの C / the baseline: raw-pointer C ───── */
typedef struct CTask {
    uint32_t pid, prio;
    uint64_t vruntime;
    struct CTask *parent, *next;
    uint32_t files, state;
} CTask;

#define NT 60000u

static CTask *c_nodes[NT];
static void *c_noise[NT];

static uint32_t rng = 12345u;
static uint32_t rnd(void) {
    rng = rng * 1103515245u + 12345u;
    return rng >> 8;
}

/* fragmented: 1 個確保するごとに 16〜272 B の別の確保をはさむ（長く動いたカーネルのヒープの代わり）
 * fragmented: every allocation is followed by an unrelated 16..272-byte allocation (standing in for a long-running heap) */
static CTask *c_build(uint32_t stride, int fragmented) {
    for (uint32_t k = 0; k < NT; k++) {
        CTask *t = (CTask *)malloc(sizeof(CTask));
        if (!t) abort();
        t->pid = k;
        t->prio = k % 40u;
        t->vruntime = (uint64_t)k * 3u;
        t->parent = NULL;
        t->next = NULL;
        t->files = 0;
        t->state = 0;
        c_nodes[k] = t;
        c_noise[k] = fragmented ? malloc(16u + (rnd() % 257u)) : NULL;
    }
    CTask *head = NULL;
    for (uint32_t k = 0; k < NT; k++) {
        uint32_t pos = (uint32_t)(((uint64_t)(NT - 1u - k) * stride) % NT);
        c_nodes[pos]->next = head;
        head = c_nodes[pos];
    }
    return head;
}

static void c_free(void) {
    for (uint32_t k = 0; k < NT; k++) {
        free(c_nodes[k]);
        free(c_noise[k]);
    }
}

/* 同じ意味（あふれたら止まる加算）にそろえる / same semantics (an overflowing add stops the program) */
static uint64_t c_add(uint64_t a, uint64_t b) {
    uint64_t r;
    if (__builtin_add_overflow(a, b, &r)) abort();
    return r;
}

static uint64_t c_walk(const CTask *t) {
    uint64_t s = 0;
    for (; t != NULL; t = t->next) s = c_add(c_add(s, t->vruntime), (uint64_t)t->prio);
    return s;
}

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

#define REPS 15
#define INNER 20
static volatile uint64_t sink;

/* 1 要素あたりの ns の中央値 / median ns per element */
#define MEASURE(result, expr)                                    \
    do {                                                         \
        double v_[REPS];                                         \
        for (int r_ = 0; r_ < REPS; r_++) {                      \
            double t0_ = now_ns();                               \
            for (int i_ = 0; i_ < INNER; i_++) sink += (expr);   \
            v_[r_] = (now_ns() - t0_) / ((double)INNER * NT);    \
        }                                                        \
        qsort(v_, REPS, sizeof(double), cmp_d);                  \
        (result) = v_[REPS / 2];                                 \
    } while (0)

int main(void) {
    const char *hp = hugepage_hint();
    printf("Fascon 1.0 traversal benchmark: %u tasks (Fascon slot %zu B, C node %zu B + malloc header), arena pages: %s\n", NT, sizeof(f20_slot_task_t), sizeof(CTask), hp);
    printf("%-52s %10s %10s\n", "scenario", "ns/task", "checksum");

    double r;
    uint64_t sum_ref = 0;

    /* 1. 作成順につないだリスト / list linked in creation order */
    fsc_Id_task h = fsc_build(1u);
    CTask *c = c_build(1u, 0);
    sum_ref = c_walk(c);
    MEASURE(r, c_walk(c));
    printf("%-52s %10.2f %10llu\n", "C pointers, packed malloc, creation order", r, (unsigned long long)c_walk(c));
    MEASURE(r, fsc_walk_chain(h));
    printf("%-52s %10.2f %10llu\n", "Fascon Id chain (gen check per step), creation order", r, (unsigned long long)fsc_walk_chain(h));
    MEASURE(r, fsc_walk_chain_idx(h));
    printf("%-52s %10.2f %10llu\n", "Fascon Id chain via @task[id] (latched), creation", r, (unsigned long long)fsc_walk_chain_idx(h));
    MEASURE(r, fsc_walk_scan());
    printf("%-52s %10.2f %10llu\n", "Fascon `for id in @task` scan (checks elided)", r, (unsigned long long)fsc_walk_scan());
    if (fsc_walk_chain(h) != sum_ref || fsc_walk_chain_idx(h) != sum_ref || fsc_walk_scan() != sum_ref) {
        printf("checksum mismatch\n");
        return 1;
    }
    /* 条件付きの走査（1.0.0 ではスロットごとに link[] の分岐と条件の分岐があった形） / filtered scans (per-slot branches on
     * link[] and on the filter in 1.0.0) */
    MEASURE(r, fsc_nr_running());
    printf("%-52s %10.2f %10llu\n", "Fascon count runnable (filtered count)", r, (unsigned long long)fsc_nr_running());
    MEASURE(r, fsc_load_running());
    printf("%-52s %10.2f %10llu\n", "Fascon load of runnable (filtered sum)", r, (unsigned long long)fsc_load_running());
    MEASURE(r, (uint64_t)fsc_pick_next());
    printf("%-52s %10.2f %10llu\n", "Fascon pick_next (min vruntime among runnable)", r, (unsigned long long)fsc_pick_next());
    MEASURE(r, (fsc_tick_running(), (uint64_t)0));
    printf("%-52s %10.2f %10s\n", "Fascon tick runnable (general loop, writes)", r, "-");
    MEASURE(r, (fsc_charge_running(), (uint64_t)0));
    printf("%-52s %10.2f %10s\n", "Fascon charge runnable (writes a column field)", r, "-");
    if (fsc_nr_running() != ref_nr_running() || fsc_load_running() != ref_load_running() || fsc_pick_next() != ref_pick_next()) {
        printf("checksum mismatch (filtered scans)\n");
        return 1;
    }
    /* まばらな表: 64 スロットの塊の 9 割を解放 / a sparse table: 90 % of the 64-slot blocks freed */
    fsc_thin();
    MEASURE(r, fsc_walk_scan());
    printf("%-52s %10.2f %10llu\n", "Fascon scan, sparse table (10 % of blocks live)", r, (unsigned long long)fsc_walk_scan());
    MEASURE(r, fsc_nr_running());
    printf("%-52s %10.2f %10llu\n", "Fascon count runnable, sparse table", r, (unsigned long long)fsc_nr_running());
    MEASURE(r, (uint64_t)fsc_pick_next());
    printf("%-52s %10.2f %10llu\n", "Fascon pick_next, sparse table", r, (unsigned long long)fsc_pick_next());
    if (fsc_nr_running() != ref_nr_running() || fsc_pick_next() != ref_pick_next()) {
        printf("checksum mismatch (sparse)\n");
        return 1;
    }
    fsc_clear();
    c_free();

    /* 2. ばらばらの順につないだリスト / list linked in a scattered order */
    h = fsc_build(7919u);
    c = c_build(7919u, 0);
    MEASURE(r, c_walk(c));
    printf("%-52s %10.2f %10llu\n", "C pointers, packed malloc, scattered order", r, (unsigned long long)c_walk(c));
    c_free();
    c = c_build(7919u, 1);
    MEASURE(r, c_walk(c));
    printf("%-52s %10.2f %10llu\n", "C pointers, fragmented malloc, scattered order", r, (unsigned long long)c_walk(c));
    MEASURE(r, fsc_walk_chain(h));
    printf("%-52s %10.2f %10llu\n", "Fascon Id chain (gen check per step), scattered", r, (unsigned long long)fsc_walk_chain(h));
    MEASURE(r, fsc_walk_chain_idx(h));
    printf("%-52s %10.2f %10llu\n", "Fascon Id chain via @task[id] (latched), scattered", r, (unsigned long long)fsc_walk_chain_idx(h));
    MEASURE(r, fsc_walk_scan());
    printf("%-52s %10.2f %10llu\n", "Fascon `for id in @task` scan (checks elided)", r, (unsigned long long)fsc_walk_scan());
    if (fsc_walk_chain(h) != sum_ref || fsc_walk_chain_idx(h) != sum_ref || c_walk(c) != sum_ref) {
        printf("checksum mismatch\n");
        return 1;
    }
    fsc_clear();
    c_free();
    return 0;
}
