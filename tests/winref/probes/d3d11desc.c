/* Direct3D 11 state object descriptions: which blend, depth-stencil, rasterizer and sampler
 * descriptions CreateXState accepts (each field out of range, fields of a disabled part out
 * of range), what GetDesc gives back (the runtime's normalized description), and which
 * descriptions give the same object (dedup by the normalized description).
 * Each line: "<state> <case> <hr> [desc words] same <first case with that object or ->".
 * libs: -ld3d11
 * arch: 32
 * run: browser (sake's Direct3D 11 runs in the page: tests/winref/run-browser.mjs) */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <d3d11.h>

static ID3D11Device *dev;

struct change { int at; DWORD v; };
struct test { const char *name; struct change c[5]; };
#define END { -1, 0 }

static void *objs[64];
static const char *names[64];

static void run(const char *what, int kind, const DWORD *base, int n, const struct test *t, int count) {
    for (int i = 0; i < count && i < 64; i++) {
        DWORD d[66], got[66];
        memcpy(d, base, n * 4);
        for (int k = 0; k < 5 && t[i].c[k].at >= 0; k++) d[t[i].c[k].at] = t[i].c[k].v;
        void *o = NULL;
        HRESULT hr;
        memset(got, 0xcc, sizeof got);
        switch (kind) {
        case 0:
            hr = ID3D11Device_CreateBlendState(dev, (D3D11_BLEND_DESC *)d, (ID3D11BlendState **)&o);
            if (o) ID3D11BlendState_GetDesc((ID3D11BlendState *)o, (D3D11_BLEND_DESC *)got);
            break;
        case 1:
            hr = ID3D11Device_CreateDepthStencilState(dev, (D3D11_DEPTH_STENCIL_DESC *)d, (ID3D11DepthStencilState **)&o);
            if (o) ID3D11DepthStencilState_GetDesc((ID3D11DepthStencilState *)o, (D3D11_DEPTH_STENCIL_DESC *)got);
            break;
        case 2:
            hr = ID3D11Device_CreateRasterizerState(dev, (D3D11_RASTERIZER_DESC *)d, (ID3D11RasterizerState **)&o);
            if (o) ID3D11RasterizerState_GetDesc((ID3D11RasterizerState *)o, (D3D11_RASTERIZER_DESC *)got);
            break;
        default:
            hr = ID3D11Device_CreateSamplerState(dev, (D3D11_SAMPLER_DESC *)d, (ID3D11SamplerState **)&o);
            if (o) ID3D11SamplerState_GetDesc((ID3D11SamplerState *)o, (D3D11_SAMPLER_DESC *)got);
            break;
        }
        char line[1024];
        int at = snprintf(line, sizeof line, "%s %s %#lx", what, t[i].name, (unsigned long)hr);
        const char *same = "-";
        if (o) {
            /* the bytes GetDesc fills (UINT8 fields: their padding stays 0xcc) */
            for (int k = 0; k < n; k++) {
                DWORD w = got[k];
                if (kind == 0 && k >= 2 && (k - 2) % 8 == 7) w &= 0xff;
                if (kind == 1 && k == 4) w &= 0xffff;
                at += snprintf(line + at, sizeof line - at, " %lx", (unsigned long)w);
            }
            for (int j = 0; j < i; j++)
                if (objs[j] == o) { same = names[j]; break; }
        }
        objs[i] = o;
        names[i] = t[i].name;
        printf("%s same %s\n", line, same);
    }
    for (int i = 0; i < count && i < 64; i++)
        if (objs[i]) IUnknown_Release((IUnknown *)objs[i]);
}

/* D3D11_BLEND_DESC: AlphaToCoverage, IndependentBlend, 8 x { BlendEnable, Src, Dest, Op,
 * SrcAlpha, DestAlpha, OpAlpha, WriteMask (UINT8) } */
