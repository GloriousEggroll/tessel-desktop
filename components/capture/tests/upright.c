/* copy_upright() turns a portal frame upright: for each wl_output transform,
 * build the buffer wlroots renders (the inverse transform of the picture)
 * and check the copy matches the picture. Run with tests/run_upright.sh. */
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
enum { SPA_META_TRANSFORMATION_None, SPA_META_TRANSFORMATION_90, SPA_META_TRANSFORMATION_180, SPA_META_TRANSFORMATION_270,
       SPA_META_TRANSFORMATION_Flipped, SPA_META_TRANSFORMATION_Flipped90, SPA_META_TRANSFORMATION_Flipped180, SPA_META_TRANSFORMATION_Flipped270 };
/* copy_upright() from native/capture_pipewire.c, extracted by run_upright.sh */
#include "copy_upright.c"
/* wlroots util/box.c, for 1x1 boxes */
static void box(int t, int x, int y, int width, int height, int *dx, int *dy) {
    switch (t) {
    case 0: *dx = x; *dy = y; break;
    case 1: *dx = height - y - 1; *dy = x; break;
    case 2: *dx = width - x - 1; *dy = height - y - 1; break;
    case 3: *dx = y; *dy = width - x - 1; break;
    case 4: *dx = width - x - 1; *dy = y; break;
    case 5: *dx = y; *dy = x; break;
    case 6: *dx = x; *dy = height - y - 1; break;
    default: *dx = height - y - 1; *dy = width - x - 1; break;
    }
}
static int invert(int t) { return (t % 2 && t < 4) ? t ^ 2 : t; } /* wlr_output_transform_invert */
int main(void) {
    const uint32_t W = 5, H = 3; /* the upright (logical) picture */
    int failures = 0;
    for (int t = 0; t < 8; t++) {
        uint32_t bw = t % 2 ? H : W, bh = t % 2 ? W : H, stride = bw * 4 + 8;
        uint32_t logical[15]; for (uint32_t i = 0; i < W*H; i++) logical[i] = 0x01000000u * (i + 1);
        uint8_t *buffer = calloc(bh, stride);
        /* wlr_scene: logical -> buffer through the inverse of the output transform */
        for (uint32_t y = 0; y < H; y++) for (uint32_t x = 0; x < W; x++) {
            int bx, by; box(invert(t), x, y, W, H, &bx, &by);
            memcpy(buffer + by*stride + bx*4, &logical[y*W + x], 4);
        }
        uint32_t out[15], ow, oh;
        copy_upright((uint8_t *)out, buffer, stride, bw, bh, t, &ow, &oh);
        int ok = ow == W && oh == H && !memcmp(out, logical, sizeof(out));
        printf("transform %d: %s\n", t, ok ? "upright" : "WRONG");
        failures += !ok; free(buffer);
    }
    return failures;
}
