/* Game-like CPU work on sake's x86 JIT and its Win32 layer: what VN engines spend their CPU on
 * between frames. Each case reports its time in ms (and a checksum line: the same on every build
 * and on Windows, or the JIT computed something else).
 *
 *   script    a bytecode interpreter running a scenario (switch dispatch, variables, strings)
 *   huffman   canonical Huffman decoding (the inner loop of inflate / PNG / ogg headers)
 *   unfilter  PNG Paeth unfiltering of a 1280x720 RGBA picture
 *   yuv       YUV 4:2:0 to BGRA, 1280x720 (movie playback without a hardware decoder)
 *   mix       16 voices of 16-bit audio resampled and mixed with volume and clipping
 *   compose   a 1280x720 scene in software: background, 3 characters, message window (KiriKiri style)
 *   compose_sse  the same with SSE2
 *   transform x87 double math: 2D/3D transforms of vertices (old engines' sprite math)
 *   heap      HeapAlloc / HeapFree and malloc / free of mixed sizes
 *   strings   sprintf, strlen, strcmp, Shift_JIS <-> UTF-16 (MultiByteToWideChar 932)
 *   api       cheap Win32 calls in a loop (GetTickCount, timeGetTime, QPC, PeekMessage, TLS)
 *   sync      critical sections, Interlocked*, event ping-pong between two threads
 *   threads   4 threads hashing at once (does the JIT scale?)
 *   file      a 16 MB archive in %TEMP%: random 64 KB reads (SetFilePointer / ReadFile), mapped reads
 *
 * Built straight to wasm (tests/bench/jsc.sh: no Win32 there), it runs the cases up to transform.
 *
 * Build: i686-w64-mingw32-gcc -O2 -msse2 cpu.c -o cpu.exe -lwinmm */
#include "bench.h"
#include <emmintrin.h>
#include <math.h>
#ifndef __EMSCRIPTEN__
#include <mmsystem.h>
#endif

static unsigned char *big_a, *big_b;

/* ---- script: a tiny VN scenario VM ---- */
enum { OP_PUSH, OP_LOAD, OP_STORE, OP_ADD, OP_SUB, OP_LT, OP_JZ, OP_JMP, OP_TEXT, OP_CALL, OP_RET, OP_MOD, OP_END };

static int vm_vars[64];
static char vm_text[512];
static unsigned vm_hash;

static void vm_say(int n)
{
    /* the engine's text command: formats a line and hashes its characters (the glyph lookups) */
    int len = sprintf(vm_text, "[%d] \x82\xb1\x82\xf1\x82\xc9\x82\xbf\x82\xcd %d", n, vm_vars[1]);
    vm_hash = fnv(vm_text, len, vm_hash);
}

static int vm_run(const int *code)
{
    int stack[64], sp = 0, pc = 0, rs[16], rp = 0, steps = 0;
    for (;;)
    {
        int op = code[pc++];
        steps++;
        switch (op)
        {
        case OP_PUSH: stack[sp++] = code[pc++]; break;
        case OP_LOAD: stack[sp++] = vm_vars[code[pc++]]; break;
        case OP_STORE: vm_vars[code[pc++]] = stack[--sp]; break;
        case OP_ADD: sp--; stack[sp - 1] += stack[sp]; break;
        case OP_SUB: sp--; stack[sp - 1] -= stack[sp]; break;
        case OP_MOD: sp--; stack[sp - 1] = stack[sp] ? stack[sp - 1] % stack[sp] : 0; break;
        case OP_LT: sp--; stack[sp - 1] = stack[sp - 1] < stack[sp]; break;
        case OP_JZ: pc = stack[--sp] ? pc + 1 : code[pc]; break;
        case OP_JMP: pc = code[pc]; break;
        case OP_TEXT: vm_say(stack[--sp]); break;
        case OP_CALL: rs[rp++] = pc + 1; pc = code[pc]; break;
        case OP_RET: pc = rs[--rp]; break;
        default: return steps;
        }
    }
}