#define RT(i, f) (2 + 8 * (i) + (f))
static const struct test blends[] = {
    { "base", { END } },
    { "base_again", { END } },
    { "src0", { { RT(0, 1), 0 }, END } },
    { "src_srccolor", { { RT(0, 1), 3 }, END } },
    { "src12", { { RT(0, 1), 12 }, END } },
    { "src13", { { RT(0, 1), 13 }, END } },
    { "src14", { { RT(0, 1), 14 }, END } },
    { "src19", { { RT(0, 1), 19 }, END } },
    { "src20", { { RT(0, 1), 20 }, END } },
    { "dest0", { { RT(0, 2), 0 }, END } },
    { "op0", { { RT(0, 3), 0 }, END } },
    { "op5", { { RT(0, 3), 5 }, END } },
    { "op6", { { RT(0, 3), 6 }, END } },
    { "srcalpha_srccolor", { { RT(0, 4), 3 }, END } },
    { "srcalpha_invdestcolor", { { RT(0, 4), 10 }, END } },
    { "srcalpha_blendfactor", { { RT(0, 4), 14 }, END } },
    { "srcalpha_src1color", { { RT(0, 4), 16 }, END } },
    { "destalpha_srccolor", { { RT(0, 5), 3 }, END } },
    { "opalpha0", { { RT(0, 6), 0 }, END } },
    { "mask0", { { RT(0, 7), 0 }, END } },
    { "mask10", { { RT(0, 7), 0x10 }, END } },
    { "mask_ff", { { RT(0, 7), 0xff }, END } },
    { "enable2", { { RT(0, 0), 2 }, END } },
    { "disabled", { { RT(0, 0), 0 }, END } },
    { "disabled_src0", { { RT(0, 0), 0 }, { RT(0, 1), 0 }, { RT(0, 3), 0 }, END } },
    { "disabled_src20", { { RT(0, 0), 0 }, { RT(0, 1), 20 }, END } },
    { "disabled_mask10", { { RT(0, 0), 0 }, { RT(0, 7), 0x10 }, END } },
    { "rt1_bad", { { RT(1, 0), 1 }, { RT(1, 1), 0 }, { RT(1, 7), 0x10 }, END } },
    { "rt7_other", { { RT(7, 0), 1 }, { RT(7, 1), 2 }, { RT(7, 2), 2 }, { RT(7, 3), 1 }, END } },
    { "indep", { { 1, 1 }, END } },
    { "indep_rt1_bad", { { 1, 1 }, { RT(1, 0), 1 }, { RT(1, 1), 0 }, END } },
    { "indep_rt1_badmask", { { 1, 1 }, { RT(1, 7), 0x10 }, END } },
    { "indep_rt1_src20_disabled", { { 1, 1 }, { RT(1, 1), 20 }, END } },
    { "indep_rt1_as_rt0", { { 1, 1 }, { RT(1, 0), 1 }, { RT(1, 1), 5 }, { RT(1, 2), 6 }, END } },
    { "a2c", { { 0, 1 }, END } },
    { "a2c2", { { 0, 2 }, END } },
    { "indep2", { { 1, 2 }, END } },
};

/* D3D11_DEPTH_STENCIL_DESC: DepthEnable, WriteMask, Func, StencilEnable, Read | Write << 8,
 * Front { Fail, DepthFail, Pass, Func }, Back { ... } */
