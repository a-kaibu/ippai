/* Shared by the benchmark programs (tests/bench): timing, the result lines tests/bench/run.mjs
 * reads ("BENCH <name> <ms>", lower is better; "BENCH-END" when done), a window and its
 * message loop, and the procedural pictures the game-like scenes draw (a background, characters
 * with alpha, a message window): no files to ship.
 *
 * Every program takes the case to run as its first argument and an optional scale (a float,
 * default 1: more work per case, for slower or faster machines).
 *
 * Built by emcc (__EMSCRIPTEN__) it is just the timing and the result lines: cpu.c's pure
 * computing cases straight to wasm, for JS engines' shells (tests/bench/js.sh). */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef __EMSCRIPTEN__
#include <emscripten.h>
#define ExitProcess exit
static __inline__ double now_ms(void) { return emscripten_get_now(); }
#else
#include <windows.h>

static LARGE_INTEGER bench_freq_;

static __inline__ double now_ms(void)
{
    LARGE_INTEGER t;
    if (!bench_freq_.QuadPart) QueryPerformanceFrequency(&bench_freq_);
    QueryPerformanceCounter(&t);
    return (double)t.QuadPart * 1000.0 / (double)bench_freq_.QuadPart;
}
#endif

static __inline__ void bench_report(const char *name, double ms)
{
    printf("BENCH %s %.4f\n", name, ms);
    fflush(stdout);
}

/* a checksum line (not a metric: run.mjs shows it when it differs between builds) */
static __inline__ void bench_check(const char *name, unsigned v)
{
    printf("BENCH-CHECK %s %08x\n", name, v);
    fflush(stdout);
}

static __inline__ void bench_end(void)
{
    printf("BENCH-END\n");
    fflush(stdout);
}

static __inline__ void bench_fail(const char *what)
{
    printf("BENCH-FAIL %s\n", what);
    fflush(stdout);
    ExitProcess(1);
}

#ifndef __EMSCRIPTEN__
#define BCHECK(x) do { HRESULT hr_ = (x); if (FAILED(hr_)) { char m_[256]; snprintf(m_, sizeof(m_), "%s -> %#lx", #x, (unsigned long)hr_); bench_fail(m_); } } while (0)

#endif

/* argv[1] and argv[2] of the command line (the case name, the scale) */
static const char *bench_case = "";
static double bench_scale = 1;

#ifdef __EMSCRIPTEN__
static __inline__ void bench_args_main(int argc, char **argv)
{
    if (argc > 1) bench_case = argv[1];
    if (argc > 2) bench_scale = atof(argv[2]) > 0 ? atof(argv[2]) : 1;
}
#else
static __inline__ void bench_args(void)
{
    static char buf[256];
    int n = 0;
    char *p = GetCommandLineA(), *w[3] = { 0 };
    lstrcpynA(buf, p, sizeof(buf));
    p = buf;
    while (*p && n < 3)
    {
        while (*p == ' ') p++;
        if (!*p) break;
        if (*p == '"')
        {
            w[n++] = ++p;
            while (*p && *p != '"') p++;
        }
        else
        {
            w[n++] = p;
            while (*p && *p != ' ') p++;
        }
        if (*p) *p++ = 0;
    }
    if (w[1]) bench_case = w[1];
    if (w[2]) bench_scale = atof(w[2]) > 0 ? atof(w[2]) : 1;
}

#endif

static __inline__ int bench_is(const char *name)
{
    return !bench_case[0] || !strcmp(bench_case, "all") || !strcmp(bench_case, name);
}

static unsigned bench_rng = 12345;
static __inline__ unsigned rnd(void)
{
    bench_rng = bench_rng * 1103515245u + 12345u;
    return bench_rng >> 8;
}

static __inline__ unsigned fnv(const void *p, size_t n, unsigned h)
{
    const unsigned char *b = (const unsigned char *)p;
    while (n--) h = (h ^ *b++) * 16777619u;
    return h;
}

#define SCREEN_W 1280
#define SCREEN_H 720

#ifndef __EMSCRIPTEN__
/* ---- windows ---- */

static LRESULT CALLBACK bench_wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_DESTROY) PostQuitMessage(0);
    return DefWindowProcA(hwnd, msg, wp, lp);
}

