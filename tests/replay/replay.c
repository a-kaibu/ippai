/* Plays back a D3D8 / D3D9 / D3D11 / DirectDraw (and Direct3D 7) call stream (the apitrace crate made it from an apitrace
 * trace, docs/replay.md) on this system, real Windows or sake, and prints what it drew in
 * tests/winref's probe format (probe.h), so that two systems' replays of one recording compare
 * line by line:
 *
 *   image f<n> ...   the back buffer before the n-th Present (frames from 0)
 *   image r<call>    (D3D9) the surface a GetRenderTargetData of the recording read back
 *   image l<call>    (D3D8) a surface the recording locked to read (D3DLOCK_READONLY)
 *   image m<call>    (D3D11) a texture the recording mapped to read (its part last copied into
 *                    it), and m<call>_alpha; data m<call> ...: a buffer's first bytes
 *   image d<call>    (-draws) the render target after every draw (DirectDraw: the surface after
 *                    every Blt / BltFast, the render target after every Direct3D 7 draw)
 *   DirectDraw (ddrawimg.c): image f<n> is the back buffer before a Flip, or the part of the
 *   primary surface a Blt / BltFast drew to (each one a frame); image l<call> a surface locked to
 *   read (DDLOCK_READONLY)
 *   hr <call> <method> <got> want <recorded>    a call whose HRESULT differs from the recording's
 *   unknown <name> / no object <name>           calls it could not make (once per name)
 *
 *   replay.exe <stream> [-draws] [-every N] [-from N] [-to N] [-tol N] [-noimages]
 *
 * Built with -DSTREAM="file" the stream is inside the exe (a winref probe: replay_<trace>.exe
 * runs without arguments, as the other probes do).
 *
 * Every method is called through its vtable index (the stream names it) with the stream's
 * arguments as words: on x86 every D3D argument is one stack word (floats too). On x64 the
 * call goes through a varargs pointer with each argument as a double holding the word's bits:
 * Win64 passes a varargs double in both its XMM register and its integer one (and on the stack
 * past the fourth), so a parameter declared integer, pointer or float each finds its bits.
 * The device's window is this program's own, windowed, at the recorded back buffer size. */
#include "../winref/probe.h"
#include "replay.h"

/* stream */
#ifdef STREAM
#ifdef _WIN64
__asm__(".section .rdata,\"dr\"\n.globl stream_data\nstream_data:\n.incbin \"" STREAM "\"\n.globl stream_end\nstream_end:\n.text\n");
#else
__asm__(".section .rdata,\"dr\"\n.globl _stream_data\n_stream_data:\n.incbin \"" STREAM "\"\n.globl _stream_end\n_stream_end:\n.text\n");
#endif
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
static void truncated(void) { printf("stream: truncated\n"); exit(2); }
static int rd_byte(void) { unsigned char c; return rd(&c, 1) ? c : EOF; }
static UINT u32(void) { UINT v = 0; if (!rd(&v, 4)) truncated(); return v; }
static ULONGLONG u64(void) { ULONGLONG v = 0; if (!rd(&v, 8)) truncated(); return v; }
static BYTE u8(void) { BYTE v = 0; if (!rd(&v, 1)) truncated(); return v; }
static WORD u16(void) { WORD v = 0; if (!rd(&v, 2)) truncated(); return v; }

/* names (by id) */
static struct name { char *s; const char *method; char iface[64]; int index, flags; void *fn; unsigned char warned; } *names;
static UINT nnames;

/* objects by handle */
static void **obj;
static UINT nobj;
static void *get_obj(UINT h) { return h < nobj ? obj[h] : NULL; }
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
/* what each lock type hands back (D3DLOCKED_RECT, D3DLOCKED_BOX, D3D11_MAPPED_SUBRESOURCE) */
struct locked_rect { INT Pitch; void *pBits; };
struct locked_box { INT RowPitch, SlicePitch; void *pBits; };
struct mapped { void *pData; UINT RowPitch, DepthPitch; };

/* options */
/* (EVERY: the default -every of an exe with its stream inside: a game's, built by
 * tests/replay/game.sh, shows a frame in EVERY, not thousands) */