static const struct test depths[] = {
    { "base", { END } },
    { "write2", { { 1, 2 }, END } },
    { "write0", { { 1, 0 }, END } },
    { "func0", { { 2, 0 }, END } },
    { "func8", { { 2, 8 }, END } },
    { "func9", { { 2, 9 }, END } },
    { "enable2", { { 0, 2 }, END } },
    { "off", { { 0, 0 }, END } },
    { "off_func0", { { 0, 0 }, { 2, 0 }, END } },
    { "off_func9", { { 0, 0 }, { 2, 9 }, END } },
    { "off_write0_func5", { { 0, 0 }, { 1, 0 }, { 2, 5 }, END } },
    { "off_write2", { { 0, 0 }, { 1, 2 }, END } },
    { "stencil", { { 3, 1 }, END } },
    { "stencil2", { { 3, 2 }, END } },
    { "stencil_fail0", { { 3, 1 }, { 5, 0 }, END } },
    { "stencil_fail9", { { 3, 1 }, { 5, 9 }, END } },
    { "stencil_dfail0", { { 3, 1 }, { 6, 0 }, END } },
    { "stencil_pass9", { { 3, 1 }, { 7, 9 }, END } },
    { "stencil_func0", { { 3, 1 }, { 8, 0 }, END } },
    { "stencil_func9", { { 3, 1 }, { 8, 9 }, END } },
    { "stencil_back_func0", { { 3, 1 }, { 12, 0 }, END } },
    { "stencil_masks", { { 3, 1 }, { 4, 0x0f3c }, END } },
    { "nostencil_fail0", { { 5, 0 }, END } },
    { "nostencil_func9", { { 8, 9 }, END } },
    { "nostencil_masks", { { 4, 0x0f3c }, END } },
    { "nostencil_ops", { { 5, 2 }, { 7, 3 }, { 8, 4 }, END } },
    { "off_nostencil_all", { { 0, 0 }, { 2, 0 }, { 4, 0 }, { 8, 0 }, END } },
};

/* D3D11_RASTERIZER_DESC: Fill, Cull, FrontCCW, DepthBias, BiasClamp, SlopeScaled, DepthClip,
 * Scissor, Multisample, AntialiasedLine */
static const struct test rasters[] = {
    { "base", { END } },
    { "fill0", { { 0, 0 }, END } },
    { "fill1", { { 0, 1 }, END } },
    { "fill2", { { 0, 2 }, END } },
    { "fill4", { { 0, 4 }, END } },
    { "cull0", { { 1, 0 }, END } },
    { "cull1", { { 1, 1 }, END } },
    { "cull4", { { 1, 4 }, END } },
    { "ccw2", { { 2, 2 }, END } },
    { "bias", { { 3, (DWORD)-5 }, END } },
    { "clamp_nan", { { 4, 0x7fc00000 }, END } },
    { "slope_inf", { { 5, 0x7f800000 }, END } },
    { "noclip", { { 6, 0 }, END } },
    { "clip2", { { 6, 2 }, END } },
    { "scissor", { { 7, 1 }, END } },
    { "ms_aa", { { 8, 1 }, { 9, 1 }, END } },
    { "aa2", { { 9, 2 }, END } },
};

/* D3D11_SAMPLER_DESC: Filter, U, V, W, MipLODBias, MaxAnisotropy, ComparisonFunc,
 * Border[4], MinLOD, MaxLOD */
