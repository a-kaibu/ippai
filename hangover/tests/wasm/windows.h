/* Just enough of windows.h for bench.c built straight to wasm (hangover/tests/bench-wasm.sh): the
 * phone's own speed, without the x86 translation, for web/bench.html?wasm */
#include <emscripten.h>
typedef union { long long QuadPart; } LARGE_INTEGER;
static inline int QueryPerformanceFrequency(LARGE_INTEGER *f) { f->QuadPart = 1000000; return 1; }
static inline int QueryPerformanceCounter(LARGE_INTEGER *t) { t->QuadPart = (long long)(emscripten_get_now() * 1000); return 1; }