#ifndef EVERY
#define EVERY 1
#endif
/* (FROM, TO, DRAWS: a drill probe's defaults, tests/replay/drill.txt: the frames around where a
 * game's replay on sake first differed from Windows', every draw of them) */
#ifndef FROM
#define FROM 0
#endif
#ifndef TO
#define TO 0x7fffffff
#endif
#ifndef DRAWS
#define DRAWS 0
#endif
int opt_draws = DRAWS, opt_every = EVERY, opt_from = FROM, opt_to = TO, opt_tol = 2, opt_noimages;
int frame;
int frame_wanted(void) { return frame >= opt_from && frame <= opt_to && (frame - opt_from) % opt_every == 0; }
int draws_wanted(void) { return opt_draws && frame >= opt_from && frame <= opt_to; }
/* the call being made (a draw's picture says which it was) */
static const char *cur_name;

static HWND hwnd;
static int win_w = 640, win_h = 480;
static void win_pos(int x, int y, int w, int h) {
    if (w <= 0 || h <= 0) return;
    win_w = w;
    win_h = h;
    SetWindowPos(hwnd, HWND_TOP, x, y, w, h, 0);
}
static void win_size(int w, int h) {
    if (w <= 0 || h <= 0 || (w == win_w && h == win_h)) return;
    RECT r = { 0, 0, w, h };
    AdjustWindowRect(&r, WS_POPUP, FALSE);
    win_w = w;
    win_h = h;
    SetWindowPos(hwnd, NULL, 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOMOVE | SWP_NOZORDER);
}

void image_out(const char *name, const void *bits, int w, int h, int pitch, int fmt) {
    if (opt_noimages) return;
    /* (a draw's picture: which call made it, for tests/replay/status.py) */
    if (name[0] == 'd' && name[1] >= '0' && name[1] <= '9' && cur_name) printf("draw %s %s\n", name + 1, cur_name);
    if (fmt == IMG_BGRX) { probe_image(name, bits, w, h, pitch, opt_tol); return; }
    DWORD *px = malloc((size_t)w * h * 4 + 4);
    for (int y = 0; y < h; y++)
        for (int x = 0; x < w; x++) {
            const BYTE *row = (const BYTE *)bits + (size_t)y * pitch;
            UINT r, g, b;
            if (fmt == IMG_RGBX) { r = row[x * 4]; g = row[x * 4 + 1]; b = row[x * 4 + 2]; }
            else {
                WORD c = ((const WORD *)row)[x];
                if (fmt == IMG_565) { r = c >> 11; g = (c >> 5) & 63; b = c & 31; g = (g << 2) | (g >> 4); }
                else { r = (c >> 10) & 31; g = (c >> 5) & 31; b = c & 31; g = (g << 3) | (g >> 2); }
                r = (r << 3) | (r >> 2); b = (b << 3) | (b >> 2);
            }
            px[y * w + x] = r << 16 | g << 8 | b;
        }
    probe_image(name, px, w, h, w * 4, opt_tol);
    free(px);
}

void image_alpha(const char *name, const void *bits, int w, int h, int pitch) {
    char n[64];
    if (opt_noimages) return;
    DWORD *px = malloc((size_t)w * h * 4 + 4);
    for (int y = 0; y < h; y++)
        for (int x = 0; x < w; x++) {
            UINT a = ((const BYTE *)bits)[(size_t)y * pitch + x * 4 + 3];
            px[y * w + x] = a << 16 | a << 8 | a;
        }
    snprintf(n, sizeof n, "%s_alpha", name);
    probe_image(n, px, w, h, w * 4, opt_tol);
    free(px);
}

#define MAXARGS 16