static const struct test samplers[] = {
    { "base", { END } },
    { "point", { { 0, 0 }, END } },
    { "filter16", { { 0, 0x16 }, END } },
    { "filter2", { { 0, 2 }, END } },
    { "aniso", { { 0, 0x55 }, END } },
    { "aniso_no_mip", { { 0, 0x54 }, END } },
    { "cmp_point", { { 0, 0x80 }, END } },
    { "cmp_linear", { { 0, 0x95 }, END } },
    { "cmp_aniso", { { 0, 0xd5 }, END } },
    { "min_linear", { { 0, 0x115 }, END } },
    { "max_linear", { { 0, 0x195 }, END } },
    { "max_aniso", { { 0, 0x1d5 }, END } },
    { "filter200", { { 0, 0x200 }, END } },
    { "addr0", { { 1, 0 }, END } },
    { "addr5", { { 2, 5 }, END } },
    { "addr6", { { 3, 6 }, END } },
    { "aniso0", { { 5, 0 }, END } },
    { "aniso16", { { 5, 16 }, END } },
    { "aniso17", { { 5, 17 }, END } },
    { "anisof_0", { { 0, 0x55 }, { 5, 0 }, END } },
    { "anisof_16", { { 0, 0x55 }, { 5, 16 }, END } },
    { "anisof_17", { { 0, 0x55 }, { 5, 17 }, END } },
    { "cmp0", { { 6, 0 }, END } },
    { "cmp9", { { 6, 9 }, END } },
    { "cmpf_0", { { 0, 0x95 }, { 6, 0 }, END } },
    { "cmpf_9", { { 0, 0x95 }, { 6, 9 }, END } },
    { "cmpf_less", { { 0, 0x95 }, { 6, 2 }, END } },
    { "lod_swapped", { { 11, 0x40a00000 }, { 12, 0x3f800000 }, END } },
    { "lod_neg", { { 11, 0xc0a00000 }, { 12, 0xc0000000 }, END } },
    { "lod_nan", { { 11, 0x7fc00000 }, END } },
    { "bias20", { { 4, 0x41a00000 }, END } },
    { "bias_neg20", { { 4, 0xc1a00000 }, END } },
    { "border", { { 7, 0x3f800000 }, { 10, 0x3f000000 }, END } },
    { "border_nan", { { 7, 0x7fc00000 }, END } },
    { "nonborder_color", { { 8, 0x3f800000 }, END } },
};

#define N(a) (int)(sizeof a / sizeof *a)

int main(void) {
    D3D_FEATURE_LEVEL fl = D3D_FEATURE_LEVEL_11_0;
    HRESULT hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, &fl, 1, D3D11_SDK_VERSION, &dev, NULL, NULL);
    if (FAILED(hr)) { printf("skip D3D11CreateDevice %#lx\n", (unsigned long)hr); return 0; }
    DWORD blend[66] = { 0, 0 };
    DWORD rt0[8] = { 1, 5, 6, 1, 2, 1, 1, 0xf };
    memcpy(blend + 2, rt0, sizeof rt0);
    run("blend", 0, blend, 66, blends, N(blends));
    DWORD depth[13] = { 1, 1, 2, 0, 0xffff, 1, 1, 1, 8, 1, 1, 1, 8 };
    run("depth", 1, depth, 13, depths, N(depths));
    DWORD raster[10] = { 3, 3, 0, 0, 0, 0, 1, 0, 0, 0 };
    run("raster", 2, raster, 10, rasters, N(rasters));
    DWORD sampler[13] = { 0x15, 1, 1, 1, 0, 1, 1, 0, 0, 0, 0, 0, 0x7f7fffff };
    run("sampler", 3, sampler, 13, samplers, N(samplers));
    {
        void *o = (void *)1;
        printf("null desc blend %#lx depth %#lx raster %#lx sampler %#lx\n",
               (unsigned long)ID3D11Device_CreateBlendState(dev, NULL, (ID3D11BlendState **)&o),
               (unsigned long)ID3D11Device_CreateDepthStencilState(dev, NULL, (ID3D11DepthStencilState **)&o),
               (unsigned long)ID3D11Device_CreateRasterizerState(dev, NULL, (ID3D11RasterizerState **)&o),
               (unsigned long)ID3D11Device_CreateSamplerState(dev, NULL, (ID3D11SamplerState **)&o));
        printf("null out blend %#lx depth %#lx raster %#lx sampler %#lx\n",
               (unsigned long)ID3D11Device_CreateBlendState(dev, (D3D11_BLEND_DESC *)blend, NULL),
               (unsigned long)ID3D11Device_CreateDepthStencilState(dev, (D3D11_DEPTH_STENCIL_DESC *)depth, NULL),
               (unsigned long)ID3D11Device_CreateRasterizerState(dev, (D3D11_RASTERIZER_DESC *)raster, NULL),
               (unsigned long)ID3D11Device_CreateSamplerState(dev, (D3D11_SAMPLER_DESC *)sampler, NULL));
    }
    printf("release device %lu\n", ID3D11Device_Release(dev));
    return 0;
}
