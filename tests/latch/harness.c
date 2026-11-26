/*
 * harness.c — 遅延フォールト（ラッチ）と 32 ビット Id のテスト / tests for deferred faults (the latch) and 32-bit Ids
 *
 *   fascon build tests/latch/latch.fsc --out-dir build/latch
 *   cc -O2 -std=c11 -Ibuild/latch tests/latch/harness.c -o build/latch/harness && build/latch/harness
 *
 * 生成 C を同じ翻訳単位に取り込み、停止（パニック）を longjmp で受け止めて、止まったあとのメモリを調べる。
 * The generated C is included into this translation unit; stops (panics) are caught with longjmp so the memory can be
 * inspected after the stop.
 */
#define _POSIX_C_SOURCE 200809L
#include <setjmp.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "latch.fascon.c"

static jmp_buf jb;
static volatile uint32_t last_code;
static char out[4096];
static size_t out_len;
static int failures;
static int checks;

void fascon_plat_print(const char *s, size_t len) {
    if (out_len + len < sizeof out) {
        memcpy(out + out_len, s, len);
        out_len += len;
    }
}
FASCON_NORETURN void fascon_plat_kernel_panic(uint32_t code, const char *msg) {
    (void)msg;
    last_code = code;
    longjmp(jb, 1);
}
FASCON_NORETURN void fascon_plat_exit(int code) { exit(code); }
FASCON_NORETURN void fascon_plat_halt(void) { exit(0); }
uint32_t fascon_plat_cpu_id(void) { return 0; }

#define CHECK(c)                                                         \
    do {                                                                 \
        checks++;                                                        \
        if (!(c)) {                                                      \
            failures++;                                                  \
            printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #c);          \
        }                                                                \
    } while (0)

