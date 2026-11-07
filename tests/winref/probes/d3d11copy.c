/* Direct3D 11 (feature level 11_0; WARP on the Windows runner): what CopyResource and
 * CopySubresourceRegion give between two formats (every pair of color formats of one texel
 * size, inside and across typeless groups: the bits, or nothing), regions with offsets, depth
 * formats; which input layouts CreateInputLayout takes for a shader's input signature; the
 * newer state descriptions (ID3D11RasterizerState1/2, ID3D11BlendState1) as GetDesc1/2 give
 * them back; every subresource (array layers, mips, 3D slices) of textures of formats a GPU may
 * keep converted (16-bit UNORM/SNORM, 5:6:5, A8, block-compressed, 32-bit float) through
 * CopyResource, CopySubresourceRegion and UpdateSubresource boxes, as Map gives them back.
 * libs: -ld3d11 -ld3dcompiler_47
 * arch: 32
 * run: browser (sake's Direct3D 11 runs in the page: tests/winref/run-browser.mjs) */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <d3d11_3.h>
#include <d3dcompiler.h>

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;

static const char *code(HRESULT hr) {
    static char b[16];
    if (hr == S_OK) return "ok";
    if (hr == S_FALSE) return "false";
    if (hr == E_INVALIDARG) return "inval";
    if (hr == E_FAIL) return "fail";
    if (hr == E_NOINTERFACE) return "noif";
    snprintf(b, sizeof b, "%#lx", (unsigned long)hr);
    return b;
}

/* ---- copies ---- */

#define TW 4
#define TH 4

/* texel bytes chosen to be the bits of every kind of value: zero, ones, signs, NaNs, infinities,
 * denormals, sRGB-sensitive mid values */
static unsigned char bits[TW * TH * 16];

static ID3D11Texture2D *tex(DXGI_FORMAT f, D3D11_USAGE usage, UINT bind, UINT cpu, int bpp, const void *data) {
    D3D11_TEXTURE2D_DESC d = {TW, TH, 1, 1, f, {1, 0}, usage, bind, cpu, 0};
    D3D11_SUBRESOURCE_DATA sd = {data, TW * bpp, 0};
    ID3D11Texture2D *t = NULL;
    if (FAILED(ID3D11Device_CreateTexture2D(dev, &d, data ? &sd : NULL, &t))) return NULL;
    return t;
}

/* the texture's bytes as hex (through a staging copy of its own format), or "-" */
static void dump(char *out, ID3D11Texture2D *t, DXGI_FORMAT f, int bpp) {
    ID3D11Texture2D *s = tex(f, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ, bpp, NULL);
    strcpy(out, "-");
    if (!s) return;
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)s, (ID3D11Resource *)t);
    D3D11_MAPPED_SUBRESOURCE m;
    if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)s, 0, D3D11_MAP_READ, 0, &m))) {
        char *o = out;
        for (int y = 0; y < TH; y++) {
            const unsigned char *p = (const unsigned char *)m.pData + y * m.RowPitch;
            for (int x = 0; x < TW * bpp; x++) o += sprintf(o, "%02x", p[x]);
            if (y < TH - 1) *o++ = '.';
        }
        *o = 0;
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)s, 0);
    }
    ID3D11Texture2D_Release(s);
}

