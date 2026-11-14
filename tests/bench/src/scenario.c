/* Scenario: a small visual novel played from the title to a save, as players play one (Direct3D
 * 9, 1280x720, paced as games are: no MASU_NOPACE). The harness (tests/bench/scenario.mjs)
 * clicks when the game waits and measures from the click to what the game shows; the game says
 * where it is on its output:
 *
 *   BENCH-STATE <state> <step>   the first frame of a new state was presented: title, line (the
 *                                next line starts typing), scene (a transition ended, its line
 *                                starts), movie, choice, saved, restored, end
 *   BENCH-WAIT <step> [idle|choice]  the line is typed: waiting for a click (idle: the harness
 *                                waits 3 s first and measures the CPU the page uses meanwhile;
 *                                choice: two buttons, the harness takes the second)
 *
 * Its data are one 70 MB archive (scenario.dat from tests/bench/gen-scenario.mjs: pictures and
 * sounds in no useful order, index at the end), read entry by entry when the script needs them:
 * the store's chunks are fetched as a real game makes the page fetch them. Pictures are
 * "Sub"-filtered and unfiltered here (PNG-like work); voices and BGM go to waveOut.
 *
 * At the end it reports its own numbers: boot_to_title (the program's start to the title),
 * frame_p50 / frame_p95 / frame_p99 (frame intervals while something moves: typing, fades),
 * fade_ms (the transitions' mean), movie_p95 and movie_late (frames of the 24 fps movie shown
 * late). "load" as the argument restores the save instead and ends there (BENCH-STATE restored).
 *
 * Build: i686-w64-mingw32-gcc -O2 scenario.c -o scenario.exe -mwindows -ld3d9 -ld3dx9_43 -lgdi32 -lwinmm */
#define COBJMACROS
#include "bench.h"
#include <d3d9.h>
#include <d3dx9.h>
#include <mmsystem.h>

typedef struct { float x, y, z, rhw; DWORD c; float u, v; } V;
#define FVF (D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1)

/* ---- the archive ---- */
typedef struct { char name[24]; unsigned off, size, kind, w, h; } Entry;
static HANDLE arc;
static Entry *ents;
static unsigned nents;
static char dir[MAX_PATH];

static void read_at(unsigned off, void *buf, unsigned n)
{
    DWORD got;
    SetFilePointer(arc, (LONG)off, NULL, FILE_BEGIN);
    if (!ReadFile(arc, buf, n, &got, NULL) || got != n) bench_fail("ReadFile scenario.dat");
}

static void open_archive(void)
{
    char path[MAX_PATH], *slash;
    unsigned head[3];
    GetModuleFileNameA(NULL, dir, sizeof(dir));
    if ((slash = strrchr(dir, '\\'))) slash[1] = 0;
    snprintf(path, sizeof(path), "%sscenario.dat", dir);
    arc = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    if (arc == INVALID_HANDLE_VALUE) bench_fail("no scenario.dat");
    read_at(0, head, sizeof(head));
    if (memcmp(head, "VNBA", 4)) bench_fail("scenario.dat: not an archive");
    nents = head[1];
    ents = (Entry *)malloc(nents * sizeof(Entry));
    read_at(head[2], ents, nents * sizeof(Entry));
}

static const Entry *entry(const char *name)
{
    unsigned i;
    for (i = 0; i < nents; i++)
        if (!strcmp(ents[i].name, name)) return &ents[i];
    bench_fail(name);
    return NULL;
}

static unsigned char *load(const Entry *e)
{
    unsigned char *b = (unsigned char *)malloc(e->size);
    if (!b) bench_fail("malloc");
    read_at(e->off, b, e->size);
    return b;
}

/* ---- Direct3D ---- */
static IDirect3DDevice9 *dev;
static IDirect3DSurface9 *backbuf;
static IDirect3DTexture9 *t_bg, *t_chr[2], *t_win, *t_text, *t_btn, *t_movie, *rt[2];
static IDirect3DPixelShader9 *ps_fade;
static HDC text_dc;
static unsigned *text_bits;

