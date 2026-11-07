/* Plays back a D3D9 call stream (`ippai trace` made it from an apitrace trace, docs/replay.md)
 * on this system's D3D9, real Windows or sake, and prints what it drew in tests/winref's probe
 * format (probe.h), so that two systems' replays of one recording compare line by line:
 *
 *   image f<n> ...   the back buffer before the n-th Present (frames from 0)
 *   image r<call>    the surface a GetRenderTargetData of the recording read back
 *   image d<call>    (-draws) the render target after every draw
 *   hr <call> <method> <got> want <recorded>    a call whose HRESULT differs from the recording's
 *   unknown <name> / no object <name>           calls it could not make (once per name)
 *
 *   d3d9replay.exe <stream> [-draws] [-every N] [-from N] [-to N] [-tol N] [-noimages]
 *
 * Built with -DSTREAM="file" the stream is inside the exe (a winref probe: replay_<trace>-32.exe
 * runs without arguments, as the other probes do).
 *
 * Every call is made through its vtable index (methods.sh lists them from d3d9.h) with the
 * arguments the stream gives as 32-bit words: on x86 every D3D9 method argument is one stack
 * word (floats too). The device's window is this program's own, windowed, at the recorded back
 * buffer size.
 * arch: 32 */
#define COBJMACROS
#include "../winref/probe.h"
#include <d3d9.h>

typedef UINT_PTR W;

static const struct method { const char *name; int index, hr; } methods[] = {
#include "d3d9methods.h"
};

/* stream */
#ifdef STREAM
__asm__(".section .rdata,\"dr\"\n.globl _stream_data\n_stream_data:\n.incbin \"" STREAM "\"\n.globl _stream_end\n_stream_end:\n.text\n");
extern const unsigned char stream_data[], stream_end[];
#endif
static FILE *in;
static const unsigned char *mem, *mem_end;
static int rd(void *p, size_t n) {
    if (mem) {
        if ((size_t)(mem_end - mem) < n) return 0;
        memcpy(p, mem, n);
        mem += n;
        return 1;
    }
    return fread(p, 1, n, in) == n;
}
static int rd_byte(void) { unsigned char c; return rd(&c, 1) ? c : EOF; }
static UINT u32(void) { UINT v = 0; if (!rd(&v, 4)) { printf("stream: truncated\n"); exit(2); } return v; }
static BYTE u8(void) { BYTE v = 0; if (!rd(&v, 1)) { printf("stream: truncated\n"); exit(2); } return v; }

/* names (by id) -> methods */
static char **names;
static const struct method **name_method;
static UINT nnames;
static unsigned char *name_warned;

/* objects by handle */
static IUnknown **obj;
static UINT nobj;
static IUnknown *get_obj(UINT h) { return h < nobj ? obj[h] : NULL; }
static void set_obj(UINT h, void *p) {
    if (!h) return;
    if (h >= nobj) {
        UINT n = h * 2 + 16;
        obj = realloc(obj, n * sizeof *obj);
        memset(obj + nobj, 0, (n - nobj) * sizeof *obj);
        nobj = n;
    }
    obj[h] = p;
}

/* locked regions: (handle, key) -> where this system put them */
static struct lock { UINT h, key; BYTE *p; INT pitch, slice; } locks[64];
static int nlocks, lock_next;
static void lock_set(UINT h, UINT key, void *p, INT pitch, INT slice) {
    int i;
    for (i = 0; i < nlocks; i++) if (locks[i].h == h && locks[i].key == key) break;
    if (i == nlocks) {
        if (nlocks < 64) nlocks++; else i = lock_next++ % 64;
    }
    locks[i] = (struct lock){ h, key, p, pitch, slice };
}
static struct lock *lock_get(UINT h, UINT key) {
    for (int i = 0; i < nlocks; i++) if (locks[i].h == h && locks[i].key == key) return &locks[i];
    return NULL;
}

/* options */
static int opt_draws, opt_every = 1, opt_from = 0, opt_to = 0x7fffffff, opt_tol = 2, opt_noimages;
static int frame;

static HWND hwnd;
static int win_w = 640, win_h = 480;