static void case_script(void)
{
    /* for (i = 0; i < N; i++) { v1 += i % 7; if (i % 16 == 0) text(i); call f } ; f: v2 = v2 + v1 - 3 */
    int n = (int)(400000 * bench_scale);
    int code[] = {
        OP_PUSH, 0, OP_STORE, 0,                            /* 0: i = 0 */
        OP_LOAD, 0, OP_PUSH, n, OP_LT, OP_JZ, 48,           /* 4: while i < n */
        OP_LOAD, 1, OP_LOAD, 0, OP_PUSH, 7, OP_MOD, OP_ADD, OP_STORE, 1, /* 11: v1 += i % 7 */
        OP_LOAD, 0, OP_PUSH, 16, OP_MOD, OP_JZ, 30, OP_JMP, 33, /* 21: if (i % 16 == 0) */
        OP_LOAD, 0, OP_TEXT,                                /* 30: text(i) */
        OP_CALL, 49,                                        /* 33: f() */
        OP_LOAD, 0, OP_PUSH, 1, OP_ADD, OP_STORE, 0, OP_JMP, 4, /* 35: i++ */
        0, 0, 0, 0,                                         /* 44 */
        OP_END,                                             /* 48 */
        OP_LOAD, 2, OP_LOAD, 1, OP_ADD, OP_PUSH, 3, OP_SUB, OP_STORE, 2, OP_RET, /* 49: f */
    };
    double t0;
    int steps;
    memset(vm_vars, 0, sizeof(vm_vars));
    vm_hash = 2166136261u;
    t0 = now_ms();
    steps = vm_run(code);
    bench_report("script", now_ms() - t0);
    bench_check("script", vm_hash ^ vm_vars[2] ^ steps);
}

/* ---- huffman: canonical codes (length-limited 15 bits), table + bit-reader decode ---- */
static void case_huffman(void)
{
    enum { N = 1 << 20 };
    static unsigned short code[256];
    static unsigned char len[256], sym_of[1 << 11], len_of[1 << 11];
    static unsigned char out[N];
    unsigned freq[256] = { 0 }, *bits;
    unsigned i, nbits = 0, r, reps = (unsigned)(6 * bench_scale) + 1, check = 0;
    double t0;
    /* a skewed source (letters common, the rest rare) and code lengths from its frequencies (by rank) */
    for (i = 0; i < N; i++) big_a[i] = (unsigned char)((rnd() % 100 < 85) ? 'a' + rnd() % 26 : rnd() % 256);
    for (i = 0; i < N; i++) freq[big_a[i]]++;
    for (i = 0; i < 256; i++)
    {
        unsigned rank = 0, k;
        for (k = 0; k < 256; k++) rank += freq[k] > freq[i] || (freq[k] == freq[i] && k < i);
        len[i] = rank < 2 ? 3 : rank < 5 ? 4 : rank < 12 ? 5 : rank < 20 ? 6 : rank < 40 ? 8 : 11;
    }
    {
        /* canonical code assignment (Kraft sum <= 1 for the lengths above) */
        unsigned next = 0, l, s;
        for (l = 1; l <= 11; l++)
        {
            for (s = 0; s < 256; s++)
                if (len[s] == l) code[s] = (unsigned short)next++;
            next <<= 1;
        }
    }
    for (i = 0; i < (1 << 11); i++) len_of[i] = 0;
    for (i = 0; i < 256; i++)
    {
        unsigned fill = 11 - len[i], k, base = (unsigned)code[i] << fill;
        for (k = 0; k < (1u << fill); k++) sym_of[base + k] = (unsigned char)i, len_of[base + k] = len[i];
    }
    bits = (unsigned *)big_b;
    memset(bits, 0, N * 2);
    for (i = 0; i < N; i++)
    {
        unsigned c = code[big_a[i]], l = len[big_a[i]], k;
        for (k = 0; k < l; k++, nbits++)
            if (c >> (l - 1 - k) & 1) bits[nbits >> 5] |= 0x80000000u >> (nbits & 31);
    }
    t0 = now_ms();
    for (r = 0; r < reps; r++)
    {
        unsigned pos = 0, o = 0;
        while (o < N)
        {
            unsigned w = pos >> 5, sh = pos & 31;
            unsigned peek = (bits[w] << sh | (sh ? bits[w + 1] >> (32 - sh) : 0)) >> 21;
            out[o++] = sym_of[peek];
            pos += len_of[peek];
        }
        check ^= fnv(out, N, 2166136261u) + r;
    }
    bench_report("huffman", now_ms() - t0);
    bench_check("huffman", check ^ (memcmp(out, big_a, N) ? 0xdead : 0));
}