static const struct { DXGI_FORMAT f; const char *n; int bpp; } fmts[] = {
    {DXGI_FORMAT_R8G8B8A8_TYPELESS, "R8G8B8A8_TYPELESS", 4}, {DXGI_FORMAT_R8G8B8A8_UNORM, "R8G8B8A8_UNORM", 4},
    {DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, "R8G8B8A8_UNORM_SRGB", 4}, {DXGI_FORMAT_R8G8B8A8_UINT, "R8G8B8A8_UINT", 4},
    {DXGI_FORMAT_R8G8B8A8_SNORM, "R8G8B8A8_SNORM", 4}, {DXGI_FORMAT_R8G8B8A8_SINT, "R8G8B8A8_SINT", 4},
    {DXGI_FORMAT_B8G8R8A8_UNORM, "B8G8R8A8_UNORM", 4}, {DXGI_FORMAT_B8G8R8A8_TYPELESS, "B8G8R8A8_TYPELESS", 4},
    {DXGI_FORMAT_B8G8R8A8_UNORM_SRGB, "B8G8R8A8_UNORM_SRGB", 4}, {DXGI_FORMAT_B8G8R8X8_UNORM, "B8G8R8X8_UNORM", 4},
    {DXGI_FORMAT_R16G16_TYPELESS, "R16G16_TYPELESS", 4}, {DXGI_FORMAT_R16G16_FLOAT, "R16G16_FLOAT", 4},
    {DXGI_FORMAT_R16G16_UNORM, "R16G16_UNORM", 4}, {DXGI_FORMAT_R16G16_UINT, "R16G16_UINT", 4},
    {DXGI_FORMAT_R16G16_SNORM, "R16G16_SNORM", 4}, {DXGI_FORMAT_R16G16_SINT, "R16G16_SINT", 4},
    {DXGI_FORMAT_R32_TYPELESS, "R32_TYPELESS", 4}, {DXGI_FORMAT_R32_FLOAT, "R32_FLOAT", 4},
    {DXGI_FORMAT_R32_UINT, "R32_UINT", 4}, {DXGI_FORMAT_R32_SINT, "R32_SINT", 4},
    {DXGI_FORMAT_R10G10B10A2_TYPELESS, "R10G10B10A2_TYPELESS", 4}, {DXGI_FORMAT_R10G10B10A2_UNORM, "R10G10B10A2_UNORM", 4},
    {DXGI_FORMAT_R10G10B10A2_UINT, "R10G10B10A2_UINT", 4}, {DXGI_FORMAT_R11G11B10_FLOAT, "R11G11B10_FLOAT", 4},
    {DXGI_FORMAT_R9G9B9E5_SHAREDEXP, "R9G9B9E5_SHAREDEXP", 4},
    {DXGI_FORMAT_R16G16B16A16_TYPELESS, "R16G16B16A16_TYPELESS", 8}, {DXGI_FORMAT_R16G16B16A16_FLOAT, "R16G16B16A16_FLOAT", 8},
    {DXGI_FORMAT_R16G16B16A16_UNORM, "R16G16B16A16_UNORM", 8}, {DXGI_FORMAT_R16G16B16A16_UINT, "R16G16B16A16_UINT", 8},
    {DXGI_FORMAT_R16G16B16A16_SNORM, "R16G16B16A16_SNORM", 8}, {DXGI_FORMAT_R16G16B16A16_SINT, "R16G16B16A16_SINT", 8},
    {DXGI_FORMAT_R32G32_TYPELESS, "R32G32_TYPELESS", 8}, {DXGI_FORMAT_R32G32_FLOAT, "R32G32_FLOAT", 8},
    {DXGI_FORMAT_R32G32_UINT, "R32G32_UINT", 8}, {DXGI_FORMAT_R32G32_SINT, "R32G32_SINT", 8},
    {DXGI_FORMAT_R32G32B32A32_TYPELESS, "R32G32B32A32_TYPELESS", 16}, {DXGI_FORMAT_R32G32B32A32_FLOAT, "R32G32B32A32_FLOAT", 16},
    {DXGI_FORMAT_R32G32B32A32_UINT, "R32G32B32A32_UINT", 16}, {DXGI_FORMAT_R32G32B32A32_SINT, "R32G32B32A32_SINT", 16},
    {DXGI_FORMAT_R8G8_TYPELESS, "R8G8_TYPELESS", 2}, {DXGI_FORMAT_R8G8_UNORM, "R8G8_UNORM", 2},
    {DXGI_FORMAT_R8G8_UINT, "R8G8_UINT", 2}, {DXGI_FORMAT_R8G8_SNORM, "R8G8_SNORM", 2}, {DXGI_FORMAT_R8G8_SINT, "R8G8_SINT", 2},
    {DXGI_FORMAT_R16_TYPELESS, "R16_TYPELESS", 2}, {DXGI_FORMAT_R16_FLOAT, "R16_FLOAT", 2}, {DXGI_FORMAT_R16_UNORM, "R16_UNORM", 2},
    {DXGI_FORMAT_R16_UINT, "R16_UINT", 2}, {DXGI_FORMAT_R16_SNORM, "R16_SNORM", 2}, {DXGI_FORMAT_R16_SINT, "R16_SINT", 2},
    {DXGI_FORMAT_B5G6R5_UNORM, "B5G6R5_UNORM", 2}, {DXGI_FORMAT_B5G5R5A1_UNORM, "B5G5R5A1_UNORM", 2},
    {DXGI_FORMAT_R8_TYPELESS, "R8_TYPELESS", 1}, {DXGI_FORMAT_R8_UNORM, "R8_UNORM", 1}, {DXGI_FORMAT_R8_UINT, "R8_UINT", 1},
    {DXGI_FORMAT_R8_SNORM, "R8_SNORM", 1}, {DXGI_FORMAT_R8_SINT, "R8_SINT", 1}, {DXGI_FORMAT_A8_UNORM, "A8_UNORM", 1},
};
#define NF (int)(sizeof fmts / sizeof fmts[0])

