/* rmw_kernels.c — 条件付き一括更新の走査関数（f20_col_masked_add_* / f20_col_masked_set_*）を、ふつうのスカラの更新と比べる。
 * 乱数の列とマスク（0・~0・ばらばらの語）、あふれる値（整数）、NaN と -0.0（浮動小数点）、nt = 0 / 1。整数の加算は、あふれる
 * 語より前だけが更新され、その語の先頭の添字が返ること
 * checks the masked RMW kernels (f20_col_masked_add_* / f20_col_masked_set_*) against a plain scalar update: random
 * columns and masks (0, ~0 and mixed words), overflowing values (integers), NaN and -0.0 (floats), nt = 0 / 1. An integer
 * add must update only the words before the overflowing one and return that word's first index
 *   cc -O2 -std=c11 -I runtime tests/cols/rmw_kernels.c -o rmw && ./rmw   (prints "ok" and exits 0) */
#include "fascon2_rt.h"
#include <stdio.h>
#include <string.h>

#define N 4096u
static uint64_t st = 88172645463325252ull;
static uint64_t rnd(void) { st ^= st << 13; st ^= st >> 7; st ^= st << 17; return st; }
static int bad = 0;
static F20_ALIGN128 uint64_t mask[N / 64];
static F20_ALIGN128 uint32_t a32[N], b32[N];
static F20_ALIGN128 uint64_t a64[N], b64[N];
static F20_ALIGN128 float af[N], bf[N];
static F20_ALIGN128 double ad[N], bd[N];

static void masks(void) {
    for (uint32_t w = 0; w < N / 64; w++) {
        uint64_t k = rnd() % 4;
        mask[w] = k == 0 ? 0u : k == 1 ? ~UINT64_C(0) : (rnd() & rnd());
    }
}
static int on(uint32_t i) { return (int)((mask[i >> 6] >> (i & 63)) & 1u); }

#define CHECK(cond, what) do { if (!(cond)) { if (bad < 20) printf("BAD %s (round %d)\n", what, round); bad++; } } while (0)

int main(void) {
    for (int round = 0; round < 400; round++) {
        masks();
        const int nt = (int)(rnd() & 1u);
        const uint32_t n = 64u * (1u + (uint32_t)(rnd() % (N / 64)));
        /* u32 add: 大きな値を混ぜてときどきあふれさせる / mix in big values so it sometimes overflows */
        for (uint32_t i = 0; i < N; i++) a32[i] = b32[i] = (rnd() % 7 == 0) ? 0xFFFFFFF0u + (uint32_t)(rnd() % 16) : (uint32_t)rnd() % 100000u;
        uint32_t x = (rnd() % 5 == 0) ? 17u : (uint32_t)(rnd() % 1000);
        if (round % 3 == 0) for (uint32_t i = 0; i < N; i++) a32[i] = b32[i] = a32[i] % 100000u; /* no overflow */
        uint32_t stop = f20_col_masked_add_u32(a32, mask, n, x, nt), want = F20_RMW_NONE;
        for (uint32_t w = 0; w < n / 64 && want == F20_RMW_NONE; w++)
            for (uint32_t i = w * 64; i < w * 64 + 64; i++)
                if (on(i) && b32[i] + x < b32[i]) { want = w * 64; break; }
        CHECK(stop == want, "u32 stop");
        for (uint32_t i = 0; i < N; i++) {
            uint32_t e = (i < n && i < want && on(i)) ? b32[i] + x : b32[i];
            CHECK(a32[i] == e, "u32 value");
        }
        /* i64 add（負の x も） / i64 add (negative x too) */
        for (uint32_t i = 0; i < N; i++) a64[i] = b64[i] = (rnd() % 9 == 0) ? (uint64_t)(INT64_MIN + (int64_t)(rnd() % 8)) : (uint64_t)((int64_t)(rnd() % 2000000) - 1000000);
        int64_t xs = (round % 2) ? -12345 : 777;
        if (round % 5 == 0) xs = INT64_MIN / 2;
        stop = f20_col_masked_add_i64((int64_t *)(void *)a64, mask, n, xs, nt);
        want = F20_RMW_NONE;
        for (uint32_t w = 0; w < n / 64 && want == F20_RMW_NONE; w++)
            for (uint32_t i = w * 64; i < w * 64 + 64; i++) {
                int64_t r;
                if (on(i) && __builtin_add_overflow((int64_t)b64[i], xs, &r)) { want = w * 64; break; }
            }
        CHECK(stop == want, "i64 stop");
        for (uint32_t i = 0; i < N; i++) {
            uint64_t e = (i < n && i < want && on(i)) ? (uint64_t)((int64_t)b64[i] + xs) : b64[i];
            CHECK(a64[i] == e, "i64 value");
        }
        /* u64 set / u64 set */
        for (uint32_t i = 0; i < N; i++) a64[i] = b64[i] = rnd();
        uint64_t y = rnd();
        f20_col_masked_set_u64(a64, mask, n, y, nt);
        for (uint32_t i = 0; i < N; i++) CHECK(a64[i] == ((i < n && on(i)) ? y : b64[i]), "u64 set");
        /* f32 add / set: NaN・-0.0 はビットのまま残る / NaN and -0.0 keep their bits */
        for (uint32_t i = 0; i < N; i++) {
            uint64_t k = rnd() % 10;
            af[i] = k == 0 ? -0.0f : k == 1 ? f20_f32_from_bits(0x7FC00001u + (uint32_t)(rnd() % 8)) : (float)((int64_t)(rnd() % 2001) - 1000);
            bf[i] = af[i];
        }
        float xf = (round % 4 == 0) ? -0.0f : (round % 4 == 1) ? 0.0f : (float)((int64_t)(rnd() % 101) - 50);
        f20_col_masked_add_f32(af, mask, n, xf, nt);
        for (uint32_t i = 0; i < N; i++) {
            float e = (i < n && on(i)) ? bf[i] + xf : bf[i];
            CHECK(f20_f32_to_bits(af[i]) == f20_f32_to_bits(e) || (f20_f32_is_nan(af[i]) && f20_f32_is_nan(e) && (i < n && on(i))), "f32 add");
        }
        for (uint32_t i = 0; i < N; i++) bd[i] = ad[i] = (rnd() % 7 == 0) ? -0.0 : (double)rnd() * 1e-9;
        double yd = (round % 3) ? -0.0 : 2.5;
        f20_col_masked_set_f64(ad, mask, n, yd, nt);
        for (uint32_t i = 0; i < N; i++) CHECK(f20_f64_to_bits(ad[i]) == f20_f64_to_bits((i < n && on(i)) ? yd : bd[i]), "f64 set");
        f20_col_masked_add_f64(ad, mask, n, 0.25, nt);
        for (uint32_t i = 0; i < N; i++) {
            double base = (i < n && on(i)) ? yd : bd[i];
            CHECK(f20_f64_to_bits(ad[i]) == f20_f64_to_bits((i < n && on(i)) ? base + 0.25 : base), "f64 add");
        }
        /* i32 set / i32 set */
        for (uint32_t i = 0; i < N; i++) a32[i] = b32[i] = (uint32_t)rnd();
        f20_col_masked_set_i32((int32_t *)(void *)a32, mask, n, -5, nt);
        for (uint32_t i = 0; i < N; i++) CHECK(a32[i] == ((i < n && on(i)) ? (uint32_t)-5 : b32[i]), "i32 set");
    }
    F20_SFENCE();
    if (bad) {
        printf("%d mismatches\n", bad);
        return 1;
    }
    printf("ok\n");
    return 0;
}
