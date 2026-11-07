/* Direct3D 9 state blocks (formal/tla/StateBlocks.tla): which states each type of
 * CreateStateBlock (ALL, PIXELSTATE, VERTEXSTATE) holds, what BeginStateBlock / EndStateBlock
 * record (and whether the calls in between change the device), what Capture and Apply do with
 * a recorded block, the calls out of order, and the references a block holds. Each line: a
 * state and whether a block of each type brings it back after it changed.
 * libs: -ld3d9
 * run: browser (sake's Direct3D 9 runs in the page: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <d3d9.h>

static IDirect3DDevice9 *dev;
static const D3DSTATEBLOCKTYPE types[3] = { D3DSBT_ALL, D3DSBT_PIXELSTATE, D3DSBT_VERTEXSTATE };

static ULONG refs(void *o) {
    IUnknown_AddRef((IUnknown *)o);
    return IUnknown_Release((IUnknown *)o);
}

/* a block of type t made with the state at a; the state set to b; the block applied: is it a
 * again? (set(v) sets, get() reads; 1 back, 0 not, - the block could not be made) */
typedef void (*setter)(int i, DWORD v);
typedef DWORD (*getter)(int i);

static void held(const char *what, int i, setter set, getter get, DWORD a, DWORD b) {
    char res[3];
    for (int k = 0; k < 3; k++) {
        IDirect3DStateBlock9 *sb = NULL;
        set(i, a);
        if (FAILED(IDirect3DDevice9_CreateStateBlock(dev, types[k], &sb))) { res[k] = '-'; continue; }
        set(i, b);
        IDirect3DStateBlock9_Apply(sb);
        res[k] = get(i) == a ? '1' : '0';
        IDirect3DStateBlock9_Release(sb);
    }
    printf("%s %d all %c pixel %c vertex %c\n", what, i, res[0], res[1], res[2]);
}

static void set_rs(int i, DWORD v) { IDirect3DDevice9_SetRenderState(dev, i, v); }
static DWORD get_rs(int i) { DWORD v = 0; IDirect3DDevice9_GetRenderState(dev, i, &v); return v; }
/* (texture stage states and sampler states: stage or sampler * 100 + state) */
static void set_tss(int i, DWORD v) { IDirect3DDevice9_SetTextureStageState(dev, i / 100, i % 100, v); }
static DWORD get_tss(int i) { DWORD v = 0; IDirect3DDevice9_GetTextureStageState(dev, i / 100, i % 100, &v); return v; }
static int sampler(int i) { int s = i / 100; return s < 16 ? s : s == 16 ? D3DDMAPSAMPLER : D3DVERTEXTEXTURESAMPLER0 + s - 17; }
static void set_ss(int i, DWORD v) { IDirect3DDevice9_SetSamplerState(dev, sampler(i), i % 100, v); }
static DWORD get_ss(int i) { DWORD v = 0; IDirect3DDevice9_GetSamplerState(dev, sampler(i), i % 100, &v); return v; }

static IDirect3DTexture9 *tex[2];
static IDirect3DVertexBuffer9 *vb[2];
static IDirect3DIndexBuffer9 *ib[2];
static IDirect3DVertexShader9 *vs[2];
static IDirect3DPixelShader9 *ps[2];
static IDirect3DVertexDeclaration9 *decl[2];