static void copies(void) {
    static const unsigned char pat[16] = {0x00, 0xff, 0x80, 0x7f, 0x01, 0xfe, 0x7f, 0x80, 0x00, 0x00, 0xc0, 0x7f, 0x00, 0x00, 0x80, 0x7f};
    for (int i = 0; i < (int)sizeof bits; i++) bits[i] = (unsigned char)(i < 64 ? pat[i % 16] ^ (i / 16) : i * 37 + 11);
    char *a = malloc(TW * TH * 16 * 2 + 16), *b = malloc(TW * TH * 16 * 2 + 16);
    /* each format on its own: what its texels hold after the upload */
    for (int i = 0; i < NF; i++) {
        ID3D11Texture2D *s = tex(fmts[i].f, D3D11_USAGE_DEFAULT, 0, 0, fmts[i].bpp, bits);
        if (!s) {
            printf("upload %s failed\n", fmts[i].n);
            continue;
        }
        dump(a, s, fmts[i].f, fmts[i].bpp);
        printf("upload %s %s\n", fmts[i].n, a);
        ID3D11Texture2D_Release(s);
    }
    /* every pair of one texel size: CopyResource, then a 2x2 region of (1, 1) to (2, 0) */
    static const unsigned char zero[TW * TH * 16];
    for (int i = 0; i < NF; i++)
        for (int j = 0; j < NF; j++) {
            if (i == j || fmts[i].bpp != fmts[j].bpp) continue;
            ID3D11Texture2D *s = tex(fmts[i].f, D3D11_USAGE_DEFAULT, 0, 0, fmts[i].bpp, bits);
            ID3D11Texture2D *d = tex(fmts[j].f, D3D11_USAGE_DEFAULT, 0, 0, fmts[j].bpp, zero);
            ID3D11Texture2D *d2 = tex(fmts[j].f, D3D11_USAGE_DEFAULT, 0, 0, fmts[j].bpp, zero);
            if (!s || !d || !d2) {
                printf("copy %s %s failed\n", fmts[i].n, fmts[j].n);
            } else {
                ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)d, (ID3D11Resource *)s);
                D3D11_BOX box = {1, 1, 0, 3, 3, 1};
                ID3D11DeviceContext_CopySubresourceRegion(ctx, (ID3D11Resource *)d2, 0, 2, 0, 0, (ID3D11Resource *)s, 0, &box);
                dump(a, d, fmts[j].f, fmts[j].bpp);
                dump(b, d2, fmts[j].f, fmts[j].bpp);
                /* (as same / zero / other bytes: the lines stay short) */
                char *u = malloc(TW * TH * 16 * 2 + 16);
                dump(u, s, fmts[i].f, fmts[i].bpp);
                printf("copy %s %s resource %s region %s\n", fmts[i].n, fmts[j].n, !strcmp(a, u) ? "same" : a, b);
                free(u);
            }
            if (s) ID3D11Texture2D_Release(s);
            if (d) ID3D11Texture2D_Release(d);
            if (d2) ID3D11Texture2D_Release(d2);
        }
    /* depth: D16 / D32 / D24S8 to and from their color formats, whole and part */
    static const struct { DXGI_FORMAT a, b; const char *n; int bpp; } ds[] = {
        {DXGI_FORMAT_D16_UNORM, DXGI_FORMAT_R16_UNORM, "D16_UNORM R16_UNORM", 2}, {DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_D16_UNORM, "R16_UNORM D16_UNORM", 2},
        {DXGI_FORMAT_D32_FLOAT, DXGI_FORMAT_R32_FLOAT, "D32_FLOAT R32_FLOAT", 4}, {DXGI_FORMAT_R32_FLOAT, DXGI_FORMAT_D32_FLOAT, "R32_FLOAT D32_FLOAT", 4},
        {DXGI_FORMAT_D16_UNORM, DXGI_FORMAT_D16_UNORM, "D16_UNORM D16_UNORM", 2}, {DXGI_FORMAT_D32_FLOAT, DXGI_FORMAT_D32_FLOAT, "D32_FLOAT D32_FLOAT", 4},
    };
    for (int i = 0; i < (int)(sizeof ds / sizeof ds[0]); i++) {
        int sd = ds[i].a == DXGI_FORMAT_D16_UNORM || ds[i].a == DXGI_FORMAT_D32_FLOAT;
        int dd = ds[i].b == DXGI_FORMAT_D16_UNORM || ds[i].b == DXGI_FORMAT_D32_FLOAT;
        ID3D11Texture2D *s = tex(ds[i].a, D3D11_USAGE_DEFAULT, sd ? D3D11_BIND_DEPTH_STENCIL : 0, 0, ds[i].bpp, sd ? NULL : bits);
        ID3D11Texture2D *d = tex(ds[i].b, D3D11_USAGE_DEFAULT, dd ? D3D11_BIND_DEPTH_STENCIL : 0, 0, ds[i].bpp, dd ? NULL : zero);
        if (!s || !d) {
            printf("depth %s failed\n", ds[i].n);
        } else {
            if (sd) {
                /* (a depth texture's contents: a clear) */
                ID3D11DepthStencilView *v = NULL;
                ID3D11Device_CreateDepthStencilView(dev, (ID3D11Resource *)s, NULL, &v);
                if (v) {
                    ID3D11DeviceContext_ClearDepthStencilView(ctx, v, D3D11_CLEAR_DEPTH, 0.375f, 0);
                    ID3D11DepthStencilView_Release(v);
                }
            }
            ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)d, (ID3D11Resource *)s);
            dump(a, d, ds[i].b, ds[i].bpp);
            printf("depth %s %s\n", ds[i].n, a);
        }
        if (s) ID3D11Texture2D_Release(s);
        if (d) ID3D11Texture2D_Release(d);
    }
    free(a);
    free(b);
}