/* 呼び出しがコード code で止まること / the call must stop with the given code */
#define EXPECT_STOP(call, code)                                          \
    do {                                                                 \
        last_code = 0;                                                   \
        if (setjmp(jb) == 0) {                                           \
            call;                                                        \
            CHECK(!"did not stop: " #call);                              \
        } else {                                                         \
            CHECK(last_code == (code));                                  \
        }                                                                \
    } while (0)

/* 調べる側の素朴な合計（生存印を見る） / the checker's plain sum (reads the live marks) */
static uint64_t ref_total(int with_prio) {
    uint64_t s = 0;
    for (uint32_t i = 0; i < arena_task.st.next_unused; i++)
        if (arena_task.link[i] == F20_LIVE) s += arena_task.slot[i].v.vruntime + (with_prio ? arena_task.slot[i].v.prio : 0u);
    return s;
}

static uint32_t rng = 2463534242u;
static uint32_t rnd(void) {
    rng ^= rng << 13;
    rng ^= rng >> 17;
    rng ^= rng << 5;
    return rng;
}

int main(void) {
    const uint32_t K = ARENA_task_K;
    CHECK(K == 3u); /* 容量 8 → 添字 3 ビット / capacity 8 → 3 index bits */
    CHECK(F20_GEN_MAX(K) == 0x1FFFFFFFu);
#if defined(ARENA_task_TRUE_SOA)
    /* 完全列化（True SoA）の表: スロットは残った部分の自然な整列（完全列化のフィールドは列にある。PERF-1.0 フェーズ 5）
     * a True-SoA table: the slot has the natural alignment of the part left in it (the True-SoA fields live in columns;
     * PERF-1.0 phase 5) */
    CHECK(sizeof(f20_id_t) == 4u && _Alignof(f20_slot_task_t) == _Alignof(f20_part_task_t) && sizeof(f20_slot_task_t) == sizeof(f20_part_task_t));
#else
    CHECK(sizeof(f20_id_t) == 4u && _Alignof(f20_slot_task_t) == 16u && sizeof(f20_slot_task_t) % 16u == 0u);
#endif
    CHECK(((uintptr_t)arena_task.slot % 16u) == 0u && ((uintptr_t)arena_task.gen % 16u) == 0u);

    /* 1. 解放と再利用: 同じスロット・次の世代 / free and reuse: same slot, next generation */
    fsc_Id_task a = fsc_make(1u, 10u);
    CHECK(arena_task_free(a));
    fsc_Id_task b = fsc_make(2u, 20u);
    CHECK(F20_ID_INDEX(a, K) == F20_ID_INDEX(b, K));
    CHECK(F20_ID_GEN(b, K) == F20_ID_GEN(a, K) + 1u);
    CHECK(arena_task_get(a) == NULL && arena_task_get(b) != NULL);

    /* 2. 古い Id への書き込みは番兵へ。生きているスロットは汚れない / a stale write goes to the sentinel; the live slot is untouched */
    EXPECT_STOP(fsc_poke(a), FASCON_PANIC_STALE_ID);
    CHECK(arena_task.slot[F20_ID_INDEX(b, K)].v.vruntime == 20u);
    CHECK(arena_task.slot[F20_SENTINEL(K)].v.vruntime == 99u);

    /* 3. 古い Id を読んだあとの出力は出ない / output after a stale read never happens */
    out_len = 0;
    EXPECT_STOP(fsc_peek_print(a), FASCON_PANIC_STALE_ID);
    CHECK(out_len == 0u);

    /* 4. 古い値で回り続けるループも止まる / a loop spinning on a stale value stops too */
    EXPECT_STOP((void)fsc_spin(a), FASCON_PANIC_STALE_ID);

    /* 5. 総和: 番兵は入らない・死んだスロットは 0 / the sum excludes the sentinel; dead slots are zero */
    CHECK(fsc_total() == 20u);
    fsc_Id_task c = fsc_make(3u, 300u);
    fsc_Id_task d = fsc_make(4u, 4000u);
    CHECK(arena_task_free(c) && arena_task_get(d) != NULL);
    CHECK(fsc_total() == 4020u && fsc_total() == ref_total(0));
    CHECK(fsc_total2() == ref_total(1));
    CHECK(fsc_pids() == 2u + 4u);

    /* 6. 乱数で確保・解放を繰り返し、素朴な合計と比べる / random alloc/free against the plain sum */
    fsc_Id_task ids[8];
    uint32_t nids = 0;
    for (uint32_t i = 0; i < arena_task.st.next_unused; i++)
        if (arena_task.link[i] == F20_LIVE) ids[nids++] = F20_ID_MAKE(arena_task.gen[i], i, K);
    int mism = 0;
    for (int step = 0; step < 200000; step++) {
        if (nids < 8u && (nids == 0u || (rnd() & 1u))) {
            fsc_Task t = { .pid = rnd() & 0xFFu, .prio = rnd() & 3u, .vruntime = (rnd() & 1u) ? (rnd() & 7u) : (((uint64_t)rnd() << 24) ^ rnd()) };
            fsc_Id_task id = arena_task_alloc(&t);
            if (id != 0u) ids[nids++] = id;
        } else {
            uint32_t k = rnd() % nids;
            if (!arena_task_free(ids[k])) mism++;
            ids[k] = ids[--nids];
        }
        if (fsc_total() != ref_total(0) || fsc_total2() != ref_total(1)) mism++;
        /* 小さな表のマスク付き集計（<= は同じ値なら最後の Id） / masked reductions on the small table (<=: the last Id on a tie) */
        uint64_t c1 = 0, best = UINT64_MAX;
        fsc_Id_task am = 0u;
        for (uint32_t i = 0; i < arena_task.st.next_unused; i++) {
            if (arena_task.link[i] != F20_LIVE) continue;
            if (arena_task.slot[i].v.prio == 1u) c1++;
            if (arena_task.slot[i].v.vruntime <= best) { best = arena_task.slot[i].v.vruntime; am = F20_ID_MAKE(arena_task.gen[i], i, K); }
        }
        if (fsc_task_count_prio1() != c1 || fsc_task_argmin() != am) mism++;
    }
    CHECK(mism == 0);
    CHECK(arena_task.st.live == nids);

    /* 7. あふれは総和の終わりに検出してコミット点で止まる / overflow is detected at the end of the sum and stops at the commit point */
    while (nids) CHECK(arena_task_free(ids[--nids]));
    fsc_Id_task e = fsc_make(5u, UINT64_MAX - 15u);
    CHECK(fsc_total() == UINT64_MAX - 15u);
    fsc_Id_task f = fsc_make(6u, 15u);
    CHECK(fsc_total() == UINT64_MAX);
    CHECK(arena_task_free(f));
    f = fsc_make(6u, 16u);
    EXPECT_STOP((void)fsc_total(), FASCON_PANIC_ARITH);
    CHECK(arena_task_free(e) && arena_task_free(f));

    /* 8. 世代を使い切ったスロットは引退し、二度と配られない / an exhausted slot retires and is never handed out again */
    fsc_Id_task g = fsc_make(7u, 1u);
    const uint32_t gi = F20_ID_INDEX(g, K);
    arena_task.gen[gi] = F20_GEN_MAX(K); /* 最後の世代まで進んだことにする / pretend it reached the last generation */
    g = F20_ID_MAKE(F20_GEN_MAX(K), gi, K);
    const uint32_t retired = arena_task.st.retired;
    CHECK(arena_task_free(g));
    CHECK(arena_task.st.retired == retired + 1u && arena_task.gen[gi] == 0u && arena_task_get(g) == NULL);
    int reused = 0;
    for (int i = 0; i < 64; i++) {
        fsc_Id_task h = fsc_make(8u, 1u);
        if (F20_ID_INDEX(h, K) == gi) reused = 1;
        CHECK(arena_task_free(h));
    }
    CHECK(!reused);

    /* 9. 外からの生の値の検査 / validating raw values from outside */
    fsc_Id_task v = fsc_make(9u, 1u);
    CHECK(f20_arena_validate(arena_task.gen, arena_task.link, ARENA_task_CAP, K, v) == v);
    CHECK(f20_arena_validate(arena_task.gen, arena_task.link, ARENA_task_CAP, K, v + (1u << K)) == 0u); /* 先の世代 / a future generation */
    CHECK(f20_arena_validate(arena_task.gen, arena_task.link, ARENA_task_CAP, K, 0u) == 0u);
    CHECK(f20_arena_validate(arena_task.gen, arena_task.link, ARENA_task_CAP, K, 0xFFFFFFFFu) == 0u);
    CHECK(arena_task_free(v));

    /* 10. どんな値の Id でも表の外は指さない / no Id value points outside the table */
    int outside = 0;
    for (uint64_t raw = 0; raw <= 0xFFFFFFFFu; raw += 0x10001u) {
        uint32_t flt = 0;
        const fsc_Task *p = arena_task_at_r((uint32_t)raw, &flt);
        fsc_Task *q = arena_task_at((uint32_t)raw, &flt);
        if (p < &arena_task.slot[0].v || p > &arena_task.slot[F20_SENTINEL(K)].v) outside++;
        if (q < &arena_task.slot[0].v || q > &arena_task.slot[F20_SENTINEL(K)].v) outside++;
    }
    CHECK(outside == 0);

    /* 11. 配置: st が先頭、live・gen・link・slot は 128 バイト境界 / layout: st first, live/gen/link/slot on 128-byte boundaries */
    CHECK(offsetof(f20_arena_t(big), st) == 0u && offsetof(f20_arena_t(big), slot) % 128u == 0u);
    CHECK(offsetof(f20_arena_t(big), st) % 128u == 0u && offsetof(f20_arena_t(big), live) % 128u == 0u);
    CHECK(offsetof(f20_arena_t(big), gen) % 128u == 0u && offsetof(f20_arena_t(big), link) % 128u == 0u);
    CHECK(offsetof(f20_arena_t(big), live) - offsetof(f20_arena_t(big), st) == 128u); /* st は 128 B を占有 / st owns 128 B */
    CHECK(((uintptr_t)&arena_big % 128u) == 0u && fascon2_arena_region_count == 2u);

    /* 12. 回数の決まったループの中の古い Id は、ループの出口のコミット点で止まる / a stale Id in a fixed-trip loop stops at the exit */
    EXPECT_STOP((void)fsc_sweep(a), FASCON_PANIC_STALE_ID);

    /* 13. 生存ビットマップ・マスク付き集計・ビットマップのループを、乱数の確保と解放のもとで素朴な C と比べる
     *     the live bitmap, masked reductions and bitmap loops against plain C under random alloc / free */
    {
        fsc_Id_big bids[256];
        uint32_t nb = 0;
        int bad = 0, bad_bits = 0;
        for (int step = 0; step < 30000; step++) {
            uint32_t r = rnd();
            if (nb < 200u && (nb < 20u || (r & 3u) != 0u)) {
                fsc_Id_big id = fsc_big_make(rnd() & 15u, rnd() % 10u, (uint64_t)(rnd() % 50u) * ((rnd() & 7u) == 0u ? 0x100000000ull : 1u));
                if (id != 0u) bids[nb++] = id;
            } else if (nb) {
                uint32_t k = rnd() % nb;
                if (!fsc_big_free(bids[k])) bad++;
                bids[k] = bids[--nb];
            }
            if ((step & 7) != 0) continue;
            /* 生存ビット = link の生存印 / live bit == link's live mark */
            for (uint32_t i = 0; i < F20_SLOTS(ARENA_big_K); i++)
                if ((uint32_t)((arena_big.live[i >> 6] >> (i & 63u)) & 1u) != (uint32_t)(arena_big.link[i] == F20_LIVE)) bad_bits++;
            /* 素朴な参照 / plain references */
            uint64_t c_run = 0, c_above = 0, s_if = 0, mn = UINT64_MAX, best = UINT64_MAX, bmax = 0;
            uint32_t x = rnd() % 10u;
            uint64_t lim = 30u + rnd() % 30u, cap = 20u + rnd() % 40u, over = rnd() % 50u;
            fsc_Id_big amin = 0u, amax = 0u;
            uint32_t first = 0;
            int got_first = 0;
            for (uint32_t i = 0; i < arena_big.st.next_unused; i++) {
                if (arena_big.link[i] != F20_LIVE) continue;
                const fsc_Task *t = &arena_big.slot[i].v;
                fsc_Id_big id = F20_ID_MAKE(arena_big.gen[i], i, ARENA_big_K);
                if (t->prio == 0u) c_run += 1;
                if (t->prio > x) c_above += 2;
                if (t->prio >= 2u && t->vruntime < lim && (t->pid & 1u) == 0u) s_if += t->vruntime;
                if (t->vruntime < mn) mn = t->vruntime;
                if (t->vruntime < best) { best = t->vruntime; amin = id; }
                if (t->prio != 7u && bmax <= t->vruntime && t->vruntime < cap) { bmax = t->vruntime; amax = id; }
                if (!got_first && t->vruntime > over) { first = t->pid; got_first = 1; }
            }
            if (fsc_big_count_running() != c_run) bad++;
            if (fsc_big_count_above(x) != c_above) bad++;
            if (fsc_big_live3() != 3u * nb) bad++;
            if (fsc_big_sum_if(lim) != s_if) bad++;
            if (fsc_big_min() != mn) bad++;
            if (fsc_big_argmin() != amin) bad++;
            if (fsc_big_argmax_le(cap) != amax) bad++;
            if (fsc_big_first_over(over) != first) bad++;
            if ((step & 1023) == 0) {
                /* 書き込みのループ: 生存スロットの prio だけ +1 / the writing loop: +1 on live slots' prio only */
                uint32_t before[256];
                for (uint32_t i = 0; i < 256u; i++) before[i] = arena_big.slot[i].v.prio;
                fsc_big_bump();
                for (uint32_t i = 0; i < 256u; i++)
                    if (arena_big.slot[i].v.prio != before[i] + (arena_big.link[i] == F20_LIVE ? 1u : 0u)) bad++;
            }
            if ((step & 4095) == 4088) {
                /* 本体で解放するループ（語を読み直す形）: 奇数の pid をすべて解放 / the freeing loop: free every odd pid */
                uint64_t odd = 0;
                for (uint32_t i = 0; i < arena_big.st.next_unused; i++)
                    if (arena_big.link[i] == F20_LIVE && (arena_big.slot[i].v.pid & 1u)) odd++;
                if (fsc_big_free_odd() != odd) bad++;
                uint32_t keep = 0;
                for (uint32_t k = 0; k < nb; k++)
                    if (arena_big_get(bids[k]) != NULL) bids[keep++] = bids[k];
                nb = keep;
                if (arena_big.st.live != nb) bad++;
            }
        }
        CHECK(bad == 0);
        CHECK(bad_bits == 0);
        printf("live bitmap + masked reductions: 30000 random steps, %d mismatches, %d bitmap/link disagreements\n", bad, bad_bits);
    }

    printf("latch harness: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
