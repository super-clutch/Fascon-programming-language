/*
 * vram_raw.c — 比較用: 生ポインタで手書きした壁紙ループ（examples/gui_desktop.fsc の draw_wallpaper と同じ絵）
 * for comparison: the wallpaper loop written by hand with a raw pointer (same picture as draw_wallpaper in
 * examples/gui_desktop.fsc)
 *
 *   clang --target=aarch64-none-elf -mcpu=cortex-a72 -O2 -ffreestanding -fno-builtin -mstrict-align -S bench/vram_raw.c
 */
#include <stdint.h>

#define FB ((uint32_t *)0x48000000u)

static inline int32_t lerp(int32_t a, int32_t b, int32_t t, int32_t n) { return a + (b - a) * t / n; }
static inline uint32_t rgb(int32_t r, int32_t g, int32_t b) { return ((uint32_t)r << 16) | ((uint32_t)g << 8) | (uint32_t)b; }

void raw_wallpaper(void) {
    for (int32_t y = 0; y < 600; y++) {
        uint32_t c = rgb(lerp(18, 40, y, 599), lerp(32, 104, y, 599), lerp(64, 128, y, 599));
        uint32_t *row = FB + y * 800;
        for (int32_t x = 0; x < 800; x++) row[x] = c;
    }
}