/* ---- subresources ---- */

static const struct { DXGI_FORMAT f, g; const char *n; int bpp, block; } subfmts[] = {
    {DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, "R8G8B8A8_UNORM", 4, 1}, {DXGI_FORMAT_R32_FLOAT, DXGI_FORMAT_R32_FLOAT, "R32_FLOAT", 4, 1},
    {DXGI_FORMAT_R16G16B16A16_UNORM, DXGI_FORMAT_R16G16B16A16_UNORM, "R16G16B16A16_UNORM", 8, 1}, {DXGI_FORMAT_R16_SNORM, DXGI_FORMAT_R16_SNORM, "R16_SNORM", 2, 1},
    {DXGI_FORMAT_B5G6R5_UNORM, DXGI_FORMAT_B5G6R5_UNORM, "B5G6R5_UNORM", 2, 1}, {DXGI_FORMAT_A8_UNORM, DXGI_FORMAT_A8_UNORM, "A8_UNORM", 1, 1},
    {DXGI_FORMAT_BC1_UNORM, DXGI_FORMAT_BC1_UNORM, "BC1_UNORM", 8, 4}, {DXGI_FORMAT_BC3_UNORM, DXGI_FORMAT_BC3_UNORM, "BC3_UNORM", 16, 4},
    {DXGI_FORMAT_BC7_UNORM, DXGI_FORMAT_BC7_UNORM, "BC7_UNORM", 16, 4}, {DXGI_FORMAT_R32G32B32A32_FLOAT, DXGI_FORMAT_R32G32B32A32_FLOAT, "R32G32B32A32_FLOAT", 16, 1},
    {DXGI_FORMAT_R16G16B16A16_UINT, DXGI_FORMAT_R16G16B16A16_UNORM, "R16G16B16A16_UINT>UNORM", 8, 1}, {DXGI_FORMAT_R16G16B16A16_UNORM, DXGI_FORMAT_R16G16B16A16_UINT, "R16G16B16A16_UNORM>UINT", 8, 1},
    {DXGI_FORMAT_R16_UINT, DXGI_FORMAT_R16_SNORM, "R16_UINT>SNORM", 2, 1}, {DXGI_FORMAT_R16_SNORM, DXGI_FORMAT_R16_UINT, "R16_SNORM>UINT", 2, 1},
    {DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_R16G16B16A16_UNORM, "R16G16B16A16_FLOAT>UNORM", 8, 1}, {DXGI_FORMAT_BC1_UNORM, DXGI_FORMAT_BC1_UNORM_SRGB, "BC1_UNORM>SRGB", 8, 4},
    {DXGI_FORMAT_R32G32B32A32_FLOAT, DXGI_FORMAT_R32G32B32A32_UINT, "R32G32B32A32_FLOAT>UINT", 16, 1},
};