static void dump_surface(const char *name, IDirect3DSurface9 *s) {
    D3DSURFACE_DESC d;
    D3DLOCKED_RECT lr;
    if (opt_noimages) return;
    if (FAILED(IDirect3DSurface9_GetDesc(s, &d))) { printf("image %s: GetDesc failed\n", name); return; }
    if (FAILED(IDirect3DSurface9_LockRect(s, &lr, NULL, D3DLOCK_READONLY))) { printf("image %s: LockRect failed\n", name); return; }
    if (d.Format == D3DFMT_X8R8G8B8 || d.Format == D3DFMT_A8R8G8B8) {
        probe_image(name, lr.pBits, d.Width, d.Height, lr.Pitch, opt_tol);
    } else if (d.Format == D3DFMT_R5G6B5 || d.Format == D3DFMT_X1R5G5B5 || d.Format == D3DFMT_A1R5G5B5) {
        DWORD *px = malloc(d.Width * d.Height * 4);
        for (UINT y = 0; y < d.Height; y++)
            for (UINT x = 0; x < d.Width; x++) {
                WORD c = ((WORD *)((BYTE *)lr.pBits + y * lr.Pitch))[x];
                UINT r, g, b;
                if (d.Format == D3DFMT_R5G6B5) { r = c >> 11; g = (c >> 5) & 63; b = c & 31; g = (g << 2) | (g >> 4); }
                else { r = (c >> 10) & 31; g = (c >> 5) & 31; b = c & 31; g = (g << 3) | (g >> 2); }
                r = (r << 3) | (r >> 2); b = (b << 3) | (b >> 2);
                px[y * d.Width + x] = r << 16 | g << 8 | b;
            }
        probe_image(name, px, d.Width, d.Height, d.Width * 4, opt_tol);
        free(px);
    } else {
        printf("image %s: format %u\n", name, (unsigned)d.Format);
    }
    IDirect3DSurface9_UnlockRect(s);
}

/* a render target's pixels, through a system-memory copy (and a plain one first if it is
 * multisampled) */
static void dump_target(IDirect3DDevice9 *dev, const char *name, IDirect3DSurface9 *rt) {
    D3DSURFACE_DESC d;
    IDirect3DSurface9 *plain = NULL, *sys = NULL;
    HRESULT hr;
    if (opt_noimages) return;
    if (FAILED(IDirect3DSurface9_GetDesc(rt, &d))) { printf("image %s: GetDesc failed\n", name); return; }
    if (d.MultiSampleType != D3DMULTISAMPLE_NONE) {
        hr = IDirect3DDevice9_CreateRenderTarget(dev, d.Width, d.Height, d.Format, D3DMULTISAMPLE_NONE, 0, FALSE, &plain, NULL);
        if (FAILED(hr)) { printf("image %s: CreateRenderTarget %#lx\n", name, hr); return; }
        IDirect3DDevice9_StretchRect(dev, rt, NULL, plain, NULL, D3DTEXF_NONE);
        rt = plain;
    }
    hr = IDirect3DDevice9_CreateOffscreenPlainSurface(dev, d.Width, d.Height, d.Format, D3DPOOL_SYSTEMMEM, &sys, NULL);
    if (SUCCEEDED(hr)) hr = IDirect3DDevice9_GetRenderTargetData(dev, rt, sys);
    if (FAILED(hr)) printf("image %s: GetRenderTargetData %#lx\n", name, hr);
    else dump_surface(name, sys);
    if (sys) IDirect3DSurface9_Release(sys);
    if (plain) IDirect3DSurface9_Release(plain);
}

static int frame_wanted(void) { return frame >= opt_from && frame <= opt_to && (frame - opt_from) % opt_every == 0; }

/* before a Present: the back buffer it shows */
static void before_present(IUnknown *o, const char *method) {
    char name[32];
    IDirect3DSurface9 *bb = NULL;
    IDirect3DDevice9 *dev = NULL;
    HRESULT hr;
    if (!frame_wanted()) return;
    sprintf(name, "f%d", frame);
    if (strstr(method, "SwapChain9")) {
        hr = IDirect3DSwapChain9_GetBackBuffer((IDirect3DSwapChain9 *)o, 0, D3DBACKBUFFER_TYPE_MONO, &bb);
        if (SUCCEEDED(hr)) hr = IDirect3DSwapChain9_GetDevice((IDirect3DSwapChain9 *)o, &dev);
    } else {
        dev = (IDirect3DDevice9 *)o;
        IDirect3DDevice9_AddRef(dev);
        hr = IDirect3DDevice9_GetBackBuffer(dev, 0, 0, D3DBACKBUFFER_TYPE_MONO, &bb);
    }
    if (FAILED(hr)) printf("image %s: GetBackBuffer %#lx\n", name, hr);
    else dump_target(dev, name, bb);
    if (bb) IDirect3DSurface9_Release(bb);
    if (dev) IDirect3DDevice9_Release(dev);
}