static void quad(V *v, float x, float y, float w, float h, DWORD c)
{
    V q[4] = { { x - 0.5f, y - 0.5f, 0, 1, c, 0, 0 }, { x + w - 0.5f, y - 0.5f, 0, 1, c, 1, 0 },
               { x - 0.5f, y + h - 0.5f, 0, 1, c, 0, 1 }, { x + w - 0.5f, y + h - 0.5f, 0, 1, c, 1, 1 } };
    memcpy(v, q, sizeof(q));
}

static void draw(IDirect3DTexture9 *t, float x, float y, float w, float h, DWORD c)
{
    V v[4];
    quad(v, x, y, w, h, c);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, v, sizeof(V));
}

/* a picture of the archive into a (new, managed) texture: unfiltered while copied */
static IDirect3DTexture9 *picture(const char *name, IDirect3DTexture9 *reuse)
{
    const Entry *e = entry(name);
    unsigned char *b = load(e);
    IDirect3DTexture9 *t = reuse;
    D3DLOCKED_RECT lr;
    unsigned y, i;
    if (!t) BCHECK(IDirect3DDevice9_CreateTexture(dev, e->w, e->h, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t, NULL));
    BCHECK(IDirect3DTexture9_LockRect(t, 0, &lr, NULL, 0));
    for (y = 0; y < e->h; y++)
    {
        unsigned char *d = (unsigned char *)lr.pBits + y * lr.Pitch, *s = b + y * e->w * 4;
        for (i = 0; i < 4; i++) d[i] = s[i];
        for (i = 4; i < e->w * 4; i++) d[i] = (unsigned char)(s[i] + d[i - 4]);
    }
    IDirect3DTexture9_UnlockRect(t, 0);
    free(b);
    return t;
}

static IDirect3DTexture9 *solid(int w, int h, unsigned argb)
{
    IDirect3DTexture9 *t;
    D3DLOCKED_RECT lr;
    int x, y;
    BCHECK(IDirect3DDevice9_CreateTexture(dev, w, h, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t, NULL));
    BCHECK(IDirect3DTexture9_LockRect(t, 0, &lr, NULL, 0));
    for (y = 0; y < h; y++)
        for (x = 0; x < w; x++)
        {
            int edge = x < 3 || y < 3 || x >= w - 3 || y >= h - 3;
            ((unsigned *)((BYTE *)lr.pBits + y * lr.Pitch))[x] = edge ? 0xf0d0d0e0 : argb;
        }
    IDirect3DTexture9_UnlockRect(t, 0);
    return t;
}

/* ---- sound ---- */
static HWAVEOUT bgm_out, voice_out;
static WAVEHDR bgm_hdr, voice_hdr;
static unsigned char *bgm_pcm, *voice_pcm;

static HWAVEOUT open_out(unsigned rate, unsigned channels)
{
    WAVEFORMATEX f = { WAVE_FORMAT_PCM, (WORD)channels, rate, rate * channels * 2, (WORD)(channels * 2), 16, 0 };
    HWAVEOUT h = NULL;
    if (waveOutOpen(&h, WAVE_MAPPER, &f, 0, 0, CALLBACK_NULL) != MMSYSERR_NOERROR) bench_fail("waveOutOpen");
    return h;
}

static void play(HWAVEOUT h, WAVEHDR *hdr, unsigned char **pcm, const char *name)
{
    const Entry *e = entry(name);
    waveOutReset(h);
    if (hdr->dwFlags & WHDR_PREPARED) waveOutUnprepareHeader(h, hdr, sizeof(*hdr));
    free(*pcm);
    *pcm = load(e);
    memset(hdr, 0, sizeof(*hdr));
    hdr->lpData = (LPSTR)*pcm;
    hdr->dwBufferLength = e->size;
    waveOutPrepareHeader(h, hdr, sizeof(*hdr));
    waveOutWrite(h, hdr, sizeof(*hdr));
}