/* ---- PNG Paeth unfilter ---- */
static void case_unfilter(void)
{
    const int w = 1280, h = 720, bpp = 4, stride = w * bpp;
    unsigned r, reps = (unsigned)(10 * bench_scale) + 1, check = 0;
    unsigned char *img = big_a;
    double t0;
    t0 = now_ms();
    for (r = 0; r < reps; r++)
    {
        int x, y;
        for (y = 0; y < h * stride; y++) img[y] = (unsigned char)(y * 7 + r);
        for (y = 1; y < h; y++)
        {
            unsigned char *row = img + y * stride, *up = row - stride;
            for (x = 0; x < stride; x++)
            {
                int a = x >= bpp ? row[x - bpp] : 0, b = up[x], c = x >= bpp ? up[x - bpp] : 0;
                int p = a + b - c, pa = abs(p - a), pb = abs(p - b), pc = abs(p - c);
                row[x] = (unsigned char)(row[x] + (pa <= pb && pa <= pc ? a : pb <= pc ? b : c));
            }
        }
        check = fnv(img + (h - 1) * stride, stride, check);
    }
    bench_report("unfilter", now_ms() - t0);
    bench_check("unfilter", check);
}

/* ---- YUV 4:2:0 -> BGRA ---- */
static void case_yuv(void)
{
    const int w = 1280, h = 720;
    unsigned char *Y = big_a, *U = Y + w * h, *V = U + w * h / 4;
    unsigned *dst = (unsigned *)big_b, r, reps = (unsigned)(10 * bench_scale) + 1, check = 0;
    double t0;
    int i;
    for (i = 0; i < w * h * 3 / 2; i++) Y[i] = (unsigned char)(rnd());
    t0 = now_ms();
    for (r = 0; r < reps; r++)
    {
        int x, y;
        for (y = 0; y < h; y++)
            for (x = 0; x < w; x++)
            {
                int c = Y[y * w + x] - 16, d = U[(y / 2) * (w / 2) + x / 2] - 128, e = V[(y / 2) * (w / 2) + x / 2] - 128;
                int R = (298 * c + 409 * e + 128) >> 8, G = (298 * c - 100 * d - 208 * e + 128) >> 8, B = (298 * c + 516 * d + 128) >> 8;
                R = R < 0 ? 0 : R > 255 ? 255 : R;
                G = G < 0 ? 0 : G > 255 ? 255 : G;
                B = B < 0 ? 0 : B > 255 ? 255 : B;
                dst[y * w + x] = 0xff000000u | R << 16 | G << 8 | B;
            }
        check = fnv(dst + r * 97, 4096, check);
    }
    bench_report("yuv", now_ms() - t0);
    bench_check("yuv", check);
}

