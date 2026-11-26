/* fmtfuzz.c — fascon_put_f64 / fascon_put_f32（往復できる最短の 10 進）の差分テスト。乱数のビットの並み（正規数・非正規数・
 * 2 のべき乗とその前後・ふつうの値）を書き出し、fmtcheck.py が Python の repr と Decimal で、読み戻すと同じ値になること・桁数が
 * 最短であること・同じ長さの候補が 2 つあれば近い方（ちょうど真ん中なら偶数）であること・小数と指数の形を確かめる
 * a differential test of fascon_put_f64 / fascon_put_f32 (the shortest decimal that reads back): it prints random bit
 * patterns (normals, subnormals, powers of two and their neighbours, ordinary values) and fmtcheck.py checks with Python's
 * repr and Decimal that each reads back to the same value, has the fewest digits, takes the nearer of two equally short
 * candidates (the even one on an exact tie), and has the decimal / exponent shape
 *   cc -O2 -std=c11 -DFASCON2_HOSTED_MMIO -I runtime tests/float/fmtfuzz.c bsp/host/fascon2_plat_host.c -o fmtfuzz
 *   ./fmtfuzz [count] > out.txt && python3 tests/float/fmtcheck.py out.txt */
#define FASCON2_FLOAT 1
#define FASCON2_RT_IMPLEMENTATION
#include "fascon2_rt.h"
static uint64_t st = 0x9E3779B97F4A7C15ull;
static uint64_t rnd(void) { st ^= st << 13; st ^= st >> 7; st ^= st << 17; return st; }
static int count = 200000;
void fascon2_main(void) {
    for (int i = 0; i < count; i++) {
        uint64_t b = rnd();
        int kind = i % 8;
        if (kind == 1) b = (b & 0x800FFFFFFFFFFFFFull) | ((uint64_t)(rnd() % 2047) << 52);
        if (kind == 2) b = (b & 0x000FFFFFFFFFFFFFull);                 /* subnormal */
        if (kind == 3) b = ((uint64_t)(rnd() % 2046 + 1) << 52);         /* power of two */
        if (kind == 4) b = ((uint64_t)(rnd() % 2046 + 1) << 52) | 1u;    /* just above a power of two */
        if (kind == 5) b = ((uint64_t)(rnd() % 2046 + 1) << 52) - 1u;    /* just below */
        if ((b >> 52 & 0x7FF) == 0x7FF) b &= ~(1ull << 62);
        fascon_put_hex(b); fascon_put_str(FASCON_STR(" ")); fascon_put_f64(f20_f64_from_bits(b)); fascon_put_str(FASCON_STR("\n"));
        uint32_t c = (uint32_t)rnd();
        if (kind == 1) c = (c & 0x807FFFFFu) | ((uint32_t)(rnd() % 255) << 23);
        if (kind == 2) c &= 0x007FFFFFu;
        if (kind == 3) c = (uint32_t)(rnd() % 254 + 1) << 23;
        if (kind == 4) c = ((uint32_t)(rnd() % 254 + 1) << 23) | 1u;
        if (kind == 5) c = ((uint32_t)(rnd() % 254 + 1) << 23) - 1u;
        if ((c >> 23 & 0xFF) == 0xFF) c &= ~(1u << 30);
        fascon_put_str(FASCON_STR("f ")); fascon_put_hex(c); fascon_put_str(FASCON_STR(" ")); fascon_put_f32(f20_f32_from_bits(c)); fascon_put_str(FASCON_STR("\n"));
    }
}
const FasconMmioRegion fascon2_mmio_regions[1] = { { NULL, 0u, 0u, 0u, 0u, 0u, 0u } };
const uint32_t fascon2_mmio_region_count = 0u;
uintptr_t fascon2_host_mmio_base[1];
const FasconArenaRegion fascon2_arena_regions[1] = { { 0 } };
const uint32_t fascon2_arena_region_count = 0u;