/* ---- the script ---- */
enum { TITLE, SCENE, LINE, MOVIE, CHOICE, SAVE, END };
typedef struct { int kind, bg, c0, f0, c1, f1, voice, bgm, flags; const WCHAR *text; } Step;
#define IDLE 1
static const Step script[] = {
    { TITLE, 0, -1, 0, -1, 0, -1, 0, 0, NULL },
    { SCENE, 1, 0, 0, 1, 0, 0, -1, 0, L"「おはよう、今日もいい天気だね。」" },
    { LINE, 1, 0, 1, 1, 0, 1, -1, 0, L"彼女はそう言って、窓の外を眺めた。空はどこまでも青い。" },
    { LINE, 1, 0, 1, 1, 2, 2, -1, 0, L"「放課後、少し付き合ってくれない？」" },
    { LINE, 1, 0, 2, 1, 2, 3, -1, 0, L"断る理由はなかった。僕は黙ってうなずいた。" },
    { LINE, 1, 0, 3, 1, 1, 4, -1, 0, L"「じゃあ、校門で待ってるから。」" },
    { SCENE, 2, 1, 0, 2, 0, 5, -1, 0, L"放課後の校舎は、昼間とは別の場所のように静かだった。" },
    { LINE, 2, 1, 3, 2, 1, 6, -1, 0, L"「遅いよ。もう帰っちゃおうかと思った。」" },
    { LINE, 2, 1, 2, 2, 3, 7, -1, 0, L"The quick brown fox jumps over the lazy dog. 0123456789" },
    { MOVIE, 2, -1, 0, -1, 0, -1, -1, 0, NULL },
    { SCENE, 3, 0, 2, 2, 2, 8, 1, 0, L"夕焼けが川面に映って、街全体が橙色に染まっていた。" },
    { LINE, 3, 0, 0, 2, 1, 9, -1, 0, L"「ねえ、どっちに行く？」" },
    { CHOICE, 3, 0, 0, 2, 1, -1, -1, 0, NULL },
    /* (bg -1: the one the choice took) */
    { SCENE, -1, 0, 1, 1, 3, 10, -1, 0, L"結局、僕たちは駅前の喫茶店に入った。" },
    { LINE, -1, 0, 2, 1, 3, 11, -1, IDLE, L"窓際の席で、彼女はしばらく何も言わずにカップを眺めていた。" },
    { LINE, -1, 0, 3, 1, 0, 12, -1, 0, L"「また、明日ね。」" },
    { SAVE, -1, 0, 3, 1, 0, -1, -1, 0, NULL },
    { END, 0, -1, 0, -1, 0, -1, -1, 0, NULL },
};
#define STEPS (int)(sizeof(script) / sizeof(script[0]))

static int step, choice, typed, typing, fading, chosen_scene;
static double fade_t0;
static const WCHAR *line;

/* frame intervals while something moves */
static double frames[8192], last_frame;
static int nframes;
static double fade_sum;
static int fades;

static int cmp(const void *a, const void *b)
{
    double x = *(const double *)a, y = *(const double *)b;
    return x < y ? -1 : x > y;
}

static double pct(double *v, int n, double p)
{
    if (!n) return 0;
    qsort(v, n, sizeof(*v), cmp);
    return v[(int)(p * (n - 1) + 0.5)];
}

static void states_2d(void)
{
    IDirect3DDevice9_SetFVF(dev, FVF);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, TRUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_MODULATE);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_ALPHAOP, D3DTOP_MODULATE);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MINFILTER, D3DTEXF_LINEAR);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);
}

/* the line typed so far into the text texture */
static void text_update(void)
{
    int n = typed, len = line ? lstrlenW(line) : 0, y;
    D3DLOCKED_RECT lr;
    memset(text_bits, 0, 1024 * 64 * 4);
    if (line) ExtTextOutW(text_dc, 0, 16, 0, NULL, line, n < len ? n : len, NULL);
    GdiFlush();
    if (SUCCEEDED(IDirect3DTexture9_LockRect(t_text, 0, &lr, NULL, 0)))
    {
        for (y = 0; y < 64; y++)
        {
            unsigned *d = (unsigned *)((BYTE *)lr.pBits + y * lr.Pitch), *s = text_bits + y * 1024;
            int x;
            for (x = 0; x < 1024; x++) d[x] = (s[x] & 0xff) << 24 | 0xffffff;
        }
        IDirect3DTexture9_UnlockRect(t_text, 0);
    }
}