/* (objects: value 0 or 1 picks one of the two) */
static void set_tex(int i, DWORD v) { IDirect3DDevice9_SetTexture(dev, i, (IDirect3DBaseTexture9 *)tex[v]); }
static DWORD get_tex(int i) {
    IDirect3DBaseTexture9 *t = NULL;
    IDirect3DDevice9_GetTexture(dev, i, &t);
    if (t) IDirect3DBaseTexture9_Release(t);
    return t == (IDirect3DBaseTexture9 *)tex[1];
}
static void set_vb(int i, DWORD v) { IDirect3DDevice9_SetStreamSource(dev, i, vb[v], 0, 16); }
static DWORD get_vb(int i) {
    IDirect3DVertexBuffer9 *b = NULL; UINT o, s;
    IDirect3DDevice9_GetStreamSource(dev, i, &b, &o, &s);
    if (b) IDirect3DVertexBuffer9_Release(b);
    return b == vb[1];
}
static void set_freq(int i, DWORD v) { IDirect3DDevice9_SetStreamSourceFreq(dev, i, v + 1); }
static DWORD get_freq(int i) { UINT f = 0; IDirect3DDevice9_GetStreamSourceFreq(dev, i, &f); return f - 1; }
static void set_ib(int i, DWORD v) { IDirect3DDevice9_SetIndices(dev, ib[v]); }
static DWORD get_ib(int i) {
    IDirect3DIndexBuffer9 *b = NULL;
    IDirect3DDevice9_GetIndices(dev, &b);
    if (b) IDirect3DIndexBuffer9_Release(b);
    return b == ib[1];
}
static void set_vs(int i, DWORD v) { IDirect3DDevice9_SetVertexShader(dev, vs[v]); }
static DWORD get_vs(int i) {
    IDirect3DVertexShader9 *s = NULL;
    IDirect3DDevice9_GetVertexShader(dev, &s);
    if (s) IDirect3DVertexShader9_Release(s);
    return s == vs[1];
}
static void set_ps(int i, DWORD v) { IDirect3DDevice9_SetPixelShader(dev, ps[v]); }
static DWORD get_ps(int i) {
    IDirect3DPixelShader9 *s = NULL;
    IDirect3DDevice9_GetPixelShader(dev, &s);
    if (s) IDirect3DPixelShader9_Release(s);
    return s == ps[1];
}
static void set_decl(int i, DWORD v) { IDirect3DDevice9_SetVertexDeclaration(dev, decl[v]); }
static DWORD get_decl(int i) {
    IDirect3DVertexDeclaration9 *d = NULL;
    IDirect3DDevice9_GetVertexDeclaration(dev, &d);
    if (d) IDirect3DVertexDeclaration9_Release(d);
    return d == decl[1];
}
static void set_fvf(int i, DWORD v) { IDirect3DDevice9_SetFVF(dev, v ? D3DFVF_XYZRHW : D3DFVF_XYZ); }
static DWORD get_fvf(int i) { DWORD f = 0; IDirect3DDevice9_GetFVF(dev, &f); return f == D3DFVF_XYZRHW; }
static void set_vsf(int i, DWORD v) { float f[4] = { (float)v, 0, 0, 0 }; IDirect3DDevice9_SetVertexShaderConstantF(dev, i, f, 1); }
static DWORD get_vsf(int i) { float f[4] = { 0 }; IDirect3DDevice9_GetVertexShaderConstantF(dev, i, f, 1); return (DWORD)f[0]; }
static void set_psf(int i, DWORD v) { float f[4] = { (float)v, 0, 0, 0 }; IDirect3DDevice9_SetPixelShaderConstantF(dev, i, f, 1); }
static DWORD get_psf(int i) { float f[4] = { 0 }; IDirect3DDevice9_GetPixelShaderConstantF(dev, i, f, 1); return (DWORD)f[0]; }
static void set_vsi(int i, DWORD v) { int n[4] = { (int)v, 0, 0, 0 }; IDirect3DDevice9_SetVertexShaderConstantI(dev, i, n, 1); }
static DWORD get_vsi(int i) { int n[4] = { 0 }; IDirect3DDevice9_GetVertexShaderConstantI(dev, i, n, 1); return n[0]; }
static void set_psi(int i, DWORD v) { int n[4] = { (int)v, 0, 0, 0 }; IDirect3DDevice9_SetPixelShaderConstantI(dev, i, n, 1); }
static DWORD get_psi(int i) { int n[4] = { 0 }; IDirect3DDevice9_GetPixelShaderConstantI(dev, i, n, 1); return n[0]; }
static void set_vsb(int i, DWORD v) { BOOL b = v; IDirect3DDevice9_SetVertexShaderConstantB(dev, i, &b, 1); }
static DWORD get_vsb(int i) { BOOL b = 0; IDirect3DDevice9_GetVertexShaderConstantB(dev, i, &b, 1); return b; }
static void set_psb(int i, DWORD v) { BOOL b = v; IDirect3DDevice9_SetPixelShaderConstantB(dev, i, &b, 1); }
static DWORD get_psb(int i) { BOOL b = 0; IDirect3DDevice9_GetPixelShaderConstantB(dev, i, &b, 1); return b; }
static void set_xf(int i, DWORD v) { D3DMATRIX m = { { { (float)v } } }; IDirect3DDevice9_SetTransform(dev, i, &m); }
static DWORD get_xf(int i) { D3DMATRIX m; IDirect3DDevice9_GetTransform(dev, i, &m); return (DWORD)m._11; }
static void set_vp(int i, DWORD v) { D3DVIEWPORT9 p = { v, 0, 32, 32, 0, 1 }; IDirect3DDevice9_SetViewport(dev, &p); }
static DWORD get_vp(int i) { D3DVIEWPORT9 p; IDirect3DDevice9_GetViewport(dev, &p); return p.X; }
static void set_sc(int i, DWORD v) { RECT r = { v, 0, 32, 32 }; IDirect3DDevice9_SetScissorRect(dev, &r); }
static DWORD get_sc(int i) { RECT r; IDirect3DDevice9_GetScissorRect(dev, &r); return r.left; }
static void set_mat(int i, DWORD v) { D3DMATERIAL9 m = { 0 }; m.Power = (float)v; IDirect3DDevice9_SetMaterial(dev, &m); }
static DWORD get_mat(int i) { D3DMATERIAL9 m; IDirect3DDevice9_GetMaterial(dev, &m); return (DWORD)m.Power; }
static void set_light(int i, DWORD v) { D3DLIGHT9 l = { D3DLIGHT_POINT }; l.Range = (float)v + 1; IDirect3DDevice9_SetLight(dev, i, &l); }
static DWORD get_light(int i) { D3DLIGHT9 l = { 0 }; IDirect3DDevice9_GetLight(dev, i, &l); return (DWORD)l.Range - 1; }
static void set_lon(int i, DWORD v) { IDirect3DDevice9_LightEnable(dev, i, v); }
static DWORD get_lon(int i) { BOOL b = 0; IDirect3DDevice9_GetLightEnable(dev, i, &b); return !!b; }
static void set_clip(int i, DWORD v) { float p[4] = { (float)v, 0, 0, 0 }; IDirect3DDevice9_SetClipPlane(dev, i, p); }
static DWORD get_clip(int i) { float p[4] = { 0 }; IDirect3DDevice9_GetClipPlane(dev, i, p); return (DWORD)p[0]; }
static void set_npatch(int i, DWORD v) { IDirect3DDevice9_SetNPatchMode(dev, (float)v); }
static DWORD get_npatch(int i) { return (DWORD)IDirect3DDevice9_GetNPatchMode(dev); }