/* (dimension 2: 8x8, 3 layers, 4 mips; 3: 8x4, 4 slices, 3 mips) */
static ID3D11Resource *subtex(int dim, DXGI_FORMAT f, int staging, const unsigned char *const *data, const UINT *pitch, const UINT *slice) {
    D3D11_SUBRESOURCE_DATA sd[12];
    int n = dim == 3 ? 3 : 12;
    for (int i = 0; data && i < n; i++) sd[i] = (D3D11_SUBRESOURCE_DATA){data[i], pitch[i], slice[i]};
    D3D11_USAGE u = staging ? D3D11_USAGE_STAGING : D3D11_USAGE_DEFAULT;
    UINT bind = staging ? 0 : D3D11_BIND_SHADER_RESOURCE, cpu = staging ? D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE : 0;
    if (dim == 3) {
        D3D11_TEXTURE3D_DESC d = {8, 4, 4, 3, f, u, bind, cpu, 0};
        ID3D11Texture3D *t = NULL;
        ID3D11Device_CreateTexture3D(dev, &d, data ? sd : NULL, &t);
        return (ID3D11Resource *)t;
    }
    D3D11_TEXTURE2D_DESC d = {8, 8, 4, 3, f, {1, 0}, u, bind, cpu, 0};
    ID3D11Texture2D *t = NULL;
    ID3D11Device_CreateTexture2D(dev, &d, data ? sd : NULL, &t);
    return (ID3D11Resource *)t;
}

/* a subresource's (row bytes, rows, slices) */
static void sublayout(int dim, int sub, int bpp, int block, UINT *row, UINT *rows, UINT *slices) {
    int l = sub % (dim == 3 ? 3 : 4);
    UINT w = 8 >> l, h = (dim == 3 ? 4 : 8) >> l;
    if (!w) w = 1;
    if (!h) h = 1;
    *row = (w + block - 1) / block * bpp;
    *rows = (h + block - 1) / block;
    *slices = dim == 3 ? (4 >> l ? 4 >> l : 1) : 1;
}

/* FNV-1a of a mapped subresource's texel bytes (not the pitches' padding) */
static void subhash(const char *what, ID3D11Resource *t, int dim, int sub, int bpp, int block) {
    D3D11_MAPPED_SUBRESOURCE m;
    HRESULT hr = ID3D11DeviceContext_Map(ctx, t, sub, D3D11_MAP_READ, 0, &m);
    if (FAILED(hr)) {
        printf("%s sub %d map %s\n", what, sub, code(hr));
        return;
    }
    UINT row, rows, slices;
    sublayout(dim, sub, bpp, block, &row, &rows, &slices);
    unsigned h = 2166136261u;
    for (UINT z = 0; z < slices; z++)
        for (UINT y = 0; y < rows; y++) {
            const unsigned char *p = (const unsigned char *)m.pData + z * m.DepthPitch + y * m.RowPitch;
            for (UINT x = 0; x < row; x++) h = (h ^ p[x]) * 16777619u;
        }
    ID3D11DeviceContext_Unmap(ctx, t, sub);
    printf("%s sub %d %08x\n", what, sub, h);
}

static void subresources(void) {
    for (int dim = 2; dim <= 3; dim++)
        for (int i = 0; i < (int)(sizeof subfmts / sizeof subfmts[0]); i++) {
            int bpp = subfmts[i].bpp, block = subfmts[i].block, n = dim == 3 ? 3 : 12;
            unsigned char *data[12];
            UINT pitch[12], slice[12];
            for (int s = 0; s < n; s++) {
                UINT row, rows, slices;
                sublayout(dim, s, bpp, block, &row, &rows, &slices);
                pitch[s] = row, slice[s] = row * rows;
                data[s] = malloc(row * rows * slices);
                for (UINT k = 0; k < row * rows * slices; k++) data[s][k] = (unsigned char)((k * 131 + s * 29 + 7) ^ (k >> 3) ^ (i * 17));
            }
            char what[96];
            ID3D11Resource *src = subtex(dim, subfmts[i].f, 0, (const unsigned char *const *)data, pitch, slice);
            ID3D11Resource *dst = subtex(dim, subfmts[i].g, 1, NULL, NULL, NULL), *dst2 = subtex(dim, subfmts[i].g, 1, NULL, NULL, NULL);
            if (!src || !dst || !dst2) {
                printf("sub %s %dd failed\n", subfmts[i].n, dim);
            } else {
                /* every subresource through CopyResource */
                ID3D11DeviceContext_CopyResource(ctx, dst, src);
                snprintf(what, sizeof what, "resource %s %dd", subfmts[i].n, dim);
                for (int s = 0; s < n; s++) subhash(what, dst, dim, s, bpp, block);
                /* the last layer's level 0 box (4, 0, 1)-(8, 4, 3) to (0, dy, 0) of the first */
                UINT dy = dim == 3 ? 0 : 4, ssub = dim == 3 ? 0 : 8;
                D3D11_BOX box = {4, 0, 1, 8, 4, 3};
                ID3D11DeviceContext_CopySubresourceRegion(ctx, dst2, 0, 0, dy, 0, src, ssub, &box);
                snprintf(what, sizeof what, "region %s %dd", subfmts[i].n, dim);
                subhash(what, dst2, dim, 0, bpp, block);
                /* UpdateSubresource of the box (4, dy, z)-(8, dy + 4, z + slices) into the copy */
                UINT z = dim == 3 ? 1 : 0, depth = dim == 3 ? 2 : 1, row = 4 / block * bpp, nrows = 4 / block;
                unsigned char up[64 * 4 * 2];
                for (UINT k = 0; k < row * nrows * depth; k++) up[k] = (unsigned char)(k * 53 + i);
                D3D11_BOX ub = {4, dy, z, 8, dy + 4, z + depth};
                ID3D11DeviceContext_UpdateSubresource(ctx, dst, 0, &ub, up, row, row * nrows);
                snprintf(what, sizeof what, "update %s %dd", subfmts[i].n, dim);
                subhash(what, dst, dim, 0, bpp, block);
            }
            if (src) ID3D11Resource_Release(src);
            if (dst) ID3D11Resource_Release(dst);
            if (dst2) ID3D11Resource_Release(dst2);
            for (int s = 0; s < n; s++) free(data[s]);
        }
}