static void scene(int with_text)
{
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H, 0xffffffff);
    if (t_chr[0]) draw(t_chr[0], 120, 80, 400, 640, 0xffffffff);
    if (t_chr[1]) draw(t_chr[1], 760, 80, 400, 640, 0xffffffff);
    draw(t_win, 40, 500, 1200, 200, 0xffffffff);
    if (with_text) draw(t_text, 80, 530, 1024, 64, 0xffffffff);
}

static void present(void)
{
    double t;
    IDirect3DDevice9_EndScene(dev);
    if (bench_capturing())
    {
        static IDirect3DSurface9 *sys;
        D3DLOCKED_RECT lr;
        if (!sys) BCHECK(IDirect3DDevice9_CreateOffscreenPlainSurface(dev, SCREEN_W, SCREEN_H, D3DFMT_X8R8G8B8, D3DPOOL_SYSTEMMEM, &sys, NULL));
        BCHECK(IDirect3DDevice9_GetRenderTargetData(dev, backbuf, sys));
        BCHECK(IDirect3DSurface9_LockRect(sys, &lr, NULL, D3DLOCK_READONLY));
        bench_frame_pixels(lr.pBits, SCREEN_W, SCREEN_H, lr.Pitch);
        IDirect3DSurface9_UnlockRect(sys);
    }
    IDirect3DDevice9_Present(dev, NULL, NULL, NULL, NULL);
    t = now_ms();
    if (last_frame && nframes < 8192) frames[nframes++] = t - last_frame;
    last_frame = t;
}

static void begin(void)
{
    IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
    IDirect3DDevice9_BeginScene(dev);
}

static void state(const char *what)
{
    printf("BENCH-STATE %s %d\n", what, step);
    fflush(stdout);
}

static void checked(const char *name)
{
    char n[64];
    snprintf(n, sizeof(n), "%s.frame", name);
    bench_check(n, bench_captured_);
}

/* the pictures of step s (pictures the previous step showed are kept, as an engine's cache) */
static int shown_bg = -1, shown_c[2] = { -2, -2 }, shown_f[2];
static void load_step(const Step *s)
{
    char name[32];
    int i, c[2] = { s->c0, s->c1 }, f[2] = { s->f0, s->f1 }, bg = s->bg < 0 ? chosen_scene : s->bg;
    if (bg != shown_bg)
    {
        snprintf(name, sizeof(name), "bg%d", bg);
        t_bg = picture(name, t_bg);
        shown_bg = bg;
    }
    for (i = 0; i < 2; i++)
    {
        if (c[i] == shown_c[i] && f[i] == shown_f[i]) continue;
        if (c[i] < 0)
        {
            if (t_chr[i]) IDirect3DTexture9_Release(t_chr[i]);
            t_chr[i] = NULL;
        }
        else
        {
            snprintf(name, sizeof(name), "ch%d_%d", c[i], f[i]);
            t_chr[i] = picture(name, t_chr[i]);
        }
        shown_c[i] = c[i], shown_f[i] = f[i];
    }
    if (s->voice >= 0)
    {
        snprintf(name, sizeof(name), "voice%d", s->voice);
        play(voice_out, &voice_hdr, &voice_pcm, name);
    }
    if (s->bgm >= 0)
    {
        snprintf(name, sizeof(name), "bgm%d", s->bgm);
        play(bgm_out, &bgm_hdr, &bgm_pcm, name);
    }
}

static void into(IDirect3DTexture9 *t, int with_text)
{
    IDirect3DSurface9 *s;
    IDirect3DTexture9_GetSurfaceLevel(t, 0, &s);
    IDirect3DDevice9_SetRenderTarget(dev, 0, s);
    begin();
    scene(with_text);
    IDirect3DDevice9_EndScene(dev);
    IDirect3DDevice9_SetRenderTarget(dev, 0, backbuf);
    IDirect3DSurface9_Release(s);
}