/* a call through a function pointer with n words (o: the object, NULL for an entry point) */
static W callv(void *fn, void *o, int n, W *a) {
#ifdef _WIN64
    /* every argument a double with the word's bits (see the top) */
    double d[MAXARGS + 1];
    int k = 0;
    if (o) { memcpy(&d[k++], &o, 8); }
    for (int i = 0; i < n; i++) memcpy(&d[k++], &a[i], 8);
    if (!k) return ((W (*)(void))fn)();
    W first;
    memcpy(&first, &d[0], 8);
    typedef W (*V)(W, ...);
    V f = (V)fn;
#define D(i) d[i]
    switch (k) {
    case 1: return f(first);
    case 2: return f(first, D(1));
    case 3: return f(first, D(1), D(2));
    case 4: return f(first, D(1), D(2), D(3));
    case 5: return f(first, D(1), D(2), D(3), D(4));
    case 6: return f(first, D(1), D(2), D(3), D(4), D(5));
    case 7: return f(first, D(1), D(2), D(3), D(4), D(5), D(6));
    case 8: return f(first, D(1), D(2), D(3), D(4), D(5), D(6), D(7));
    case 9: return f(first, D(1), D(2), D(3), D(4), D(5), D(6), D(7), D(8));
    case 10: return f(first, D(1), D(2), D(3), D(4), D(5), D(6), D(7), D(8), D(9));
    case 11: return f(first, D(1), D(2), D(3), D(4), D(5), D(6), D(7), D(8), D(9), D(10));
    case 12: return f(first, D(1), D(2), D(3), D(4), D(5), D(6), D(7), D(8), D(9), D(10), D(11));
    case 13: return f(first, D(1), D(2), D(3), D(4), D(5), D(6), D(7), D(8), D(9), D(10), D(11), D(12));
    case 14: return f(first, D(1), D(2), D(3), D(4), D(5), D(6), D(7), D(8), D(9), D(10), D(11), D(12), D(13));
    case 15: return f(first, D(1), D(2), D(3), D(4), D(5), D(6), D(7), D(8), D(9), D(10), D(11), D(12), D(13), D(14));
    case 16: return f(first, D(1), D(2), D(3), D(4), D(5), D(6), D(7), D(8), D(9), D(10), D(11), D(12), D(13), D(14), D(15));
    case 17: return f(first, D(1), D(2), D(3), D(4), D(5), D(6), D(7), D(8), D(9), D(10), D(11), D(12), D(13), D(14), D(15), D(16));
    }
#undef D
#else
    W b[MAXARGS + 1];
    int k = 0;
    if (o) b[k++] = (W)o;
    for (int i = 0; i < n; i++) b[k++] = a[i];
#define F(...) ((W(__stdcall *)(__VA_ARGS__))fn)
#define A1 W
#define A2 A1, W
#define A3 A2, W
#define A4 A3, W
#define A5 A4, W
#define A6 A5, W
#define A7 A6, W
#define A8 A7, W
#define A9 A8, W
#define A10 A9, W
#define A11 A10, W
#define A12 A11, W
#define A13 A12, W
#define A14 A13, W
#define A15 A14, W
#define A16 A15, W
#define A17 A16, W
    switch (k) {
    case 0: return ((W(__stdcall *)(void))fn)();
    case 1: return F(A1)(b[0]);
    case 2: return F(A2)(b[0], b[1]);
    case 3: return F(A3)(b[0], b[1], b[2]);
    case 4: return F(A4)(b[0], b[1], b[2], b[3]);
    case 5: return F(A5)(b[0], b[1], b[2], b[3], b[4]);
    case 6: return F(A6)(b[0], b[1], b[2], b[3], b[4], b[5]);
    case 7: return F(A7)(b[0], b[1], b[2], b[3], b[4], b[5], b[6]);
    case 8: return F(A8)(b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]);
    case 9: return F(A9)(b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8]);
    case 10: return F(A10)(b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9]);
    case 11: return F(A11)(b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10]);
    case 12: return F(A12)(b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11]);
    case 13: return F(A13)(b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12]);
    case 14: return F(A14)(b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13]);
    case 15: return F(A15)(b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14]);
    case 16: return F(A16)(b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]);
    case 17: return F(A17)(b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15], b[16]);
    }
#undef F
#endif
    printf("too many arguments: %d\n", n);
    exit(2);
}

/* callv's own check: integers, pointers and floats in every position a method's arguments take
 * (registers and stack on x64), as the D3D methods declare them */