/* ---- input layouts ---- */

static ID3DBlob *vs(const char *src) {
    ID3DBlob *b = NULL, *err = NULL;
    HRESULT hr = D3DCompile(src, strlen(src), "probe", NULL, NULL, "main", "vs_4_0", 0, 0, &b, &err);
    if (err) ID3D10Blob_Release(err);
    return SUCCEEDED(hr) ? b : NULL;
}

static void layouts(void) {
    static const char *shaders[] = {
        "float4 main(float4 p : POSITION) : SV_Position { return p; }",
        "float4 main(float4 p : POSITION, float4 c : COLOR, float2 t : TEXCOORD1) : SV_Position { return p + c + t.xyxy; }",
        "float4 main(float4 p : POSITION, uint v : SV_VertexID, uint i : SV_InstanceID) : SV_Position { return p + v + i; }",
        "float4 main() : SV_Position { return 0; }",
        "float4 main(float4 p : pOsItIoN, float4 q : POSITION1) : SV_Position { return p + q; }",
        "float4 main(float4 p : SV_Position, float4 c : SV_ClipDistance) : SV_Position { return p + c; }",
    };
    /* element lists: name, index, format, slot, offset, class */
    static const struct { const char *n; int count; D3D11_INPUT_ELEMENT_DESC e[4]; } lists[] = {
        {"none", 0, {{0}}},
        {"pos", 1, {{"POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"pos_lower", 1, {{"position", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"pos1", 1, {{"POSITION", 1, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"pos_pos1", 2, {{"POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}, {"POSITION", 1, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 16, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"pos_color_tex1", 3, {{"POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}, {"COLOR", 0, DXGI_FORMAT_R8G8B8A8_UNORM, 0, 16, D3D11_INPUT_PER_VERTEX_DATA, 0}, {"TEXCOORD", 1, DXGI_FORMAT_R32G32_FLOAT, 1, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"pos_color_tex0", 3, {{"POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}, {"COLOR", 0, DXGI_FORMAT_R8G8B8A8_UNORM, 0, 16, D3D11_INPUT_PER_VERTEX_DATA, 0}, {"TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 1, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"pos_inst_color_vert_same_slot", 2, {{"POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 0, D3D11_INPUT_PER_INSTANCE_DATA, 1}, {"COLOR", 0, DXGI_FORMAT_R8G8B8A8_UNORM, 0, 16, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"pos_inst_color_vert_two_slots", 2, {{"POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 0, D3D11_INPUT_PER_INSTANCE_DATA, 1}, {"COLOR", 0, DXGI_FORMAT_R8G8B8A8_UNORM, 1, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"pos_sv_ids", 3, {{"POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}, {"SV_VertexID", 0, DXGI_FORMAT_R32_UINT, 0, 16, D3D11_INPUT_PER_VERTEX_DATA, 0}, {"SV_InstanceID", 0, DXGI_FORMAT_R32_UINT, 0, 20, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"sv_position_clip", 2, {{"SV_Position", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}, {"SV_ClipDistance", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 16, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"pos_vert_step1", 1, {{"POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 1}}},
        {"pos_slot15", 1, {{"POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 15, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"pos_slot32", 1, {{"POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 32, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"pos_unknown_format", 1, {{"POSITION", 0, DXGI_FORMAT_UNKNOWN, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"pos_bc1", 1, {{"POSITION", 0, DXGI_FORMAT_BC1_UNORM, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"pos_twice", 2, {{"POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0}, {"POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 16, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
        {"pos_append", 2, {{"POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, D3D11_APPEND_ALIGNED_ELEMENT, D3D11_INPUT_PER_VERTEX_DATA, 0}, {"COLOR", 0, DXGI_FORMAT_R8G8B8A8_UNORM, 0, D3D11_APPEND_ALIGNED_ELEMENT, D3D11_INPUT_PER_VERTEX_DATA, 0}}},
    };
    for (int s = 0; s < (int)(sizeof shaders / sizeof shaders[0]); s++) {
        ID3DBlob *b = vs(shaders[s]);
        if (!b) {
            printf("layout shader %d compile failed\n", s);
            continue;
        }
        for (int l = 0; l < (int)(sizeof lists / sizeof lists[0]); l++) {
            ID3D11InputLayout *o = (void *)1;
            HRESULT hr = ID3D11Device_CreateInputLayout(dev, lists[l].count ? lists[l].e : lists[0].e, lists[l].count, ID3D10Blob_GetBufferPointer(b), ID3D10Blob_GetBufferSize(b), &o);
            printf("layout shader %d %s %s out %s\n", s, lists[l].n, code(hr), o == NULL ? "null" : o == (void *)1 ? "kept" : "set");
            if (SUCCEEDED(hr) && o && o != (void *)1) ID3D11InputLayout_Release(o);
        }
        ID3D11InputLayout *o = (void *)1;
        HRESULT hr = ID3D11Device_CreateInputLayout(dev, NULL, 0, ID3D10Blob_GetBufferPointer(b), ID3D10Blob_GetBufferSize(b), &o);
        printf("layout shader %d null_descs %s out %s\n", s, code(hr), o == NULL ? "null" : o == (void *)1 ? "kept" : "set");
        if (SUCCEEDED(hr) && o && o != (void *)1) ID3D11InputLayout_Release(o);
        ID3D10Blob_Release(b);
    }
}

/* ---- the newer state descriptions ---- */

static void dump_words(const char *what, const void *p, int n) {
    printf("%s", what);
    for (int i = 0; i < n; i++) printf(" %x", ((const UINT *)p)[i]);
    printf("\n");
}

static void states(void) {
    ID3D11Device1 *d1 = NULL;
    ID3D11Device3 *d3 = NULL;
    printf("device1 %s\n", code(ID3D11Device_QueryInterface(dev, &IID_ID3D11Device1, (void **)&d1)));
    printf("device3 %s\n", code(ID3D11Device_QueryInterface(dev, &IID_ID3D11Device3, (void **)&d3)));
    D3D11_RASTERIZER_DESC r = {D3D11_FILL_SOLID, D3D11_CULL_BACK, FALSE, 0, 0, 0, TRUE, FALSE, FALSE, FALSE};
    ID3D11RasterizerState *rs = NULL;
    ID3D11Device_CreateRasterizerState(dev, &r, &rs);
    if (rs) {
        void *x = NULL;
        HRESULT h1 = ID3D11RasterizerState_QueryInterface(rs, &IID_ID3D11RasterizerState1, &x);
        if (x) IUnknown_Release((IUnknown *)x);
        x = NULL;
        HRESULT h2 = ID3D11RasterizerState_QueryInterface(rs, &IID_ID3D11RasterizerState2, &x);
        printf("raster qi1 %s qi2 %s\n", code(h1), code(h2));
        if (x) {
            D3D11_RASTERIZER_DESC2 g;
            memset(&g, 0xcc, sizeof g);
            ID3D11RasterizerState2_GetDesc2((ID3D11RasterizerState2 *)x, &g);
            dump_words("raster desc2 of plain", &g, sizeof g / 4);
            IUnknown_Release((IUnknown *)x);
        }
    }
    if (d1) {
        static const UINT forced[] = {0, 1, 4, 8, 16, 3, 32};
        for (int i = 0; i < (int)(sizeof forced / sizeof forced[0]); i++) {
            D3D11_RASTERIZER_DESC1 r1;
            memcpy(&r1, &r, sizeof r);
            r1.ForcedSampleCount = forced[i];
            for (int ms = 0; ms < 2; ms++) {
                r1.MultisampleEnable = ms;
                ID3D11RasterizerState1 *s = NULL;
                HRESULT hr = ID3D11Device1_CreateRasterizerState1(d1, &r1, &s);
                printf("raster1 forced %u ms %d %s same %d", forced[i], ms, code(hr), s && rs && (void *)s == (void *)rs);
                if (s) {
                    D3D11_RASTERIZER_DESC1 g;
                    memset(&g, 0xcc, sizeof g);
                    ID3D11RasterizerState1_GetDesc1(s, &g);
                    printf(" desc1 forced %u ms %u", g.ForcedSampleCount, g.MultisampleEnable);
                    D3D11_RASTERIZER_DESC g0;
                    memset(&g0, 0xcc, sizeof g0 + 0);
                    ID3D11RasterizerState1_GetDesc(s, &g0);
                    printf(" desc ms %u", g0.MultisampleEnable);
                    ID3D11RasterizerState1_Release(s);
                }
                printf("\n");
            }
        }
        /* blend: logic ops (D3D11_FEATURE_D3D11_OPTIONS says if the device has them) */
        D3D11_FEATURE_DATA_D3D11_OPTIONS o;
        memset(&o, 0, sizeof o);
        ID3D11Device_CheckFeatureSupport(dev, D3D11_FEATURE_D3D11_OPTIONS, &o, sizeof o);
        printf("options logic_op %d\n", o.OutputMergerLogicOp);
        for (int lo = 0; lo < 3; lo++)
            for (int ind = 0; ind < 2; ind++) {
                D3D11_BLEND_DESC1 b;
                memset(&b, 0, sizeof b);
                b.IndependentBlendEnable = ind;
                for (int t = 0; t < 8; t++) {
                    b.RenderTarget[t].SrcBlend = b.RenderTarget[t].SrcBlendAlpha = D3D11_BLEND_ONE;
                    b.RenderTarget[t].DestBlend = b.RenderTarget[t].DestBlendAlpha = D3D11_BLEND_ZERO;
                    b.RenderTarget[t].BlendOp = b.RenderTarget[t].BlendOpAlpha = D3D11_BLEND_OP_ADD;
                    b.RenderTarget[t].RenderTargetWriteMask = 0xf;
                }
                b.RenderTarget[0].LogicOpEnable = lo > 0;
                b.RenderTarget[0].LogicOp = lo == 2 ? D3D11_LOGIC_OP_XOR : D3D11_LOGIC_OP_CLEAR;
                b.RenderTarget[1].BlendEnable = lo == 2;
                ID3D11BlendState1 *s = NULL;
                HRESULT hr = ID3D11Device1_CreateBlendState1(d1, &b, &s);
                printf("blend1 logic %d independent %d %s", lo, ind, code(hr));
                if (s) {
                    D3D11_BLEND_DESC1 g;
                    memset(&g, 0xcc, sizeof g);
                    ID3D11BlendState1_GetDesc1(s, &g);
                    printf(" rt0 %u %u %u rt1 %u %u %u", g.RenderTarget[0].BlendEnable, g.RenderTarget[0].LogicOpEnable, g.RenderTarget[0].LogicOp, g.RenderTarget[1].BlendEnable,
                           g.RenderTarget[1].LogicOpEnable, g.RenderTarget[1].LogicOp);
                    ID3D11BlendState1_Release(s);
                }
                printf("\n");
            }
        ID3D11Device1_Release(d1);
    }
    if (d3) {
        static const UINT cons[] = {0, 1, 2};
        for (int i = 0; i < 3; i++) {
            D3D11_RASTERIZER_DESC2 r2;
            memset(&r2, 0, sizeof r2);
            memcpy(&r2, &r, sizeof r);
            r2.ConservativeRaster = cons[i];
            ID3D11RasterizerState2 *s = NULL;
            HRESULT hr = ID3D11Device3_CreateRasterizerState2(d3, &r2, &s);
            printf("raster2 conservative %u %s", cons[i], code(hr));
            if (s) {
                D3D11_RASTERIZER_DESC2 g;
                memset(&g, 0xcc, sizeof g);
                ID3D11RasterizerState2_GetDesc2(s, &g);
                printf(" desc2 %u %u", g.ForcedSampleCount, g.ConservativeRaster);
                ID3D11RasterizerState2_Release(s);
            }
            printf("\n");
        }
        ID3D11Device3_Release(d3);
    }
    if (rs) ID3D11RasterizerState_Release(rs);
}

int main(void) {
    D3D_FEATURE_LEVEL fl = D3D_FEATURE_LEVEL_11_0;
    HRESULT hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, &fl, 1, D3D11_SDK_VERSION, &dev, NULL, &ctx);
    if (FAILED(hr)) {
        printf("D3D11CreateDevice failed %#lx\n", (unsigned long)hr);
        return 1;
    }
    copies();
    subresources();
    layouts();
    states();
    ID3D11DeviceContext_Release(ctx);
    printf("release device %lu\n", ID3D11Device_Release(dev));
    return 0;
}