static void movie(void)
{
    double t0, worst[64];
    int k, late = 0, n = 0;
    state("movie");
    t0 = now_ms();
    for (k = 0; k < 48; k++)
    {
        char name[16];
        const Entry *e;
        unsigned char *b;
        D3DLOCKED_RECT lr;
        unsigned y, i;
        double due = t0 + k * 1000.0 / 24, t;
        /* (frames read one by one as they are due, as a movie player does) */
        while ((t = now_ms()) < due) Sleep(1);
        if (t - due > 1000.0 / 24) late++;
        bench_pump();
        snprintf(name, sizeof(name), "mv%02d", k);
        e = entry(name);
        b = load(e);
        if (FAILED(IDirect3DTexture9_LockRect(t_movie, 0, &lr, NULL, D3DLOCK_DISCARD))) bench_fail("LockRect movie");
        for (y = 0; y < e->h; y++)
        {
            unsigned char *d = (unsigned char *)lr.pBits + y * lr.Pitch, *s = b + y * e->w * 4;
            for (i = 0; i < 4; i++) d[i] = s[i];
            for (i = 4; i < e->w * 4; i++) d[i] = (unsigned char)(s[i] + d[i - 4]);
        }
        IDirect3DTexture9_UnlockRect(t_movie, 0);
        free(b);
        begin();
        draw(t_movie, 0, 0, SCREEN_W, SCREEN_H, 0xffffffff);
        present();
        if (k) worst[n++] = frames[nframes - 1];
    }
    bench_report("movie_p95", pct(worst, n, 0.95));
    bench_report("movie_late", late);
}

static void save(void)
{
    char path[MAX_PATH];
    HANDLE f;
    DWORD put;
    int rec[4] = { 0x31564153, STEPS - 3, choice, chosen_scene };
    snprintf(path, sizeof(path), "%ssave", dir);
    CreateDirectoryA(path, NULL);
    snprintf(path, sizeof(path), "%ssave\\save.dat", dir);
    f = CreateFileA(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (f == INVALID_HANDLE_VALUE) bench_fail("CreateFile save.dat");
    WriteFile(f, rec, sizeof(rec), &put, NULL);
    /* (and a thumbnail: what the screen showed, 1/4 size) */
    {
        static unsigned thumb[320 * 180];
        IDirect3DSurface9 *sys;
        D3DLOCKED_RECT lr;
        int x, y;
        BCHECK(IDirect3DDevice9_CreateOffscreenPlainSurface(dev, SCREEN_W, SCREEN_H, D3DFMT_X8R8G8B8, D3DPOOL_SYSTEMMEM, &sys, NULL));
        BCHECK(IDirect3DDevice9_GetRenderTargetData(dev, backbuf, sys));
        BCHECK(IDirect3DSurface9_LockRect(sys, &lr, NULL, D3DLOCK_READONLY));
        for (y = 0; y < 180; y++)
            for (x = 0; x < 320; x++) thumb[y * 320 + x] = ((unsigned *)((BYTE *)lr.pBits + y * 4 * lr.Pitch))[x * 4];
        IDirect3DSurface9_UnlockRect(sys);
        IDirect3DSurface9_Release(sys);
        WriteFile(f, thumb, sizeof(thumb), &put, NULL);
    }
    CloseHandle(f);
}

static int clicked, click_x, click_y;
static LRESULT CALLBACK wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_LBUTTONDOWN) clicked = 1, click_x = (short)LOWORD(lp), click_y = (short)HIWORD(lp);
    if (msg == WM_DESTROY) PostQuitMessage(0);
    return DefWindowProcA(hwnd, msg, wp, lp);
}

static void finish(void)
{
    bench_report("frame_p50", pct(frames, nframes, 0.5));
    bench_report("frame_p95", pct(frames, nframes, 0.95));
    bench_report("frame_p99", pct(frames, nframes, 0.99));
    if (fades) bench_report("fade_ms", fade_sum / fades);
}

