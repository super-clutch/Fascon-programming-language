/* fsimd_c.h — 浮動小数点の計測の手書きの C（AoS）の宣言。fsimd_c.c を -DFAST=0（素直な -O2）と -DFAST=1（-ffast-math）で
 * 2 回コンパイルし、それぞれ c_* と c_*_fast を作る
 * declarations of the hand-written C (AoS) of the float benchmark. fsimd_c.c is compiled twice, -DFAST=0 (plain -O2) and
 * -DFAST=1 (-ffast-math), giving c_* and c_*_fast */
#ifndef FSIMD_C_H
#define FSIMD_C_H
#include <stdint.h>

typedef struct {
    float x, vx;
    double mass, energy;
    uint32_t kind, tag;
} CPart;
_Static_assert(sizeof(CPart) == 32, "CPart is 32 bytes");

extern CPart c_parts[NT], c_parts_fast[NT];
extern double c_arg;

#define FSIMD_DECL(sfx)                                                                                                \
    void c_build##sfx(void);                                                                                           \
    double c_mass##sfx(void);                                                                                          \
    double c_x##sfx(void);                                                                                             \
    double c_hot##sfx(void);                                                                                           \
    double c_heat##sfx(void);                                                                                          \
    double c_push##sfx(void);                                                                                          \
    double c_reset##sfx(void);                                                                                         \
    double c_set##sfx(void);
FSIMD_DECL()
FSIMD_DECL(_fast)
#endif