static const DWORD vs_code[] = { 0xfffe0200, 0x0200001f, 0x80000000, 0x900f0000, 0x02000001, 0xc00f0000, 0x90e40000, 0x0000ffff };
static const DWORD ps_code[] = { 0xffff0200, 0x02000001, 0x800f0800, 0xa0e40000, 0x0000ffff };

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "d3d9sb", WS_POPUP, 0, 0, 64, 64, 0, 0, wc.hInstance, 0);
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) { puts("skip no Direct3D 9"); return 0; }
    D3DPRESENT_PARAMETERS pp = { 0 };
    pp.BackBufferWidth = 64; pp.BackBufferHeight = 64; pp.BackBufferFormat = D3DFMT_X8R8G8B8; pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD; pp.hDeviceWindow = hwnd; pp.Windowed = TRUE;
    HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev);
    if (FAILED(hr)) { printf("skip CreateDevice %#lx\n", hr); return 0; }
    D3DVERTEXELEMENT9 el[] = { { 0, 0, D3DDECLTYPE_FLOAT4, 0, D3DDECLUSAGE_POSITION, 0 }, D3DDECL_END() };
    for (int k = 0; k < 2; k++) {
        IDirect3DDevice9_CreateTexture(dev, 4, 4, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &tex[k], NULL);
        IDirect3DDevice9_CreateVertexBuffer(dev, 64, 0, 0, D3DPOOL_MANAGED, &vb[k], NULL);
        IDirect3DDevice9_CreateIndexBuffer(dev, 64, 0, D3DFMT_INDEX16, D3DPOOL_MANAGED, &ib[k], NULL);
        IDirect3DDevice9_CreateVertexShader(dev, vs_code, &vs[k]);
        IDirect3DDevice9_CreatePixelShader(dev, ps_code, &ps[k]);
        IDirect3DDevice9_CreateVertexDeclaration(dev, el, &decl[k]);
    }

    /* every render state D3D9 defines */
    for (int i = 0; i < 256; i++) {
        DWORD o = get_rs(i);
        if (FAILED(IDirect3DDevice9_SetRenderState(dev, i, o))) continue;
        if (i == D3DRS_ZENABLE || i == D3DRS_FILLMODE || i == D3DRS_SHADEMODE || i == D3DRS_CULLMODE) held("rs", i, set_rs, get_rs, 1, 2);
        else held("rs", i, set_rs, get_rs, o ^ 1, o ^ 2);
        set_rs(i, o);
    }
    /* texture stage states (stages 0 and 7), sampler states (0, 15, the displacement map
     * sampler, vertex texture sampler 0) */
    for (int st = 0; st < 8; st += 7)
        for (int i = 1; i <= 32; i++) {
            DWORD o = get_tss(st * 100 + i);
            held("tss", st * 100 + i, set_tss, get_tss, o ^ 1, o ^ 2);
            set_tss(st * 100 + i, o);
        }
    for (int s = 0; s < 18; s += s == 0 ? 15 : 1)
        for (int i = 1; i <= 13; i++) {
            DWORD o = get_ss(s * 100 + i);
            held("ss", s * 100 + i, set_ss, get_ss, o ^ 1, o ^ 2);
            set_ss(s * 100 + i, o);
        }
    held("texture", 0, set_tex, get_tex, 1, 0);
    held("texture", 15, set_tex, get_tex, 1, 0);
    held("texture", D3DDMAPSAMPLER, set_tex, get_tex, 1, 0);
    held("texture", D3DVERTEXTEXTURESAMPLER0, set_tex, get_tex, 1, 0);
    held("stream", 0, set_vb, get_vb, 1, 0);
    held("freq", 0, set_freq, get_freq, 1, 0);
    held("indices", 0, set_ib, get_ib, 1, 0);
    held("vs", 0, set_vs, get_vs, 1, 0);
    held("ps", 0, set_ps, get_ps, 1, 0);
    held("decl", 0, set_decl, get_decl, 1, 0);
    held("fvf", 0, set_fvf, get_fvf, 1, 0);
    held("vsf", 0, set_vsf, get_vsf, 3, 4);
    held("vsf", 255, set_vsf, get_vsf, 3, 4);
    held("psf", 0, set_psf, get_psf, 3, 4);
    held("psf", 223, set_psf, get_psf, 3, 4);
    held("vsi", 0, set_vsi, get_vsi, 3, 4);
    held("psi", 15, set_psi, get_psi, 3, 4);
    held("vsb", 0, set_vsb, get_vsb, 1, 0);
    held("psb", 15, set_psb, get_psb, 1, 0);
    held("transform", D3DTS_VIEW, set_xf, get_xf, 3, 4);
    held("transform", D3DTS_PROJECTION, set_xf, get_xf, 3, 4);
    held("transform", D3DTS_TEXTURE0, set_xf, get_xf, 3, 4);
    held("transform", D3DTS_WORLD, set_xf, get_xf, 3, 4);
    held("transform", D3DTS_WORLDMATRIX(5), set_xf, get_xf, 3, 4);
    held("viewport", 0, set_vp, get_vp, 3, 4);
    held("scissor", 0, set_sc, get_sc, 3, 4);
    held("material", 0, set_mat, get_mat, 3, 4);
    held("light", 2, set_light, get_light, 3, 4);
    held("lightenable", 2, set_lon, get_lon, 1, 0);
    held("clipplane", 1, set_clip, get_clip, 3, 4);
    held("npatch", 0, set_npatch, get_npatch, 3, 4);
    /* a light first set after the block was made: still there after Apply? */
    {
        IDirect3DStateBlock9 *sb = NULL;
        IDirect3DDevice9_CreateStateBlock(dev, D3DSBT_ALL, &sb);
        set_light(6, 5);
        IDirect3DStateBlock9_Apply(sb);
        D3DLIGHT9 l = { 0 };
        hr = IDirect3DDevice9_GetLight(dev, 6, &l);
        printf("light made after the block: GetLight %#lx range %d\n", hr, (int)l.Range);
        IDirect3DStateBlock9_Release(sb);
    }

    /* recording */
    {
        IDirect3DStateBlock9 *sb = NULL, *sb2 = NULL;
        set_rs(D3DRS_ZENABLE, 0); set_rs(D3DRS_ALPHAREF, 10); set_tex(0, 0);
        printf("BeginStateBlock %#lx\n", IDirect3DDevice9_BeginStateBlock(dev));
        printf("BeginStateBlock again %#lx\n", IDirect3DDevice9_BeginStateBlock(dev));
        printf("CreateStateBlock while recording %#lx\n", IDirect3DDevice9_CreateStateBlock(dev, D3DSBT_ALL, &sb2));
        set_rs(D3DRS_ZENABLE, 1);
        printf("while recording: ZENABLE reads %lu\n", get_rs(D3DRS_ZENABLE));
        ULONG before = refs(tex[1]);
        set_tex(0, 1);
        printf("while recording: texture 0 is the new one %lu, its refs +%ld\n", get_tex(0), (long)refs(tex[1]) - (long)before);
        printf("EndStateBlock %#lx\n", IDirect3DDevice9_EndStateBlock(dev, &sb));
        printf("after recording: ZENABLE %lu texture 0 is the new one %lu, its refs +%ld\n", get_rs(D3DRS_ZENABLE), get_tex(0), (long)refs(tex[1]) - (long)before);
        printf("EndStateBlock again %#lx\n", IDirect3DDevice9_EndStateBlock(dev, &sb2));
        set_rs(D3DRS_ZENABLE, 2); set_rs(D3DRS_ALPHAREF, 20); set_tex(0, 0);
        IDirect3DStateBlock9_Apply(sb);
        printf("Apply recorded: ZENABLE %lu ALPHAREF %lu texture 0 is the new one %lu\n", get_rs(D3DRS_ZENABLE), get_rs(D3DRS_ALPHAREF), get_tex(0));
        /* Capture takes the recorded states only */
        set_rs(D3DRS_ZENABLE, 0); set_rs(D3DRS_ALPHAREF, 30); set_tex(0, 0);
        IDirect3DStateBlock9_Capture(sb);
        set_rs(D3DRS_ZENABLE, 2); set_rs(D3DRS_ALPHAREF, 40); set_tex(0, 1);
        IDirect3DStateBlock9_Apply(sb);
        printf("Capture, Apply recorded: ZENABLE %lu ALPHAREF %lu texture 0 is the new one %lu\n", get_rs(D3DRS_ZENABLE), get_rs(D3DRS_ALPHAREF), get_tex(0));
        set_tex(0, 1);
        before = refs(tex[1]);
        IDirect3DStateBlock9_Capture(sb);
        printf("Capture: refs of the texture it took +%ld\n", (long)refs(tex[1]) - (long)before);
        set_tex(0, 0);
        before = refs(tex[1]);
        ULONG left = IDirect3DStateBlock9_Release(sb);
        printf("Release recorded %lu, refs of its texture %+ld\n", left, (long)refs(tex[1]) - (long)before);
        /* a block that recorded nothing; one that recorded a state set twice */
        IDirect3DDevice9_BeginStateBlock(dev);
        printf("EndStateBlock of nothing %#lx\n", IDirect3DDevice9_EndStateBlock(dev, &sb));
        set_rs(D3DRS_ALPHAREF, 50);
        if (sb) { IDirect3DStateBlock9_Apply(sb); IDirect3DStateBlock9_Release(sb); }
        printf("Apply of nothing: ALPHAREF %lu\n", get_rs(D3DRS_ALPHAREF));
        IDirect3DDevice9_BeginStateBlock(dev);
        set_rs(D3DRS_ALPHAREF, 60); set_rs(D3DRS_ALPHAREF, 61);
        set_vsf(7, 9);
        IDirect3DDevice9_EndStateBlock(dev, &sb);
        set_rs(D3DRS_ALPHAREF, 70); set_vsf(7, 1); set_vsf(8, 1);
        IDirect3DStateBlock9_Apply(sb);
        printf("Apply of a state set twice: ALPHAREF %lu vsf 7 %lu vsf 8 %lu\n", get_rs(D3DRS_ALPHAREF), get_vsf(7), get_vsf(8));
        IDirect3DStateBlock9_Release(sb);
        /* a texture released while a recorded block holds it */
        IDirect3DTexture9 *t = NULL;
        IDirect3DDevice9_CreateTexture(dev, 4, 4, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t, NULL);
        IDirect3DDevice9_BeginStateBlock(dev);
        IDirect3DDevice9_SetTexture(dev, 1, (IDirect3DBaseTexture9 *)t);
        IDirect3DDevice9_EndStateBlock(dev, &sb);
        printf("Release of a texture a recorded block holds %lu\n", IDirect3DTexture9_Release(t));
        IDirect3DDevice9_SetTexture(dev, 1, NULL);
        IDirect3DStateBlock9_Apply(sb);
        IDirect3DBaseTexture9 *g = NULL;
        IDirect3DDevice9_GetTexture(dev, 1, &g);
        printf("Apply: texture 1 is it %d\n", g == (IDirect3DBaseTexture9 *)t);
        if (g) IDirect3DBaseTexture9_Release(g);
        IDirect3DDevice9_SetTexture(dev, 1, NULL);
        printf("Release of the block %lu\n", IDirect3DStateBlock9_Release(sb));
    }
    /* out of order */
    printf("EndStateBlock without Begin %#lx\n", IDirect3DDevice9_EndStateBlock(dev, NULL));
    {
        IDirect3DStateBlock9 *sb = NULL;
        printf("CreateStateBlock type 0 %#lx\n", IDirect3DDevice9_CreateStateBlock(dev, 0, &sb));
        if (sb) IDirect3DStateBlock9_Release(sb), sb = NULL;
        printf("CreateStateBlock type 4 %#lx\n", IDirect3DDevice9_CreateStateBlock(dev, 4, &sb));
        if (sb) IDirect3DStateBlock9_Release(sb);
    }
    for (int k = 0; k < 2; k++) {
        IDirect3DTexture9_Release(tex[k]);
        IDirect3DVertexBuffer9_Release(vb[k]);
        IDirect3DIndexBuffer9_Release(ib[k]);
        IDirect3DVertexShader9_Release(vs[k]);
        IDirect3DPixelShader9_Release(ps[k]);
        IDirect3DVertexDeclaration9_Release(decl[k]);
    }
    printf("Release device %lu\n", IDirect3DDevice9_Release(dev));
    IDirect3D9_Release(d3d);
    return 0;
}