/* a visible window whose client area is w x h */
static __inline__ HWND bench_window(const char *title, int w, int h, WNDPROC proc)
{
    WNDCLASSA wc = { 0 };
    RECT r = { 0, 0, w, h };
    HWND hwnd;
    wc.lpfnWndProc = proc ? proc : bench_wndproc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.hCursor = LoadCursorA(NULL, (LPCSTR)IDC_ARROW);
    wc.lpszClassName = title;
    RegisterClassA(&wc);
    AdjustWindowRect(&r, WS_OVERLAPPEDWINDOW, FALSE);
    hwnd = CreateWindowA(title, title, WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0, r.right - r.left, r.bottom - r.top, 0, 0,
                         wc.hInstance, 0);
    if (!hwnd) bench_fail("CreateWindow");
    return hwnd;
}

/* the window's messages; 0 after WM_QUIT */
static __inline__ int bench_pump(void)
{
    MSG msg;
    while (PeekMessageA(&msg, 0, 0, 0, PM_REMOVE))
    {
        if (msg.message == WM_QUIT) return 0;
        TranslateMessage(&msg);
        DispatchMessageA(&msg);
    }
    return 1;
}

/* ---- what the frames drew ----
 * bench_frames asks for a frame to be read back (bench_capturing()); the program's present path
 * then reads the picture it shows (GetDIBits, Lock, GetRenderTargetData, a staging copy,
 * glReadPixels) and hands it to bench_frame_pixels. The last warm-up frame is a fixed one, so its
 * hash is a checksum ("BENCH-CHECK <case>.frame"): the build drew what the previous one drew. One
 * more read back ends the timed frames, so they include the GPU finishing them. */
static int bench_capture_, bench_captured_n_;
static unsigned bench_captured_;

static __inline__ int bench_capturing(void) { return bench_capture_; }

/* w x h 32-bit pixels, `pitch` bytes per row; the alpha byte is ignored (X8R8G8B8) */
static __inline__ void bench_frame_pixels(const void *p, int w, int h, int pitch)
{
    unsigned hash = 2166136261u, first = *(const unsigned *)p & 0xffffff, same = 1;
    int x, y;
    for (y = 0; y < h; y++)
    {
        const unsigned *row = (const unsigned *)((const char *)p + (size_t)y * pitch);
        for (x = 0; x < w; x++)
        {
            unsigned v = row[x] & 0xffffff;
            same &= v == first;
            hash = (hash ^ v) * 16777619u;
        }
    }
    if (same) bench_fail("the frame read back is all one color: nothing was drawn");
    bench_captured_ = hash;
    bench_captured_n_++;
    bench_capture_ = 0;
}

static __inline__ void bench_pump_or_exit(void)
{
    if (!bench_pump()) ExitProcess(0);
}

/* Runs frame(i) `warm` times, then at least `n` times timed: reports ms per frame. The frames are as
 * fast as the guest and the page can make them (run.mjs sets IPPAI_NOPACE and unlimits the
 * page's frame rate). */
static __inline__ void bench_frames(const char *name, int warm, int n, void (*frame)(int))
{
    char check[64];
    double t0;
    int i, got;
    n = (int)(n * bench_scale);
    if (n < 1) n = 1;
    if (warm < 1) warm = 1;
    for (i = 0; i < warm; i++)
    {
        bench_pump_or_exit();
        bench_capture_ = i == warm - 1;
        frame(i);
    }
    if (!bench_captured_n_) bench_fail("the frame was not read back");
    snprintf(check, sizeof(check), "%s.frame", name);
    bench_check(check, bench_captured_);
    /* (at least n frames and 2 s: short runs of fast cases are mostly noise) */
    got = bench_captured_n_;
    /* (run.mjs reads the page's GPU time, timestamp-query, at the mark and after the report: the
     * pauses let the time of the frames before reach it, and keep the next frames out) */
    printf("BENCH-MARK %s\n", name);
    fflush(stdout);
    Sleep(200);
    t0 = now_ms();
    for (i = 0; i < n || now_ms() - t0 < 2000 * bench_scale; i++)
    {
        bench_pump_or_exit();
        bench_capture_ = 0;
        frame(warm + i);
    }
    /* (the last one is read back: the GPU's queue is done when the clock stops) */
    bench_pump_or_exit();
    bench_capture_ = 1;
    frame(warm + i++);
    bench_report(name, (now_ms() - t0) / i);
    if (bench_captured_n_ == got) bench_fail("the last frame was not read back");
    printf("BENCH-FRAMES %s %d\n", name, i);
    fflush(stdout);
    Sleep(200);
}

#endif

/* ---- pictures (32-bit BGRA, top-down) ---- */