static W WINAPI callv_probe(void *o, W a, float b, W c, float d, W e, float f, W g, float h) {
    return (W)o == 0x51 && a == 1 && b == 2.5f && c == (W)-3 && d == -0.25f && e == 5 && f == 1e30f && g == 7 && h == 8.0f ? 1 : 0;
}
static void callv_check(void) {
    float fl[] = { 2.5f, -0.25f, 1e30f, 8.0f };
    W a[8] = { 1, 0, (W)-3, 0, 5, 0, 7, 0 };
    for (int i = 0; i < 4; i++) { UINT u; memcpy(&u, &fl[i], 4); a[i * 2 + 1] = u; }
    if (callv((void *)callv_probe, (void *)0x51, 8, a) != 1) { printf("callv: arguments do not arrive\n"); exit(2); }
}

/* an entry point by name, from the D3D DLLs */
static void *entry(const char *name) {
    static const char *dlls[] = { "d3d9.dll", "d3d8.dll", "d3d11.dll", "dxgi.dll", "ddraw.dll" };
    for (int i = 0; i < 5; i++) {
        HMODULE m = GetModuleHandleA(dlls[i]);
        if (!m) m = LoadLibraryA(dlls[i]);
        void *f = m ? (void *)GetProcAddress(m, name) : NULL;
        if (f) return f;
    }
    return NULL;
}

static BYTE scratch[MAXARGS][4096];

static void warn(UINT id, int what, const char *fmt) {
    if (id < nnames && !(names[id].warned & what)) {
        names[id].warned |= what;
        printf(fmt, names[id].s);
    }
}

enum { K_WORD, K_HANDLE, K_BLOB, K_OUTOBJ, K_LOCK, K_SCRATCH = 7, K_WINDOW, K_OUTARR };
enum { L_BUF, L_RECT, L_BOX, L_MAP, L_DDSD2, L_DDSD };

/* which API's hooks a call's interface goes to (7: DirectDraw and its Direct3D 7, whose
 * interfaces' names have no 8 or 9) */
static int api_of(const char *iface) {
    if (!strncmp(iface, "ID3D11", 6) || !strncmp(iface, "IDXGI", 5)) return 11;
    if (!strncmp(iface, "IDirectDraw", 11)) return 7;
    if (strchr(iface, '8')) return 8;
    return strchr(iface, '9') ? 9 : 7;
}