/* ---- audio mix ---- */
static void case_mix(void)
{
    enum { VOICES = 16, SRC = 22050 * 2, NOUT = 4096 };
    static short src[VOICES][SRC];
    static int acc[NOUT * 2];
    static short out[NOUT * 2];
    unsigned pos[VOICES], step[VOICES], vol[VOICES], blocks = (unsigned)(800 * bench_scale) + 1, b, check = 0;
    double t0;
    int v, i;
    for (v = 0; v < VOICES; v++)
    {
        for (i = 0; i < SRC; i++) src[v][i] = (short)(sin(i * (0.01 + v * 0.003)) * 12000);
        pos[v] = 0;
        step[v] = 32768 + v * 2048; /* 16.16: 0.5x .. 1x (22.05 kHz voices into 44.1 kHz) */
        vol[v] = 64 + v * 8;
    }
    t0 = now_ms();
    for (b = 0; b < blocks; b++)
    {
        memset(acc, 0, sizeof(acc));
        for (v = 0; v < VOICES; v++)
        {
            unsigned p = pos[v];
            for (i = 0; i < NOUT; i++)
            {
                unsigned k = (p >> 16) % (SRC - 1), f = p & 0xffff;
                int s = (src[v][k] * (int)(65536 - f) + src[v][k + 1] * (int)f) >> 16;
                s = s * (int)vol[v] >> 8;
                acc[2 * i] += s * (v & 1 ? 3 : 1) >> 1;
                acc[2 * i + 1] += s * (v & 1 ? 1 : 3) >> 1;
                p += step[v];
            }
            pos[v] = p;
        }
        for (i = 0; i < NOUT * 2; i++) out[i] = (short)(acc[i] > 32767 ? 32767 : acc[i] < -32768 ? -32768 : acc[i]);
        check = fnv(out, 64, check);
    }
    bench_report("mix", now_ms() - t0);
    bench_check("mix", check);
}

/* ---- software composition (the CPU side of GDI-drawing engines) ---- */
static unsigned *bg, *chars[3], *win, *screen;

static void compose_pictures(void)
{
    int i;
    if (bg) return;
    bg = pic_alloc(SCREEN_W, SCREEN_H);
    pic_background(bg, SCREEN_W, SCREEN_H, 1);
    for (i = 0; i < 3; i++)
    {
        chars[i] = pic_alloc(400, 640);
        pic_character(chars[i], 400, 640, i + 1, 0);
    }
    win = pic_alloc(1200, 200);
    pic_window(win, 1200, 200, 0);
    screen = pic_alloc(SCREEN_W, SCREEN_H);
}

static void blend_c(unsigned *dst, int dw, const unsigned *src, int w, int h, int x0, int y0)
{
    int x, y;
    for (y = 0; y < h; y++)
    {
        unsigned *d = dst + (y + y0) * dw + x0;
        const unsigned *s = src + y * w;
        for (x = 0; x < w; x++)
        {
            unsigned sp = s[x], a = sp >> 24, dp;
            if (!a) continue;
            if (a == 255) { d[x] = sp; continue; }
            dp = d[x];
            d[x] = 0xff000000u | ((((sp & 0xff00ff) * a + (dp & 0xff00ff) * (255 - a)) >> 8) & 0xff00ff) |
                   ((((sp & 0xff00) * a + (dp & 0xff00) * (255 - a)) >> 8) & 0xff00);
        }
    }
}

static void blend_sse(unsigned *dst, int dw, const unsigned *src, int w, int h, int x0, int y0)
{
    const __m128i zero = _mm_setzero_si128(), ff = _mm_set1_epi16(255);
    int x, y;
    for (y = 0; y < h; y++)
    {
        unsigned *d = dst + (y + y0) * dw + x0;
        const unsigned *s = src + y * w;
        for (x = 0; x + 4 <= w; x += 4)
        {
            __m128i sp = _mm_loadu_si128((const __m128i *)(s + x)), dp = _mm_loadu_si128((__m128i *)(d + x));
            __m128i slo = _mm_unpacklo_epi8(sp, zero), shi = _mm_unpackhi_epi8(sp, zero);
            __m128i dlo = _mm_unpacklo_epi8(dp, zero), dhi = _mm_unpackhi_epi8(dp, zero);
            __m128i alo = _mm_shufflehi_epi16(_mm_shufflelo_epi16(slo, 0xff), 0xff);
            __m128i ahi = _mm_shufflehi_epi16(_mm_shufflelo_epi16(shi, 0xff), 0xff);
            __m128i rlo = _mm_srli_epi16(_mm_add_epi16(_mm_mullo_epi16(slo, alo), _mm_mullo_epi16(dlo, _mm_sub_epi16(ff, alo))), 8);
            __m128i rhi = _mm_srli_epi16(_mm_add_epi16(_mm_mullo_epi16(shi, ahi), _mm_mullo_epi16(dhi, _mm_sub_epi16(ff, ahi))), 8);
            _mm_storeu_si128((__m128i *)(d + x), _mm_or_si128(_mm_packus_epi16(rlo, rhi), _mm_set1_epi32((int)0xff000000)));
        }
    }
}

