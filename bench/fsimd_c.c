/* fsimd_c.c — 浮動小数点の計測の手書きの C（AoS）。-DFAST=0 は素直な -O2、-DFAST=1 は -ffast-math でコンパイルする（リンクは
 * -ffast-math なしで: crtfastmath.o を入れず、プログラム全体の MXCSR を変えない）
 * the hand-written C (AoS) of the float benchmark. -DFAST=0 is compiled at plain -O2, -DFAST=1 with -ffast-math (linked
 * without -ffast-math, so crtfastmath.o stays out and the MXCSR of the program is unchanged) */
#include "fsimd_c.h"

#if FAST
#define P c_parts_fast
#define F(name) name##_fast
#else
#define P c_parts
#define F(name) name
#endif

void F(c_build)(void) {
    for (uint64_t k = 0; k < NT; k++) {
        P[k].x = (float)(k % 1000u) * 0.25f;
        P[k].vx = 1.0f;
        P[k].mass = (double)(k % 977u) * 0.5 + 1.0;
        P[k].energy = (double)(k % 61u);
        P[k].kind = (uint32_t)(((k * 2654435761u) >> 13) % 4u);
        P[k].tag = (uint32_t)k;
    }
}
double F(c_mass)(void) {
    double s = 0.0;
    for (uint32_t i = 0; i < NT; i++) s += P[i].mass;
    return s;
}
double F(c_x)(void) {
    float s = 0.0f;
    for (uint32_t i = 0; i < NT; i++) s += P[i].x;
    return (double)s;
}
double F(c_hot)(void) {
    double s = 0.0;
    for (uint32_t i = 0; i < NT; i++)
        if (P[i].kind == 1u) s += P[i].energy;
    return s;
}
double F(c_heat)(void) {
    for (uint32_t i = 0; i < NT; i++)
        if (P[i].kind == 1u) P[i].energy += 0.5;
    return 0.0;
}
double F(c_push)(void) {
    for (uint32_t i = 0; i < NT; i++)
        if (P[i].kind == 1u) P[i].x += 1.0f;
    return 0.0;
}
double F(c_reset)(void) {
    const double e = c_arg;
    for (uint32_t i = 0; i < NT; i++)
        if (P[i].kind == 2u) P[i].energy = e;
    return 0.0;
}
double F(c_set)(void) {
    const double m = c_arg;
    for (uint32_t i = 0; i < NT; i++) P[i].mass = m;
    return 0.0;
}
