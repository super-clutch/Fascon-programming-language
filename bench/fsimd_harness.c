/*
 * fsimd_harness.c — 浮動小数点の完全列化（#[fast_math]）と条件付き一括更新（Masked Column RMW）の計測ハーネス: Fascon の
 * 生成 C と、手書きの C（AoS: 同じ 32 バイトの構造体の配列）で同じ読み出しと書き込みを測る
 * timing harness for float True SoA (#[fast_math]) and masked column RMW: the same reads and writes in the generated C of
 * Fascon and in hand-written C (AoS: an array of the same 32-byte struct)
 *
 *   bench/fsimd_bench.sh が作る / built by bench/fsimd_bench.sh:
 *   cc -O2 -std=c11 [-march=native] -DNT=<粒子の数 / particles> -I<生成 C のフォルダ / generated C dir> \
 *      bench/fsimd_harness.c bench/fsimd_c.c -o h          (fsimd_c.c は -DFAST=0 / 1 で 2 回 / twice, -DFAST=0 / 1)
 *
 * 正直な注意 / honest caveats:
 *  - C の比較相手は 2 つ: 素直な手書きのループ（-O2。浮動小数点の和は書いた順なので、コンパイラはベクトル化しない）と、
 *    同じループを -ffast-math で（和の順序の入れ替えを許す。Fascon の #[fast_math] と同じ自由）
 *    two C baselines: plain hand-written loops (-O2; float sums keep the written order, so the compiler does not
 *    vectorize them) and the same loops at -ffast-math (reordering allowed: the same freedom as #[fast_math] in Fascon)
 *  - 1 台の仮想機械の結果。CPU・キャッシュ・メモリ・OS が変われば数字も変わる
 *    results from one virtual machine; numbers change with the CPU, caches, memory and OS
 *  - 測る関数は volatile な関数ポインタで呼ぶ / the timed functions are called through volatile function pointers
 */
#define _POSIX_C_SOURCE 200809L
#define _DEFAULT_SOURCE 1
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "fsimd_bench.fascon.c"
#if defined(__linux__)
#include <sys/mman.h>
#endif

#ifndef NT
#error "define NT (the particle count)"
#endif

#include "fsimd_c.h"

/* ───── プラットフォーム層（PC） / platform layer (PC) ───── */
void fascon_plat_print(const char *s, size_t len) { (void)s; (void)len; }
FASCON_NORETURN void fascon_plat_kernel_panic(uint32_t code, const char *msg) {
    fprintf(stderr, "FASCON PANIC 0x%04x %s\n", (unsigned)code, msg ? msg : "");
    exit(70);
}
FASCON_NORETURN void fascon_plat_exit(int code) { exit(code); }
FASCON_NORETURN void fascon_plat_halt(void) { exit(0); }
uint32_t fascon_plat_cpu_id(void) { return 0; }

CPart c_parts[NT] __attribute__((aligned(2097152)));
CPart c_parts_fast[NT] __attribute__((aligned(2097152)));
double c_arg;

/* ───── Fascon の関数を同じ形で / the Fascon functions in the same shape ───── */
static double f_mass(void) { return fsc_total_mass(); }
static double f_x(void) { return (double)fsc_total_x(); }
static double f_hot(void) { return fsc_hot_energy(); }
static double f_heat(void) { fsc_heat_hot(); return 0.0; }
static double f_push(void) { fsc_push_hot(); return 0.0; }
static double f_reset(void) { fsc_reset_cold(c_arg); return 0.0; }
static double f_set(void) { fsc_set_mass(c_arg); return 0.0; }

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
static volatile double sink;
typedef double (*fn_t)(void);

/* 1 粒子あたりの ns の中央値。代入は回ごとに値を変える / median ns per particle; assignments change the value every round */
static double measure(fn_t f0) {
    volatile fn_t f = f0;
    double v[REPS];
    for (int r = 0; r < REPS; r++) {
        double t0 = now_ns();
        for (int i = 0; i < INNER; i++) {
            c_arg = (double)((r * INNER + i) % 7) + 0.25;
            sink += f();
        }
        v[r] = (now_ns() - t0) / ((double)INNER * NT);
    }
    qsort(v, REPS, sizeof(double), cmp_d);
    return v[REPS / 2];
}