static void compose(int frame, void (*blend)(unsigned *, int, const unsigned *, int, int, int, int))
{
    int i;
    memcpy(screen, bg, SCREEN_W * SCREEN_H * 4);
    for (i = 0; i < 3; i++) blend(screen, SCREEN_W, chars[i], 400, 640, 40 + i * 410, 80 - (frame + i * 7) % 16);
    blend(screen, SCREEN_W, win, 1200, 200, 40, 500);
}

static void case_compose(const char *name, void (*blend)(unsigned *, int, const unsigned *, int, int, int, int))
{
    int f, n = (int)(30 * bench_scale) + 1;
    double t0;
    compose_pictures();
    t0 = now_ms();
    for (f = 0; f < n; f++) compose(f, blend);
    bench_report(name, (now_ms() - t0) / n);
    bench_check(name, fnv(screen, SCREEN_W * SCREEN_H * 4, 2166136261u));
}

/* ---- x87 transforms ---- */
static void case_transform(void)
{
    enum { N = 20000 };
    static double vx[N], vy[N], vz[N], ox[N], oy[N];
    unsigned r, reps = (unsigned)(60 * bench_scale) + 1;
    double t0, sum = 0;
    int i;
    for (i = 0; i < N; i++) vx[i] = i % 100 - 50, vy[i] = i % 37 - 18, vz[i] = i % 11 + 1;
    t0 = now_ms();
    for (r = 0; r < reps; r++)
    {
        double a = r * 0.01, c = cos(a), s = sin(a), zoom = 1.0 + 0.1 * sin(a * 3);
        for (i = 0; i < N; i++)
        {
            double x = vx[i] * c - vz[i] * s, z = vx[i] * s + vz[i] * c + 200, y = vy[i];
            ox[i] = 640 + x * zoom * 300 / z;
            oy[i] = 360 + y * zoom * 300 / z;
        }
        sum += ox[r % N] + oy[(r * 7) % N] + sqrt(fabs(ox[N / 2]));
    }
    bench_report("transform", now_ms() - t0);
    bench_check("transform", (unsigned)(long long)(sum * 1000));
}

#ifndef __EMSCRIPTEN__
/* ---- heap ---- */
static unsigned heap_churn(int crt, unsigned n)
{
    enum { SLOTS = 4096 };
    static void *slot[SLOTS];
    HANDLE heap = GetProcessHeap();
    unsigned i, check = 0;
    for (i = 0; i < n; i++)
    {
        unsigned k = rnd() % SLOTS, size = (rnd() % 8 == 0) ? 1024 + rnd() % 16384 : 8 + rnd() % 120;
        if (slot[k])
        {
            check += ((unsigned char *)slot[k])[0];
            if (crt) free(slot[k]);
            else HeapFree(heap, 0, slot[k]);
        }
        slot[k] = crt ? malloc(size) : HeapAlloc(heap, 0, size);
        ((unsigned char *)slot[k])[0] = (unsigned char)i;
    }
    for (i = 0; i < SLOTS; i++)
    {
        if (crt) free(slot[i]);
        else if (slot[i]) HeapFree(heap, 0, slot[i]);
        slot[i] = NULL;
    }
    return check;
}

static void case_heap(void)
{
    unsigned n = (unsigned)(300000 * bench_scale) + 1, check;
    double t0 = now_ms();
    check = heap_churn(0, n);
    bench_report("heap_win32", now_ms() - t0);
    t0 = now_ms();
    check += heap_churn(1, n);
    bench_report("heap_crt", now_ms() - t0);
    bench_check("heap", check);
}