/* D3DPRESENT_PARAMETERS as 14 words: windowed, in this program's window, no vsync */
static void present_params(W *a, int i) {
    DWORD *pp = (DWORD *)a[i];
    if (!pp) return;
    if (pp[0] && pp[1] && ((int)pp[0] != win_w || (int)pp[1] != win_h)) {
        RECT r = { 0, 0, (LONG)pp[0], (LONG)pp[1] };
        AdjustWindowRect(&r, WS_POPUP, FALSE);
        win_w = pp[0];
        win_h = pp[1];
        SetWindowPos(hwnd, NULL, 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOMOVE | SWP_NOZORDER);
    }
    pp[7] = (DWORD)(UINT_PTR)hwnd;
    pp[8] = TRUE;
    pp[12] = 0;
    pp[13] = D3DPRESENT_INTERVAL_IMMEDIATE;
}

static W callv(void *o, int index, int n, W *a) {
    void *fn = (*(void ***)o)[index];
#define F(...) ((W(__stdcall *)(void *, ##__VA_ARGS__))fn)
    switch (n) {
    case 0: return F()(o);
    case 1: return F(W)(o, a[0]);
    case 2: return F(W, W)(o, a[0], a[1]);
    case 3: return F(W, W, W)(o, a[0], a[1], a[2]);
    case 4: return F(W, W, W, W)(o, a[0], a[1], a[2], a[3]);
    case 5: return F(W, W, W, W, W)(o, a[0], a[1], a[2], a[3], a[4]);
    case 6: return F(W, W, W, W, W, W)(o, a[0], a[1], a[2], a[3], a[4], a[5]);
    case 7: return F(W, W, W, W, W, W, W)(o, a[0], a[1], a[2], a[3], a[4], a[5], a[6]);
    case 8: return F(W, W, W, W, W, W, W, W)(o, a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]);
    case 9: return F(W, W, W, W, W, W, W, W, W)(o, a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8]);
    case 10: return F(W, W, W, W, W, W, W, W, W, W)(o, a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9]);
    case 11: return F(W, W, W, W, W, W, W, W, W, W, W)(o, a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10]);
    case 12: return F(W, W, W, W, W, W, W, W, W, W, W, W)(o, a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11]);
    }
#undef F
    printf("too many arguments: %d\n", n);
    exit(2);
}

#define MAXARGS 16
static BYTE scratch[MAXARGS][4096];

static void warn(UINT id, int what, const char *fmt) {
    if (id < nnames && !(name_warned[id] & what)) {
        name_warned[id] |= what;
        printf(fmt, names[id]);
    }
}

