/* gpu_offload.gpu.c — gpu_offload.fsc の GPU カーネル（PC ではワーカースレッドが走らせ、--gpu=off では呼んだコアで走る）
 * the GPU kernels of gpu_offload.fsc (run by the worker thread on the PC, in place on the calling core with --gpu=off).
 * 要素の C の形は Fascon の struct Particle と同じ並び / the C shape of the element follows struct Particle of the Fascon source */
#include "fascon2_rt.h"

typedef struct {
    float x, v;
    uint32_t steps, tag;
} Particle;

/* STEP: x += v × grid × block / STEP: x += v * grid * block */
FASCON_GPU_KERNEL(1) {
    Particle *p = (Particle *)elem;
    (void)slot;
    p->x += p->v * (float)(grid * block);
    p->steps += 1u;
}

/* SCALE: x *= 2 / SCALE: x *= 2 */
FASCON_GPU_KERNEL(2) {
    Particle *p = (Particle *)elem;
    (void)slot;
    (void)grid;
    (void)block;
    p->x *= 2.0f;
    p->steps += 10u;
}