/* ---- strings ---- */
static void case_strings(void)
{
    static const char sjis[] = "\x82\xb1\x82\xf1\x82\xc9\x82\xbf\x82\xcd\x81\x41\x90\xa2\x8a\x45\x81\x42 Hello, world 0123";
    WCHAR wide[128];
    char back[256], buf[256];
    unsigned i, n = (unsigned)(60000 * bench_scale) + 1, check = 0;
    double t0 = now_ms();
    for (i = 0; i < n; i++)
    {
        int wl = MultiByteToWideChar(932, 0, sjis, -1, wide, 128);
        int bl = WideCharToMultiByte(932, 0, wide, wl, back, sizeof(back), NULL, NULL);
        int l = sprintf(buf, "%s/%d/%08x/%.2f", "scene", (int)i, i * 2654435761u, i * 0.5);
        check += wl + bl + l + (unsigned)strlen(buf) + (strcmp(back, sjis) == 0) + (unsigned)lstrlenW(wide);
        check = check * 31 + (unsigned char)buf[l / 2];
    }
    bench_report("strings", now_ms() - t0);
    bench_check("strings", check);
}

/* ---- api ---- */
static void case_api(void)
{
    unsigned i, n = (unsigned)(200000 * bench_scale) + 1, check = 0;
    DWORD tls = TlsAlloc();
    LARGE_INTEGER q;
    MSG msg;
    double t0 = now_ms();
    for (i = 0; i < n; i++)
    {
        check += GetTickCount() & 1;
        check += timeGetTime() & 1;
        QueryPerformanceCounter(&q);
        check += (unsigned)q.LowPart & 1;
        TlsSetValue(tls, (void *)(size_t)i);
        check += (unsigned)(size_t)TlsGetValue(tls) == i;
        if (!(i & 15)) check += PeekMessageA(&msg, 0, 0, 0, PM_NOREMOVE);
        check += GetCurrentThreadId() != 0;
    }
    TlsFree(tls);
    bench_report("api", now_ms() - t0);
    bench_check("api", n + 0 * check); /* (the results depend on the clock: only the count) */
}

/* ---- sync ---- */
static HANDLE ping, pong;
static volatile LONG rounds_left;

static DWORD WINAPI ponger(void *arg)
{
    (void)arg;
    for (;;)
    {
        WaitForSingleObject(ping, INFINITE);
        if (rounds_left <= 0) { SetEvent(pong); return 0; }
        InterlockedDecrement(&rounds_left);
        SetEvent(pong);
    }
}

static void case_sync(void)
{
    CRITICAL_SECTION cs;
    volatile LONG counter = 0;
    unsigned i, n = (unsigned)(400000 * bench_scale) + 1, rounds = (unsigned)(4000 * bench_scale) + 1;
    HANDLE th;
    double t0 = now_ms(), t1;
    InitializeCriticalSection(&cs);
    for (i = 0; i < n; i++)
    {
        EnterCriticalSection(&cs);
        counter++;
        LeaveCriticalSection(&cs);
        InterlockedIncrement(&counter);
        InterlockedCompareExchange(&counter, counter, counter);
    }
    DeleteCriticalSection(&cs);
    t1 = now_ms();
    bench_report("sync_lock", t1 - t0);
    ping = CreateEventA(NULL, FALSE, FALSE, NULL);
    pong = CreateEventA(NULL, FALSE, FALSE, NULL);
    rounds_left = rounds;
    th = CreateThread(NULL, 0, ponger, NULL, 0, NULL);
    t0 = now_ms();
    for (;;)
    {
        SetEvent(ping);
        WaitForSingleObject(pong, INFINITE);
        if (rounds_left <= 0) break;
    }
    SetEvent(ping);
    WaitForSingleObject(th, INFINITE);
    bench_report("sync_pingpong", now_ms() - t0);
    bench_check("sync", (unsigned)counter);
    CloseHandle(th);
}

/* ---- threads ---- */
static DWORD WINAPI hasher(void *arg)
{
    unsigned *res = arg, h = 2166136261u, i, n = (unsigned)(30000000 * bench_scale);
    for (i = 0; i < n; i++) h = (h ^ (i & 255)) * 16777619u;
    *res = h;
    return 0;
}