static void call_record(void) {
    UINT no = u32(), id = u32(), ret = u32();
    BYTE has_ret = u8() & 1, n = u8();
    BYTE kind[MAXARGS];
    UINT word[MAXARGS];
    W a[MAXARGS];
    void *blob[MAXARGS] = { 0 };
    W out[MAXARGS] = { 0 };
    union { D3DLOCKED_RECT r; D3DLOCKED_BOX b; } box[MAXARGS];
    if (n > MAXARGS) { printf("stream: %u arguments\n", n); exit(2); }
    for (int i = 0; i < n; i++) {
        kind[i] = u8();
        word[i] = 0;
        switch (kind[i]) {
        case 0: case 1: case 3: case 4: case 5: case 6: word[i] = u32(); break;
        case 2: {
            UINT len = u32();
            blob[i] = malloc(len + 4);
            if (len && !rd(blob[i], len)) { printf("stream: truncated\n"); exit(2); }
            memset((BYTE *)blob[i] + len, 0, 4);
            break;
        }
        case 7: case 8: break;
        default: printf("stream: argument kind %u\n", kind[i]); exit(2);
        }
    }
    const char *name = id < nnames ? names[id] : "?";
    const struct method *m = id < nnames ? name_method[id] : NULL;
    int is_fn = !strstr(name, "::");
    for (int i = 0; i < n; i++) {
        switch (kind[i]) {
        case 0: a[i] = word[i]; break;
        case 1: a[i] = (W)get_obj(word[i]); break;
        case 2: a[i] = (W)blob[i]; break;
        case 3: case 4: a[i] = (W)&out[i]; break;
        case 5: case 6: memset(&box[i], 0, sizeof box[i]); a[i] = (W)&box[i]; break;
        case 7: memset(scratch[i], 0, sizeof scratch[i]); a[i] = (W)scratch[i]; break;
        case 8: a[i] = (W)hwnd; break;
        }
    }
    const char *method = strstr(name, "::") ? strstr(name, "::") + 2 : name;
    HRESULT hr = 0;
    int made = 0;
    if (is_fn) {
        if (!strcmp(name, "Direct3DCreate9") && n >= 2) {
            set_obj(word[n - 1], Direct3DCreate9(a[0]));
        } else if (!strcmp(name, "Direct3DCreate9Ex") && n >= 3) {
            static HRESULT (WINAPI *create_ex)(UINT, IDirect3D9Ex **);
            if (!create_ex) create_ex = (void *)GetProcAddress(GetModuleHandleA("d3d9.dll"), "Direct3DCreate9Ex");
            IDirect3D9Ex *d = NULL;
            hr = create_ex ? create_ex(a[0], &d) : E_NOTIMPL;
            set_obj(word[1], d);
            made = 1;
        } else {
            warn(id, 1, "unknown %s\n");
        }
    } else if (!m) {
        warn(id, 1, "unknown %s\n");
    } else if (!n || !a[0]) {
        warn(id, 2, "no object %s\n");
    } else {
        if (!strcmp(method, "CreateDevice") || !strcmp(method, "CreateDeviceEx")) {
            present_params(a, 5);
            if (n > 7) a[6] = 0; /* (no fullscreen display mode) */
        } else if (!strcmp(method, "Reset") || !strcmp(method, "ResetEx") || !strcmp(method, "CreateAdditionalSwapChain")) {
            present_params(a, 1);
            if (!strcmp(method, "ResetEx")) a[2] = 0;
        }
        if (!strcmp(method, "Present") || !strcmp(method, "PresentEx")) {
            before_present((IUnknown *)a[0], name);
            frame++;
        }
        hr = callv((void *)a[0], m->index, n - 1, a + 1);
        made = m->hr;
        if (SUCCEEDED(hr) || !m->hr) {
            for (int i = 1; i < n; i++) {
                if (kind[i] == 3) set_obj(word[i], (void *)out[i]);
                else if (kind[i] == 4) lock_set(word[0], word[i], (void *)out[i], 0, 0);
                else if (kind[i] == 5) lock_set(word[0], word[i], box[i].r.pBits, box[i].r.Pitch, 0);
                else if (kind[i] == 6) lock_set(word[0], word[i], box[i].b.pBits, box[i].b.RowPitch, box[i].b.SlicePitch);
            }
        }
        if (m->hr && SUCCEEDED(hr)) {
            char img[32];
            if (!strcmp(method, "GetRenderTargetData") && n >= 3 && a[2]) {
                sprintf(img, "r%u", no);
                dump_surface(img, (IDirect3DSurface9 *)a[2]);
            } else if (opt_draws && !strncmp(method, "Draw", 4) && strstr(name, "Device9")) {
                IDirect3DSurface9 *rt = NULL;
                if (SUCCEEDED(IDirect3DDevice9_GetRenderTarget((IDirect3DDevice9 *)a[0], 0, &rt))) {
                    sprintf(img, "d%u", no);
                    dump_target((IDirect3DDevice9 *)a[0], img, rt);
                    IDirect3DSurface9_Release(rt);
                }
            }
        }
    }
    if (made && has_ret && (UINT)hr != ret) printf("hr %u %s %#lx want %#x\n", no, name, (unsigned long)hr, ret);
    for (int i = 0; i < n; i++) free(blob[i]);
}