/* the frame of the current step: typing, a fade, the choice, or nothing new (0) */
static int frame(void)
{
    const Step *s = &script[step];
    if (fading)
    {
        double k = (now_ms() - fade_t0) / 500.0;
        float c[4] = { (float)(k > 1 ? 1 : k), 0, 0, 0 };
        begin();
        IDirect3DDevice9_SetPixelShader(dev, ps_fade);
        IDirect3DDevice9_SetPixelShaderConstantF(dev, 0, c, 1);
        IDirect3DDevice9_SetTexture(dev, 1, (IDirect3DBaseTexture9 *)rt[1]);
        draw(rt[0], 0, 0, SCREEN_W, SCREEN_H, 0xffffffff);
        IDirect3DDevice9_SetPixelShader(dev, NULL);
        IDirect3DDevice9_SetTexture(dev, 1, NULL);
        present();
        if (k >= 1)
        {
            fading = 0;
            fade_sum += now_ms() - fade_t0, fades++;
            typing = 1;
            state("scene");
        }
        return 1;
    }
    if (typing)
    {
        typed += 2;
        text_update();
        begin();
        scene(1);
        present();
        if (typed == 2 && s->kind == LINE) state("line");
        if (typed >= lstrlenW(line))
        {
            typing = 0;
            printf("BENCH-WAIT %d%s\n", step, s->flags & IDLE ? " idle" : "");
            fflush(stdout);
        }
        return 1;
    }
    return 0;
}

static void go(int to);

static void choice_screen(void)
{
    begin();
    scene(0);
    draw(t_btn, 440, 260, 400, 80, 0xffffffff);
    draw(t_btn, 440, 390, 400, 80, 0xffffffff);
    bench_capture_ = 1;
    present();
    checked("choice");
    state("choice");
    printf("BENCH-WAIT %d choice\n", step);
    fflush(stdout);
}

static void go(int to)
{
    const Step *s;
    step = to;
    s = &script[step];
    last_frame = 0;
    switch (s->kind)
    {
    case TITLE:
        load_step(s);
        line = L"クリックしてはじめる";
        typed = 1000;
        text_update();
        begin();
        scene(1);
        present();
        state("title");
        printf("BENCH-WAIT %d\n", step);
        fflush(stdout);
        break;
    case SCENE:
        /* the old picture and the new one into render targets, then a 0.5 s crossfade */
        into(rt[0], 0);
        load_step(s);
        line = s->text, typed = 0;
        into(rt[1], 0);
        fading = 1;
        fade_t0 = now_ms();
        break;
    case LINE:
        load_step(s);
        line = s->text, typed = 0, typing = 1;
        break;
    case MOVIE:
        movie();
        go(step + 1);
        break;
    case CHOICE:
        choice_screen();
        break;
    case SAVE:
        save();
        state("saved");
        go(step + 1);
        break;
    case END:
        state("end");
        finish();
        bench_end();
        ExitProcess(0);
    }
}

static void click(void)
{
    const Step *s = &script[step];
    if (fading || typing) return; /* (no skipping: every run does the same work) */
    if (s->kind == CHOICE)
    {
        if (click_x < 440 || click_x >= 840) return;
        if (click_y >= 260 && click_y < 340) choice = 0;
        else if (click_y >= 390 && click_y < 470) choice = 1;
        else return;
        chosen_scene = choice ? 5 : 4;
        go(step + 1);
        return;
    }
    go(step + 1);
}