static void case_threads(void)
{
    HANDLE th[4];
    unsigned res[4], check = 0, one;
    double t0 = now_ms(), t1;
    int i;
    hasher(&one);
    t1 = now_ms();
    bench_report("threads_1", t1 - t0);
    t0 = now_ms();
    for (i = 0; i < 4; i++) th[i] = CreateThread(NULL, 0, hasher, &res[i], 0, NULL);
    WaitForMultipleObjects(4, th, TRUE, INFINITE);
    bench_report("threads_4", now_ms() - t0);
    for (i = 0; i < 4; i++) check ^= res[i] + i, CloseHandle(th[i]);
    bench_check("threads", check ^ one);
}

/* ---- file ---- */
static void case_file(void)
{
    enum { SIZE = 16 << 20, CHUNK = 64 << 10 };
    char path[MAX_PATH];
    HANDLE f, map;
    DWORD got;
    unsigned i, n = (unsigned)(800 * bench_scale) + 1, check = 0;
    unsigned char *view;
    double t0;
    GetTempPathA(MAX_PATH - 20, path);
    lstrcatA(path, "benchdata.bin");
    for (i = 0; i < SIZE; i++) big_a[i] = (unsigned char)(i * 131 >> 3);
    t0 = now_ms();
    f = CreateFileA(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (f == INVALID_HANDLE_VALUE) bench_fail("CreateFile write");
    for (i = 0; i < SIZE / CHUNK; i++) WriteFile(f, big_a + i * CHUNK, CHUNK, &got, NULL);
    CloseHandle(f);
    bench_report("file_write", now_ms() - t0);
    f = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    if (f == INVALID_HANDLE_VALUE) bench_fail("CreateFile read");
    t0 = now_ms();
    for (i = 0; i < n; i++)
    {
        unsigned off = (rnd() % (SIZE / CHUNK)) * CHUNK + rnd() % 4096;
        if (off + CHUNK > SIZE) off = SIZE - CHUNK;
        SetFilePointer(f, (LONG)off, NULL, FILE_BEGIN);
        if (!ReadFile(f, big_b, CHUNK, &got, NULL) || got != CHUNK) bench_fail("ReadFile");
        check += big_b[0] ^ big_b[CHUNK - 1];
    }
    bench_report("file_read", now_ms() - t0);
    map = CreateFileMappingA(f, NULL, PAGE_READONLY, 0, 0, NULL);
    view = map ? MapViewOfFile(map, FILE_MAP_READ, 0, 0, 0) : NULL;
    if (!view) bench_fail("MapViewOfFile");
    t0 = now_ms();
    for (i = 0; i < n * 4; i++)
    {
        unsigned off = rnd() % (SIZE - 4096);
        check += view[off] + view[off + 4095];
    }
    for (i = 0; i < SIZE; i += 4096) check += view[i];
    bench_report("file_mapped", now_ms() - t0);
    UnmapViewOfFile(view);
    CloseHandle(map);
    CloseHandle(f);
    DeleteFileA(path);
    bench_check("file", check);
}

#endif

int main(int argc, char **argv)
{
#ifdef __EMSCRIPTEN__
    bench_args_main(argc, argv);
#else
    (void)argc, (void)argv;
    bench_args();
#endif
    big_a = malloc(16 << 20);
    big_b = malloc(16 << 20);
    if (!big_a || !big_b) bench_fail("malloc");
    if (bench_is("script")) case_script();
    if (bench_is("huffman")) case_huffman();
    if (bench_is("unfilter")) case_unfilter();
    if (bench_is("yuv")) case_yuv();
    if (bench_is("mix")) case_mix();
    if (bench_is("compose")) case_compose("compose", blend_c);
    if (bench_is("compose_sse")) case_compose("compose_sse", blend_sse);
    if (bench_is("transform")) case_transform();
#ifndef __EMSCRIPTEN__
    if (bench_is("heap")) case_heap();
    if (bench_is("strings")) case_strings();
    if (bench_is("api")) case_api();
    if (bench_is("sync")) case_sync();
    if (bench_is("threads")) case_threads();
    if (bench_is("file")) case_file();
#endif
    bench_end();
    return 0;
}