/* a background: sky gradient, hills, noise */
static __inline__ void pic_background(unsigned *p, int w, int h, int seed)
{
    int x, y;
    for (y = 0; y < h; y++)
        for (x = 0; x < w; x++)
        {
            int hill = h * 2 / 3 + (int)(40 * ((x * (seed + 3) / 97 % 64) - 32) / 32);
            unsigned r, g, b, n = (x * 7919u + y * 104729u + seed * 31u) * 2654435761u >> 28;
            if (y < hill)
            {
                r = 40 + y * 120 / h + seed * 30 % 60;
                g = 80 + y * 100 / h;
                b = 200 - y * 60 / h;
            }
            else
            {
                r = 30 + n;
                g = 120 + (x + seed * 50) % 40 + n;
                b = 40 + n;
            }
            p[y * w + x] = 0xff000000u | (r & 255) << 16 | (g & 255) << 8 | (b & 255);
        }
}

/* a character: an opaque ellipse body with a soft (alpha) edge on transparent black;
 * premultiplied = colors multiplied by alpha */
static __inline__ void pic_character(unsigned *p, int w, int h, int hue, int premultiplied)
{
    int x, y;
    for (y = 0; y < h; y++)
        for (x = 0; x < w; x++)
        {
            float dx = (x - w / 2) / (w * 0.42f), dy = (y - h * 0.55f) / (h * 0.47f), d = dx * dx + dy * dy;
            float hd = (x - w / 2) * (x - w / 2) / (w * 0.2f * w * 0.2f) + (y - h * 0.18f) * (y - h * 0.18f) / (h * 0.12f * h * 0.12f);
            unsigned a, r, g, b;
            if (hd < d) d = hd;
            a = d < 0.9f ? 255 : d < 1 ? (unsigned)((1 - d) * 2550) : 0;
            r = (unsigned)(128 + 100 * ((hue >> 0) & 1) + (y * 27 / h)) & 255;
            g = (unsigned)(128 + 100 * ((hue >> 1) & 1) - (x * 30 / w)) & 255;
            b = (unsigned)(128 + 100 * ((hue >> 2) & 1)) & 255;
            if (premultiplied) r = r * a / 255, g = g * a / 255, b = b * a / 255;
            p[y * w + x] = a << 24 | r << 16 | g << 8 | b;
        }
}

/* a message window: dark translucent panel with a lighter border */
static __inline__ void pic_window(unsigned *p, int w, int h, int premultiplied)
{
    int x, y;
    for (y = 0; y < h; y++)
        for (x = 0; x < w; x++)
        {
            int edge = x < 4 || y < 4 || x >= w - 4 || y >= h - 4;
            unsigned a = edge ? 230 : 170, c = edge ? 200 : 20 + y * 30 / h;
            if (premultiplied) c = c * a / 255;
            p[y * w + x] = a << 24 | c << 16 | c << 8 | (premultiplied ? (c + 10) * a / 255 : c + 10);
        }
}

/* a small particle (snow, petals, light): radial alpha */
static __inline__ void pic_particle(unsigned *p, int w, int h, int premultiplied)
{
    int x, y;
    for (y = 0; y < h; y++)
        for (x = 0; x < w; x++)
        {
            float dx = (x - w / 2 + 0.5f) / (w / 2), dy = (y - h / 2 + 0.5f) / (h / 2), d = dx * dx + dy * dy;
            unsigned a = d >= 1 ? 0 : (unsigned)((1 - d) * 255), c = premultiplied ? a : 255;
            p[y * w + x] = a << 24 | c << 16 | c << 8 | c;
        }
}

/* A movie frame for the upload cases: moving bars, changes every frame */
static __inline__ void pic_movie(unsigned *p, int w, int h, int t)
{
    int x, y;
    for (y = 0; y < h; y++)
    {
        unsigned *row = p + y * w;
        for (x = 0; x < w; x++) row[x] = 0xff000000u | ((x + t * 8) & 255) << 16 | ((y + t * 3) & 255) << 8 | ((x ^ y) & 255);
    }
}

static __inline__ unsigned *pic_alloc(int w, int h)
{
    unsigned *p = (unsigned *)malloc((size_t)w * h * 4);
    if (!p) bench_fail("malloc");
    return p;
}

/* The scenario text a message window shows, one line per frame turn (Japanese, as in the games) */
#ifndef __EMSCRIPTEN__
static const WCHAR *bench_lines[] __attribute__((unused)) = {
    L"「おはよう、今日もいい天気だね。」",
    L"彼女はそう言って、窓の外を眺めた。空はどこまでも青い。",
    L"「放課後、少し付き合ってくれない？」",
    L"The quick brown fox jumps over the lazy dog. 0123456789",
};
#define BENCH_LINES (sizeof(bench_lines) / sizeof(bench_lines[0]))
#endif