static void restore(void)
{
    char path[MAX_PATH];
    HANDLE f;
    DWORD got;
    int rec[4];
    snprintf(path, sizeof(path), "%ssave\\save.dat", dir);
    f = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    if (f == INVALID_HANDLE_VALUE) bench_fail("no save.dat");
    if (!ReadFile(f, rec, sizeof(rec), &got, NULL) || got != sizeof(rec) || rec[0] != 0x31564153) bench_fail("save.dat: bad");
    CloseHandle(f);
    step = rec[1];
    chosen_scene = rec[3];
    load_step(&script[step]);
    line = script[step].text, typed = 1000;
    text_update();
    begin();
    scene(1);
    bench_capture_ = 1;
    present();
    checked("restored");
    state("restored");
    bench_end();
    ExitProcess(0);
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    D3DPRESENT_PARAMETERS pp = { 0 };
    IDirect3D9 *d3d;
    HWND hwnd;
    double t0 = now_ms();
    int i;
    (void)inst, (void)prev, (void)cmd, (void)show;
    bench_args();
    hwnd = bench_window("scenario", SCREEN_W, SCREEN_H, wndproc);
    d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) bench_fail("Direct3DCreate9");
    pp.Windowed = TRUE;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
    pp.BackBufferWidth = SCREEN_W;
    pp.BackBufferHeight = SCREEN_H;
    pp.BackBufferFormat = D3DFMT_X8R8G8B8;
    pp.BackBufferCount = 1;
    pp.hDeviceWindow = hwnd;
    pp.PresentationInterval = D3DPRESENT_INTERVAL_ONE;
    BCHECK(IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_HARDWARE_VERTEXPROCESSING, &pp, &dev));
    IDirect3DDevice9_GetRenderTarget(dev, 0, &backbuf);
    states_2d();
    open_archive();
    bgm_out = open_out(44100, 2);
    voice_out = open_out(22050, 1);
    {
        ID3DXBuffer *code, *errors = NULL;
        const char *src = "ps_2_0\n dcl t0.xy\n dcl_2d s0\n dcl_2d s1\n texld r0, t0, s0\n texld r1, t0, s1\n"
                          " lrp r2, c0.x, r1, r0\n mov oC0, r2\n";
        if (FAILED(D3DXAssembleShader(src, strlen(src), NULL, NULL, 0, &code, &errors))) bench_fail("D3DXAssembleShader");
        BCHECK(IDirect3DDevice9_CreatePixelShader(dev, ID3DXBuffer_GetBufferPointer(code), &ps_fade));
    }
    for (i = 0; i < 2; i++)
        BCHECK(IDirect3DDevice9_CreateTexture(dev, SCREEN_W, SCREEN_H, 1, D3DUSAGE_RENDERTARGET, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, &rt[i], NULL));
    BCHECK(IDirect3DDevice9_CreateTexture(dev, 480, 270, 1, D3DUSAGE_DYNAMIC, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, &t_movie, NULL));
    BCHECK(IDirect3DDevice9_CreateTexture(dev, 1024, 64, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t_text, NULL));
    {
        unsigned *p = pic_alloc(1200, 200);
        D3DLOCKED_RECT lr;
        int y;
        pic_window(p, 1200, 200, 0);
        BCHECK(IDirect3DDevice9_CreateTexture(dev, 1200, 200, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t_win, NULL));
        BCHECK(IDirect3DTexture9_LockRect(t_win, 0, &lr, NULL, 0));
        for (y = 0; y < 200; y++) memcpy((BYTE *)lr.pBits + y * lr.Pitch, p + y * 1200, 1200 * 4);
        IDirect3DTexture9_UnlockRect(t_win, 0);
        free(p);
    }
    t_btn = solid(400, 80, 0xc0303050);
    {
        BITMAPINFO bi = { { sizeof(BITMAPINFOHEADER), 1024, -64, 1, 32, BI_RGB } };
        text_dc = CreateCompatibleDC(NULL);
        SelectObject(text_dc, CreateDIBSection(text_dc, &bi, DIB_RGB_COLORS, (void **)&text_bits, NULL, 0));
        SelectObject(text_dc, CreateFontW(-32, 0, 0, 0, FW_NORMAL, 0, 0, 0, SHIFTJIS_CHARSET, 0, 0, ANTIALIASED_QUALITY, 0, L"MS Gothic"));
        SetBkMode(text_dc, TRANSPARENT);
        SetTextColor(text_dc, RGB(255, 255, 255));
    }
    if (!strcmp(bench_case, "load")) restore();
    go(0);
    bench_report("boot_to_title", now_ms() - t0);
    for (;;)
    {
        if (!bench_pump()) ExitProcess(0);
        if (clicked)
        {
            clicked = 0;
            click();
        }
        /* (nothing moving: wait for input or the next tick, as engines' loops do) */
        if (!frame()) MsgWaitForMultipleObjects(0, NULL, FALSE, 16, QS_ALLINPUT);
    }
}