static void call_record(void) {
    UINT no = u32(), id = u32(), ret = u32();
    BYTE has_ret = u8() & 1, n = u8();
    BYTE kind[MAXARGS], lkind[MAXARGS];
    UINT word32[MAXARGS], key[MAXARGS];
    UINT *arr[MAXARGS] = { 0 };
    ULONGLONG word[MAXARGS];
    W a[MAXARGS];
    BYTE *blob[MAXARGS] = { 0 };
    W out[MAXARGS] = { 0 };
    union { struct locked_rect r; struct locked_box b; struct mapped m; BYTE dd[160]; } box[MAXARGS];
    if (n > MAXARGS) { printf("stream: %u arguments\n", n); exit(2); }
    for (int i = 0; i < n; i++) {
        kind[i] = u8();
        word[i] = word32[i] = 0;
        switch (kind[i]) {
        case K_WORD: word[i] = u64(); break;
        case K_HANDLE: case K_OUTOBJ: word32[i] = u32(); break;
        case K_LOCK: lkind[i] = u8(); word32[i] = u32(); key[i] = u32(); break;
        case K_BLOB: {
            UINT len = u32();
            blob[i] = malloc(len + 8);
            if (len && !rd(blob[i], len)) truncated();
            memset(blob[i] + len, 0, 8);
            for (UINT k = u16(); k; k--) {
                UINT off = u32();
                BYTE fk = u8();
                UINT v = u32();
                void *p = fk == 0 ? get_obj(v) : fk == 1 ? (void *)(blob[i] + v) : (void *)hwnd;
                if (off + sizeof p <= len) memcpy(blob[i] + off, &p, sizeof p);
            }
            break;
        }
        case K_OUTARR: {
            word32[i] = u32();
            arr[i] = malloc(word32[i] * 4 + 4);
            for (UINT k = 0; k < word32[i]; k++) arr[i][k] = u32();
            break;
        }
        case K_SCRATCH: case K_WINDOW: break;
        default: printf("stream: argument kind %u\n", kind[i]); exit(2);
        }
    }
    W outarr[MAXARGS][16];
    for (int i = 0; i < n; i++) {
        switch (kind[i]) {
        case K_WORD: a[i] = (W)word[i]; break;
        case K_HANDLE: a[i] = (W)get_obj(word32[i]); break;
        case K_BLOB: a[i] = (W)blob[i]; break;
        case K_OUTOBJ: a[i] = (W)&out[i]; break;
        case K_LOCK:
            memset(&box[i], 0, sizeof box[i]);
            if (lkind[i] == L_DDSD2 || lkind[i] == L_DDSD) ddraw_lock_prep(lkind[i] == L_DDSD2, box[i].dd);
            a[i] = lkind[i] == L_BUF ? (W)&out[i] : (W)&box[i];
            break;
        case K_SCRATCH: memset(scratch[i], 0, sizeof scratch[i]); a[i] = (W)scratch[i]; break;
        case K_WINDOW: a[i] = (W)hwnd; break;
        case K_OUTARR: memset(outarr[i], 0, sizeof outarr[i]); a[i] = (W)outarr[i]; break;
        }
    }
    struct name *nm = id < nnames && names[id].s ? &names[id] : NULL;
    cur_name = nm ? nm->s : NULL;
    HRESULT hr = 0;
    int made = 0;
    if (!nm) {
        printf("stream: name %u\n", id);
        exit(2);
    } else if (nm->index < 0) {
        /* an entry point (one returning its object has it as the last argument) */
        if (!nm->fn) nm->fn = entry(nm->s);
        int k = n - ((nm->flags & 2) != 0);
        if (!nm->fn) warn(id, 1, "unknown %s\n");
        else {
            W r = callv(nm->fn, NULL, k, a);
            if (nm->flags & 2) set_obj(word32[n - 1], (void *)r);
            hr = (HRESULT)r;
            made = nm->flags & 1;
            for (int i = 0; i < k; i++) {
                if (kind[i] == K_OUTOBJ) set_obj(word32[i], (void *)out[i]);
                else if (kind[i] == K_OUTARR) for (UINT j = 0; j < word32[i] && j < 16; j++) set_obj(arr[i][j], (void *)outarr[i][j]);
            }
        }
    } else if (!n || !a[0]) {
        warn(id, 2, "no object %s\n");
    } else {
        int api = api_of(nm->iface);
        if (api == 9) d3d9_before(nm->iface, nm->method, a, n, no);
        else if (api == 7) ddraw_before(nm->iface, nm->method, a, n, no);
        else if (api == 8) d3d8_before(nm->iface, nm->method, a, n, no);
        else d3d11_before(nm->iface, nm->method, a, n, no);
        void *fn = (*(void ***)a[0])[nm->index];
        hr = (HRESULT)callv(fn, (void *)a[0], n - 1, a + 1);
        made = nm->flags & 1;
        if (SUCCEEDED(hr) || !made) {
            for (int i = 1; i < n; i++) {
                if (kind[i] == K_OUTOBJ) set_obj(word32[i], (void *)out[i]);
                else if (kind[i] == K_OUTARR) for (UINT j = 0; j < word32[i] && j < 16; j++) set_obj(arr[i][j], (void *)outarr[i][j]);
                else if (kind[i] == K_LOCK) {
                    switch (lkind[i]) {
                    case L_BUF: lock_set(word32[i], key[i], (void *)out[i], 0, 0); break;
                    case L_RECT: lock_set(word32[i], key[i], box[i].r.pBits, box[i].r.Pitch, 0); break;
                    case L_BOX: lock_set(word32[i], key[i], box[i].b.pBits, box[i].b.RowPitch, box[i].b.SlicePitch); break;
                    case L_MAP: lock_set(word32[i], key[i], box[i].m.pData, box[i].m.RowPitch, box[i].m.DepthPitch); break;
                    case L_DDSD2: case L_DDSD: {
                        void *p;
                        INT pitch;
                        ddraw_lock_got(lkind[i] == L_DDSD2, box[i].dd, &p, &pitch);
                        lock_set(word32[i], key[i], p, pitch, 0);
                        break;
                    }
                    }
                }
            }
        }
        if (api == 9) d3d9_after(nm->iface, nm->method, a, n, no, hr);
        else if (api == 7) ddraw_after(nm->iface, nm->method, a, n, no, hr);
        else if (api == 8) d3d8_after(nm->iface, nm->method, a, n, no, hr);
        else d3d11_after(nm->iface, nm->method, a, n, no, hr);
    }
    if (made && has_ret && (UINT)hr != ret) printf("hr %u %s %#lx want %#x\n", no, nm->s, (unsigned long)hr, ret);
    for (int i = 0; i < n; i++) { free(blob[i]); free(arr[i]); }
}

