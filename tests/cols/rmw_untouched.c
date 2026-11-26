/* rmw_untouched.c - checks for the masked column RMW kernels (f20_col_masked_add_* / f20_col_masked_set_*):
 *  1. every one of the 12 kernels (add / set x u32 u64 i32 i64 f32 f64, nt = 0 / 1) against a scalar model
 *  2. all-zero filter words never load or store the column: the pages of the column that only zero words cover are
 *     PROT_NONE while the kernel runs (a touch is a SIGSEGV), and a PROT_NONE guard page sits right after the column and
 *     right after the filter words (no read past n)
 *  3. integer adds: an overflowing word leaves that word and every later word untouched and returns its first index
 * Linux only (mmap / mprotect).
 *   cc -O2 -std=gnu11 -I runtime tests/cols/rmw_untouched.c -o u && ./u   (prints "ok" and exits 0) */
#define _GNU_SOURCE
#include "fascon2_rt.h"
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define PAGE 4096u
static uint64_t st = 0x9E3779B97F4A7C15ull;
static uint64_t rnd(void) {
    st ^= st << 13;
    st ^= st >> 7;
    st ^= st << 17;
    return st;
}
static sigjmp_buf jb;
static volatile int in_kernel = 0;
static void on_segv(int s) {
    (void)s;
    if (in_kernel) siglongjmp(jb, 1);
    _exit(3);
}
static int bad = 0;
#define FAIL(...)                              \
    do {                                       \
        if (bad < 30) printf("BAD " __VA_ARGS__); \
        bad++;                                 \
    } while (0)