static double measure_ghz(void) {
#if defined(__x86_64__)
    double best = 0.0;
    for (int r = 0; r < 5; r++) {
        uint64_t x = 3u;
        const uint64_t n = 100000000u;
        double t0 = now_ns();
        for (uint64_t i = 0; i < n; i++) __asm__ volatile("imul %0, %0" : "+r"(x));
        double g = 3.0 * (double)n / (now_ns() - t0);
        sink += (double)x;
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
    if (madvise((void *)c_parts, sizeof c_parts, MADV_HUGEPAGE) != 0) return "madvise failed";
    if (madvise((void *)c_parts_fast, sizeof c_parts_fast, MADV_HUGEPAGE) != 0) return "madvise failed";
    return "2 MiB (MADV_HUGEPAGE)";
#else
    return "4 KiB";
#endif
}

/* 検算: 値のビットの並みの和（順序によらず正確）。Fascon と両方の C が同じ状態にあること / checks: sums of the bit
 * patterns of the values (exact in any order); Fascon and both C arrays must hold the same state */
static int check(const char *what) {
    uint64_t c[5] = {0, 0, 0, 0, 0}, cf[5] = {0, 0, 0, 0, 0};
    const CPart *ps[2] = {c_parts, c_parts_fast};
    uint64_t *out[2] = {c, cf};
    for (int k = 0; k < 2; k++)
        for (uint32_t i = 0; i < NT; i++) {
            uint32_t xb;
            uint64_t mb, eb;
            memcpy(&xb, &ps[k][i].x, 4);
            memcpy(&mb, &ps[k][i].mass, 8);
            memcpy(&eb, &ps[k][i].energy, 8);
            out[k][0] += xb;
            out[k][1] += mb >> 32;
            out[k][2] += mb & 0xFFFFFFFFu;
            out[k][3] += eb >> 32;
            out[k][4] += eb & 0xFFFFFFFFu;
        }
    const uint64_t f[5] = {fsc_bits_x(), fsc_bits_hi(0), fsc_bits_lo(0), fsc_bits_hi(1), fsc_bits_lo(1)};
    for (int i = 0; i < 5; i++)
        if (f[i] != c[i] || f[i] != cf[i]) {
            printf("state mismatch after %s (%d): fascon %llu, C %llu, C fast-math %llu\n", what, i, (unsigned long long)f[i], (unsigned long long)c[i], (unsigned long long)cf[i]);
            return 1;
        }
    /* 和は順序で末尾が変わりうる: 相対誤差で。f32 の 4 百万個の書いた順の和は C の側の丸めだけで 1 割ほどずれうるので、
     * 大きな誤り（半分を読み落とすなど）だけを見る。正確な検算は上のビットの和 / sums may differ in the last places with
     * the order: a relative check. A written-order f32 sum of 4 million values drifts by about 10% from the rounding of the C
     * side alone, so only gross errors (half the data missed, say) are caught there; the exact checks are the bit sums above */
    const double fs[3] = {f_mass(), f_x(), f_hot()}, cs[3] = {c_mass(), c_x(), c_hot()};
    const double tol[3] = {1e-9, 0.25, 1e-9};
    for (int i = 0; i < 3; i++) {
        const double d = fs[i] - cs[i], m = cs[i] < 0 ? -cs[i] : cs[i];
        if ((d < 0 ? -d : d) > tol[i] * (m > 1.0 ? m : 1.0)) {
            printf("sum mismatch after %s (%d): fascon %.17g, C %.17g\n", what, i, fs[i], cs[i]);
            return 1;
        }
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
    c_build_fast();
    if (check("build")) return 1;
    const double ghz = measure_ghz();
    static const char *names[7] = {"read: sum of mass (f64)", "read: sum of x (f32)", "read: energy sum of HOT (f64, filtered)", "write: energy += 0.5, HOT (f64 masked add)", "write: x += 1.0, HOT (f32 masked add)", "write: energy = e, COLD (f64 masked set)", "write: mass = m, every particle (f64)"};
    const fn_t fs[7] = {f_mass, f_x, f_hot, f_heat, f_push, f_reset, f_set};
    const fn_t cs[7] = {c_mass, c_x, c_hot, c_heat, c_push, c_reset, c_set};
    const fn_t cfs[7] = {c_mass_fast, c_x_fast, c_hot_fast, c_heat_fast, c_push_fast, c_reset_fast, c_set_fast};
    printf("# %u particles, pages %s, core clock %.2f GHz (IMUL chain), Fascon table %zu B\n", (unsigned)NT, hp, ghz, sizeof arena_p);
    for (int k = 0; k < 7; k++) {
        const double fv = measure(fs[k]);
        const double cv = measure(cs[k]);
        const double cfv = measure(cfs[k]);
        printf("%s\t%s\t%.3f\t%.3f\t%.3f\t%.2f\t%.2f\t%.2f\n", label, names[k], fv, cv, cfv, fv * ghz, cv * ghz, cfv * ghz);
        /* 書き込みは 3 つとも同じ回数・同じ値で走ったので、状態は同じはず / every write ran the same number of times with the
         * same values in all three, so the states must agree */
        if (k >= 3 && check(names[k])) return 1;
    }
    return 0;
}