static void memcpy_record(void) {
    UINT h = u32(), key = u32(), off = u32(), pitch = u32(), slice = u32(), row = u32(), len = u32();
    BYTE *data = malloc(len ? len : 1);
    if (len && !rd(data, len)) truncated();
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
            /* (row: only the locked region's bytes of each row, the rest are other pixels) */
            UINT end = row && row < (UINT)l->pitch ? row : (UINT)l->pitch;
            if (x < end && (!l->slice || (INT)(y * l->pitch) < l->slice)) {
                UINT c = k < end - x ? k : end - x;
                memcpy(l->p + z * l->slice + y * l->pitch + x, data + pos, c);
            }
            pos += k;
        }
    }
    free(data);
}

static void name_record(void) {
    UINT id = u32();
    WORD len = u16();
    char *s = malloc(len + 1);
    if (!rd(s, len)) truncated();
    s[len] = 0;
    int index = (int)u32();
    BYTE flags = u8();
    if (id >= nnames) {
        UINT k = id * 2 + 16;
        names = realloc(names, k * sizeof *names);
        memset(names + nnames, 0, (k - nnames) * sizeof *names);
        nnames = k;
    }
    struct name *nm = &names[id];
    nm->s = s;
    nm->index = index;
    nm->flags = flags;
    const char *sep = strstr(s, "::");
    nm->method = sep ? sep + 2 : s;
    snprintf(nm->iface, sizeof nm->iface, "%.*s", sep ? (int)(sep - s) : 0, s);
}

int main(int argc, char **argv) {
    const char *path = NULL;
    callv_check();
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
    if (!path) { printf("usage: replay <stream> [-draws] [-every N] [-from N] [-to N] [-tol N] [-noimages]\n"); return 2; }
    if (!mem && !(in = fopen(path, "rb"))) { printf("cannot open %s\n", path); return 2; }
    char magic[12];
    if (!rd(magic, 12) || memcmp(magic, "IPRS\3\0\0\0", 8)) { printf("not a replay stream (version 3)\n"); return 2; }
    UINT bits;
    memcpy(&bits, magic + 8, 4);
    if (bits != sizeof(void *) * 8) { printf("a %u-bit stream: this replay is %u-bit\n", bits, (unsigned)sizeof(void *) * 8); return 2; }

    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = DefWindowProcA;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "replay";
    RegisterClassA(&wc);
    hwnd = CreateWindowA("replay", "replay", WS_POPUP | WS_VISIBLE, 0, 0, win_w, win_h, 0, 0, wc.hInstance, 0);

    int c, calls = 0;
    while ((c = rd_byte()) != EOF) {
        switch (c) {
        case 1: name_record(); break;
        case 2: call_record(); calls++; break;
        case 3: memcpy_record(); break;
        case 4: { UINT w = u32(), h = u32(); win_size(w, h); break; }
        case 5: { INT x = (INT)u32(), y = (INT)u32(); UINT w = u32(), h = u32(); win_pos(x, y, w, h); break; }
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