/* an area of `bytes` whose end touches a PROT_NONE guard page; returns its start (page offset kept 256-aligned) */
typedef struct {
    uint8_t *base;
    size_t pages;
    uint8_t *start;
} area_t;
static area_t area_new(size_t bytes) {
    area_t a;
    a.pages = (bytes + PAGE - 1u) / PAGE;
    a.base = mmap(NULL, (a.pages + 1u) * PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (a.base == MAP_FAILED) {
        perror("mmap");
        _exit(2);
    }
    mprotect(a.base + a.pages * PAGE, PAGE, PROT_NONE);
    a.start = a.base + a.pages * PAGE - bytes;
    return a;
}
static void area_free(area_t *a) { munmap(a->base, (a->pages + 1u) * PAGE); }

static int on(const uint64_t *m, uint32_t i) { return (int)((m[i >> 6] >> (i & 63u)) & 1u); }

/* one round for element type T: values come from fill(i), the model from model(v) */
#define ROUND(NAME, T, ISADD, ISINT, ISFLOAT, ISSIGNED, FILL, XGEN, OVF)                                                     \
    /* the kernel runs in a function of its own, so nothing of it lives across sigsetjmp */                            \
    static __attribute__((noinline)) uint32_t run_##NAME##_##ISADD(T *c, const uint64_t *m, uint32_t n, T x, int nt) { \
        return KERNEL_##NAME##_##ISADD(c, m, n, x, nt);                                                                \
    }                                                                                                                  \
    static void round_##NAME##_##ISADD(int nt) {                                                                       \
        const size_t sz = sizeof(T);                                                                                   \
        const uint32_t words = 1u + (uint32_t)(rnd() % 160u);                                                        \
        const uint32_t n = words * 64u;                                                                                \
        area_t ca = area_new((size_t)n * sz), ma = area_new((size_t)words * 8u);                                      \
        T *c = (T *)(void *)ca.start;                                                                                  \
        uint64_t *m = (uint64_t *)(void *)ma.start;                                                                    \
        static T ref[160u * 64u];                                                                                      \
        static uint8_t prot[200];                                                                                      \
        for (uint32_t w = 0; w < words; w++) {                                                                         \
            const uint64_t k = rnd() % 4u;                                                                             \
            m[w] = k == 0 ? 0u : k == 1 ? ~UINT64_C(0) : (rnd() & rnd() & rnd()) | (UINT64_C(1) << (rnd() & 63u));   \
        }                                                                                                              \
        for (uint32_t i = 0; i < n; i++) {                                                                             \
            const T v = FILL;                                                                                          \
            c[i] = v;                                                                                                  \
            ref[i] = v;                                                                                                \
        }                                                                                                              \
        /* pages covered only by zero words: zero them all on purpose for some pages, then protect */                 \
        const size_t off = (size_t)(ca.start - ca.base);                                                               \
        for (size_t p = 0; p < ca.pages; p++) {                                                                        \
            prot[p] = 0;                                                                                               \
            const size_t lo_b = p * PAGE < off ? 0 : p * PAGE - off, hi_b = (p + 1u) * PAGE - off;                    \
            const uint32_t lo = (uint32_t)(lo_b / sz), hi = (uint32_t)(hi_b / sz < n ? hi_b / sz : n);                \
            if (rnd() % 3u == 0) {                                                                                     \
                for (uint32_t w = lo >> 6; w <= (hi - 1u) >> 6; w++) m[w] = 0u;                                         \
            }                                                                                                          \
            int zero = 1;                                                                                              \
            for (uint32_t w = lo >> 6; w <= (hi - 1u) >> 6; w++) zero &= m[w] == 0u;                                   \
            prot[p] = (uint8_t)zero;                                                                                   \
        }                                                                                                              \
        const T x = XGEN;                                                                                              \
        /* the model */                                                                                                \
        uint32_t want = F20_RMW_NONE;                                                                                  \
        if (ISADD && ISINT) {                                                                                          \
            for (uint32_t w = 0; w < words && want == F20_RMW_NONE; w++)                                               \
                for (uint32_t i = w * 64u; i < w * 64u + 64u; i++) {                                                   \
                    T r;                                                                                               \
                    if (on(m, i) && OVF(ref[i], x, &r)) {                                           \
                        want = w * 64u;                                                                                \
                        break;                                                                                         \
                    }                                                                                                  \
                }                                                                                                      \
        }                                                                                                              \
        for (size_t p = 0; p < ca.pages; p++)                                                                          \
            if (prot[p]) mprotect(ca.base + p * PAGE, PAGE, PROT_NONE);                                                \
        volatile uint32_t got = F20_RMW_NONE;                                                                          \
        in_kernel = 1;                                                                                                 \
        if (sigsetjmp(jb, 1) == 0) {                                                                                   \
            got = run_##NAME##_##ISADD(c, m, n, x, nt);                                                                \
            in_kernel = 0;                                                                                             \
        } else {                                                                                                       \
            in_kernel = 0;                                                                                             \
            FAIL("%s add=%d nt=%d: touched a protected page (zero words) or ran past n\n", #NAME, ISADD, nt);         \
        }                                                                                                              \
        F20_SFENCE();                                                                                                  \
        mprotect(ca.base, ca.pages * PAGE, PROT_READ | PROT_WRITE);                                                    \
        if (ISADD && ISINT && got != want) FAIL("%s add nt=%d: stop %u, want %u\n", #NAME, nt, (unsigned)got, want);  \
        for (uint32_t i = 0; i < n; i++) {                                                                             \
            T e = ref[i];                                                                                              \
            if (on(m, i) && i < want) e = ISADD ? (T)(ref[i] + x) : x;                                                 \
            int same = memcmp(&c[i], &e, sizeof(T)) == 0;                                                              \
            if (!same && ISFLOAT && ISADD && c[i] != c[i] && e != e && on(m, i)) same = 1; /* NaN payloads */         \
            if (!same) {                                                                                               \
                FAIL("%s add=%d nt=%d: slot %u (word %u, mask %016llx)\n", #NAME, ISADD, nt, i, i >> 6, (unsigned long long)m[i >> 6]); \
                break;                                                                                                 \
            }                                                                                                          \
        }                                                                                                              \
        area_free(&ca);                                                                                                \
        area_free(&ma);                                                                                                \
        (void)ISSIGNED;                                                                                                \
    }

#define KERNEL_u32_1 f20_col_masked_add_u32
#define KERNEL_u64_1 f20_col_masked_add_u64
#define KERNEL_i32_1 f20_col_masked_add_i32
#define KERNEL_i64_1 f20_col_masked_add_i64
#define KERNEL_f32_1(c, m, n, x, nt) (f20_col_masked_add_f32(c, m, n, x, nt), F20_RMW_NONE)
#define KERNEL_f64_1(c, m, n, x, nt) (f20_col_masked_add_f64(c, m, n, x, nt), F20_RMW_NONE)
#define KERNEL_u32_0(c, m, n, x, nt) (f20_col_masked_set_u32(c, m, n, x, nt), F20_RMW_NONE)
#define KERNEL_u64_0(c, m, n, x, nt) (f20_col_masked_set_u64(c, m, n, x, nt), F20_RMW_NONE)
#define KERNEL_i32_0(c, m, n, x, nt) (f20_col_masked_set_i32(c, m, n, x, nt), F20_RMW_NONE)
#define KERNEL_i64_0(c, m, n, x, nt) (f20_col_masked_set_i64(c, m, n, x, nt), F20_RMW_NONE)
#define KERNEL_f32_0(c, m, n, x, nt) (f20_col_masked_set_f32(c, m, n, x, nt), F20_RMW_NONE)
#define KERNEL_f64_0(c, m, n, x, nt) (f20_col_masked_set_f64(c, m, n, x, nt), F20_RMW_NONE)

/* values: mostly small, sometimes at the edge so that adds overflow */
#define FILL_U32 ((rnd() % 9 == 0) ? 0xFFFFFFF0u + (uint32_t)(rnd() % 16) : (uint32_t)(rnd() % 100000u))
#define FILL_U64 ((rnd() % 9 == 0) ? UINT64_MAX - (rnd() % 16) : rnd() % 1000000u)
#define FILL_I32 ((rnd() % 9 == 0) ? (int32_t)(INT32_MIN + (int32_t)(rnd() % 8)) : (rnd() % 9 == 1) ? (int32_t)(INT32_MAX - (int32_t)(rnd() % 8)) : (int32_t)((int64_t)(rnd() % 200001) - 100000))
#define FILL_I64 ((rnd() % 9 == 0) ? (int64_t)(INT64_MIN + (int64_t)(rnd() % 8)) : (rnd() % 9 == 1) ? (int64_t)(INT64_MAX - (int64_t)(rnd() % 8)) : (int64_t)(rnd() % 2000001) - 1000000)
#define FILL_F32 ((rnd() % 10 == 0) ? -0.0f : (rnd() % 10 == 1) ? f20_f32_from_bits(0x7FC00001u + (uint32_t)(rnd() % 8)) : (rnd() % 10 == 2) ? f20_f32_from_bits(0xFF800000u) : (float)((int64_t)(rnd() % 2001) - 1000) * 0.5f)
#define FILL_F64 ((rnd() % 10 == 0) ? -0.0 : (rnd() % 10 == 1) ? f20_f64_from_bits(UINT64_C(0x7FF8000000000001) + rnd() % 8) : (double)((int64_t)(rnd() % 2000001) - 1000000) * 0.25)
#define X_U32 ((rnd() % 4 == 0) ? (uint32_t)(rnd() % 32) : (uint32_t)(rnd() % 1000))
#define X_U64 ((rnd() % 4 == 0) ? rnd() % 32 : rnd() % 1000)
#define X_I32 ((rnd() % 2) ? (int32_t)(rnd() % 50) - 25 : (int32_t)((rnd() % 3 == 0) ? -9 : 7))
#define X_I64 ((rnd() % 2) ? (int64_t)(rnd() % 50) - 25 : (int64_t)((rnd() % 3 == 0) ? -9 : 7))
#define X_F32 ((rnd() % 4 == 0) ? -0.0f : (rnd() % 4 == 1) ? 0.0f : (float)((int64_t)(rnd() % 101) - 50) * 0.25f)
#define X_F64 ((rnd() % 4 == 0) ? -0.0 : (rnd() % 4 == 1) ? 0.0 : (double)((int64_t)(rnd() % 101) - 50) * 0.125)

#define IOVF(a, b, r) __builtin_add_overflow((a), (b), (r))
#define FOVF(a, b, r) ((void)(r), 0)
ROUND(u32, uint32_t, 1, 1, 0, 0, FILL_U32, X_U32, IOVF)
ROUND(u64, uint64_t, 1, 1, 0, 0, FILL_U64, X_U64, IOVF)
ROUND(i32, int32_t, 1, 1, 0, 1, FILL_I32, X_I32, IOVF)
ROUND(i64, int64_t, 1, 1, 0, 1, FILL_I64, X_I64, IOVF)
ROUND(f32, float, 1, 0, 1, 1, FILL_F32, X_F32, FOVF)
ROUND(f64, double, 1, 0, 1, 1, FILL_F64, X_F64, FOVF)
ROUND(u32, uint32_t, 0, 1, 0, 0, FILL_U32, X_U32, IOVF)
ROUND(u64, uint64_t, 0, 1, 0, 0, FILL_U64, X_U64, IOVF)
ROUND(i32, int32_t, 0, 1, 0, 1, FILL_I32, X_I32, IOVF)
ROUND(i64, int64_t, 0, 1, 0, 1, FILL_I64, X_I64, IOVF)
ROUND(f32, float, 0, 0, 1, 1, FILL_F32, X_F32, FOVF)
ROUND(f64, double, 0, 0, 1, 1, FILL_F64, X_F64, FOVF)

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_segv;
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
    for (int r = 0; r < 300; r++) {
        for (int nt = 0; nt < 2; nt++) {
            round_u32_1(nt);
            round_u64_1(nt);
            round_i32_1(nt);
            round_i64_1(nt);
            round_f32_1(nt);
            round_f64_1(nt);
            round_u32_0(nt);
            round_u64_0(nt);
            round_i32_0(nt);
            round_i64_0(nt);
            round_f32_0(nt);
            round_f64_0(nt);
        }
    }
    if (bad) {
        printf("%d mismatches (F20_VECTOR_BYTES %d)\n", bad, F20_VECTOR_BYTES);
        return 1;
    }
    printf("ok (F20_VECTOR_BYTES %d)\n", F20_VECTOR_BYTES);
    return 0;
}