static void memcpy_record(void) {
    UINT h = u32(), key = u32(), off = u32(), pitch = u32(), slice = u32(), len = u32();
    BYTE *data = malloc(len ? len : 1);
    if (len && !rd(data, len)) { printf("stream: truncated\n"); exit(2); }
    struct lock *l = lock_get(h, key);
    if (!l || !l->p) {
        static int warned;
        if (!warned++) printf("memcpy into no lock (object %u key %u)\n", h, key);
    } else if (!pitch) {
        memcpy(l->p + off, data, len);
    } else {
        /* row by row: this system's pitches may differ from the recording's */
        for (UINT pos = 0; pos < len;) {
            UINT o = off + pos, z = slice ? o / slice : 0, r = slice ? o % slice : o;
            UINT y = r / pitch, x = r % pitch, k = pitch - x;
            if (k > len - pos) k = len - pos;
            if ((INT)x < l->pitch && (!l->slice || (INT)(y * l->pitch) < l->slice)) {
                UINT c = k < (UINT)(l->pitch - x) ? k : (UINT)(l->pitch - x);
                memcpy(l->p + z * l->slice + y * l->pitch + x, data + pos, c);
            }
            pos += k;
        }
    }
    free(data);
}

static void name_record(void) {
    UINT id = u32();
    WORD len;
    if (!rd(&len, 2)) { printf("stream: truncated\n"); exit(2); }
    char *s = malloc(len + 1);
    if (!rd(s, len)) { printf("stream: truncated\n"); exit(2); }
    s[len] = 0;
    if (id >= nnames) {
        UINT k = id * 2 + 16;
        names = realloc(names, k * sizeof *names);
        name_method = realloc(name_method, k * sizeof *name_method);
        name_warned = realloc(name_warned, k);
        memset(names + nnames, 0, (k - nnames) * sizeof *names);
        memset(name_method + nnames, 0, (k - nnames) * sizeof *name_method);
        memset(name_warned + nnames, 0, k - nnames);
        nnames = k;
    }
    names[id] = s;
    for (size_t i = 0; i < sizeof methods / sizeof methods[0]; i++)
        if (!strcmp(methods[i].name, s)) { name_method[id] = &methods[i]; break; }
}

int main(int argc, char **argv) {
    const char *path = NULL;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "-draws")) opt_draws = 1;
        else if (!strcmp(argv[i], "-noimages")) opt_noimages = 1;
        else if (!strcmp(argv[i], "-every") && i + 1 < argc) opt_every = atoi(argv[++i]);
        else if (!strcmp(argv[i], "-from") && i + 1 < argc) opt_from = atoi(argv[++i]);
        else if (!strcmp(argv[i], "-to") && i + 1 < argc) opt_to = atoi(argv[++i]);
        else if (!strcmp(argv[i], "-tol") && i + 1 < argc) opt_tol = atoi(argv[++i]);
        else path = argv[i];
    }
    if (opt_every < 1) opt_every = 1;
#ifdef STREAM
    if (!path) { mem = stream_data; mem_end = stream_end; path = STREAM; }
#endif
    if (!path) { printf("usage: d3d9replay <stream> [-draws] [-every N] [-from N] [-to N] [-tol N] [-noimages]\n"); return 2; }
    if (!mem && !(in = fopen(path, "rb"))) { printf("cannot open %s\n", path); return 2; }
    char magic[8];
    if (!rd(magic, 8) || memcmp(magic, "IPRS\1\0\0\0", 8)) { printf("not a replay stream (version 1)\n"); return 2; }

    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = DefWindowProcA;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "d3d9replay";
    RegisterClassA(&wc);
    hwnd = CreateWindowA("d3d9replay", "d3d9replay", WS_POPUP | WS_VISIBLE, 0, 0, win_w, win_h, 0, 0, wc.hInstance, 0);

    int c, calls = 0;
    while ((c = rd_byte()) != EOF) {
        switch (c) {
        case 1: name_record(); break;
        case 2: call_record(); calls++; break;
        case 3: memcpy_record(); break;
        default: printf("stream: record %d\n", c); return 2;
        }
        if (!(calls & 1023)) {
            MSG msg;
            while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) DispatchMessageA(&msg);
        }
    }
    printf("calls %d frames %d\n", calls, frame);
    return 0;
}
