/* Direct3D 11 (feature level 11_0; WARP on the Windows runner): clears and their rounding to
 * UNORM, fill rules and interpolation, lines and points, sampling (filters, address modes,
 * border, LOD), texture formats incl. BC and sRGB, blend states, depth/stencil, rasterizer
 * state, copies and updates, mipmap generation, and shader arithmetic: float and integer
 * operations of HLSL (compiled with the system's D3DCompile) on chosen and random inputs,
 * read back from float/uint targets bit for bit (approximate operations within the limits the
 * Direct3D 11 functional spec gives them).
 * libs: -ld3d11 -ld3dcompiler_47
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <d3d11.h>
#include <d3dcompiler.h>
#include <math.h>

#define W 64
#define H 64

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;
static ID3D11Texture2D *rt, *stage, *zbuf;
static ID3D11RenderTargetView *rtv;
static ID3D11DepthStencilView *dsv;

static const char *SRC, *MATH;

static ID3DBlob *compile(const char *src, const char *entry, const char *target, const D3D_SHADER_MACRO *defs) {
    ID3DBlob *b = NULL, *err = NULL;
    HRESULT hr = D3DCompile(src, strlen(src), "probe", defs, NULL, entry, target, 0, 0, &b, &err);
    if (err) ID3D10Blob_Release(err);
    if (FAILED(hr)) {
        printf("D3DCompile %s %s %s failed\n", entry, target, defs && defs[0].Name ? defs[0].Definition : "");
        return NULL;
    }
    return b;
}

static ID3D11VertexShader *vs_of(const char *src, const char *entry, ID3DBlob **keep) {
    ID3DBlob *b = compile(src, entry, src == SRC ? "vs_4_0" : "vs_5_0", NULL);
    ID3D11VertexShader *s = NULL;
    if (!b) return NULL;
    ID3D11Device_CreateVertexShader(dev, ID3D10Blob_GetBufferPointer(b), ID3D10Blob_GetBufferSize(b), NULL, &s);
    if (keep) *keep = b; else ID3D10Blob_Release(b);
    return s;
}

static ID3D11PixelShader *ps_of(const char *src, const char *entry, const D3D_SHADER_MACRO *defs) {
    ID3DBlob *b = compile(src, entry, src == SRC ? "ps_4_0" : "ps_5_0", defs);
    ID3D11PixelShader *s = NULL;
    if (!b) return NULL;
    ID3D11Device_CreatePixelShader(dev, ID3D10Blob_GetBufferPointer(b), ID3D10Blob_GetBufferSize(b), NULL, &s);
    ID3D10Blob_Release(b);
    return s;
}

/* ---- pictures ---- */

static const char *SRC =
    "Texture2D t0 : register(t0); SamplerState s0 : register(s0);\n"
    "cbuffer c : register(b0) { float4 k; float lod; float3 pad; }\n"
    "struct V { float4 p : SV_Position; float4 c : COLOR; float2 uv : TEXCOORD; };\n"
    "V vs(float2 p : POSITION, float4 c : COLOR, float2 uv : TEXCOORD) {\n"
    "  V o; o.p = float4(p.x / 32 - 1, 1 - p.y / 32, 0.5, 1); o.c = c; o.uv = uv; return o; }\n"
    "V vsz(float3 p : POSITION, float4 c : COLOR, float2 uv : TEXCOORD) {\n"
    "  V o; o.p = float4(p.x / 32 - 1, 1 - p.y / 32, p.z, 1); o.c = c; o.uv = uv; return o; }\n"
    "float4 color(V i) : SV_Target { return i.c; }\n"
    "float4 tex(V i) : SV_Target { return t0.Sample(s0, i.uv) * i.c; }\n"
    "float4 texlod(V i) : SV_Target { return t0.SampleLevel(s0, i.uv, lod); }\n"
    "float4 texbias(V i) : SV_Target { return t0.SampleBias(s0, i.uv, lod); }\n"
    "float4 load(V i) : SV_Target { return t0.Load(int3(i.uv * 8, 0)); }\n"
    "float4 konst(V i) : SV_Target { return k; }\n"
    "struct V2 { float4 p : SV_Position; nointerpolation float4 c : COLOR; float2 uv : TEXCOORD; };\n"
    "float4 flat(V2 i) : SV_Target { return i.c; }\n"
    "struct V3 { float4 p : SV_Position; noperspective float4 c : COLOR; float2 uv : TEXCOORD; };\n"
    "float4 noper(V3 i) : SV_Target { return i.c; }\n"
    "float4 discard_(V i) : SV_Target { if (i.c.a < 0.5) discard; return i.c; }\n"
    "float4 pos(V i) : SV_Target { return float4(frac(i.p.xy / 8), i.p.z, 1); }\n"
    "struct V4 { float4 p : SV_Position; float4 c : COLOR; float2 uv : TEXCOORD; };\n"
    "V4 vsw(float4 p : POSITION, float4 c : COLOR, float2 uv : TEXCOORD) {\n"
    "  V4 o; o.p = float4((p.x / 32 - 1) * p.w, (1 - p.y / 32) * p.w, p.z * p.w, p.w); o.c = c; o.uv = uv; return o; }\n";

static ID3D11VertexShader *vs, *vsz, *vsw;
static ID3D11InputLayout *layout, *layoutz, *layoutw;
static ID3D11PixelShader *ps_color, *ps_tex, *ps_texlod, *ps_texbias, *ps_load, *ps_konst, *ps_flat, *ps_noper, *ps_discard, *ps_pos;
static ID3D11Buffer *cb;

struct v2 { float x, y; float c[4]; float u, v; };
struct v3 { float x, y, z; float c[4]; float u, v; };
struct v4 { float x, y, z, w; float c[4]; float u, v; };

static void rgba(float *c, DWORD argb) {
    c[0] = (argb >> 16 & 255) / 255.0f; c[1] = (argb >> 8 & 255) / 255.0f; c[2] = (argb & 255) / 255.0f; c[3] = (argb >> 24) / 255.0f;
}

static void draw_buf(const void *v, int stride, int n, D3D11_PRIMITIVE_TOPOLOGY topo) {
    D3D11_BUFFER_DESC bd = { stride * n, D3D11_USAGE_IMMUTABLE, D3D11_BIND_VERTEX_BUFFER, 0, 0, 0 };
    D3D11_SUBRESOURCE_DATA sd = { v, 0, 0 };
    ID3D11Buffer *b = NULL;
    if (FAILED(ID3D11Device_CreateBuffer(dev, &bd, &sd, &b))) { puts("CreateBuffer vertices failed"); return; }
    UINT st = stride, off = 0;
    ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &b, &st, &off);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, topo);
    ID3D11DeviceContext_Draw(ctx, n, 0);
    ID3D11Buffer_Release(b);
}

static void use(ID3D11VertexShader *v, ID3D11InputLayout *l, ID3D11PixelShader *p) {
    ID3D11DeviceContext_IASetInputLayout(ctx, l);
    ID3D11DeviceContext_VSSetShader(ctx, v, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, p, NULL, 0);
}

static void quad(float x0, float y0, float x1, float y1, DWORD c, float u0, float v0, float u1, float v1) {
    struct v2 q[4] = { { x0, y0, {0}, u0, v0 }, { x1, y0, {0}, u1, v0 }, { x0, y1, {0}, u0, v1 }, { x1, y1, {0}, u1, v1 } };
    for (int i = 0; i < 4; i++) rgba(q[i].c, c);
    draw_buf(q, sizeof q[0], 4, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
}

static void quadz(float x0, float y0, float x1, float y1, float z, DWORD c) {
    struct v3 q[4] = { { x0, y0, z }, { x1, y0, z }, { x0, y1, z }, { x1, y1, z } };
    for (int i = 0; i < 4; i++) rgba(q[i].c, c);
    draw_buf(q, sizeof q[0], 4, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
}

static void konst(float r, float g, float b, float a, float lod) {
    float d[8] = { r, g, b, a, lod };
    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)cb, 0, NULL, d, 0, 0);
}

static ID3D11SamplerState *sampler(D3D11_FILTER f, D3D11_TEXTURE_ADDRESS_MODE a, const float *border) {
    D3D11_SAMPLER_DESC d = { f, a, a, a, 0, 1, D3D11_COMPARISON_NEVER, { 0, 0, 0, 0 }, 0, D3D11_FLOAT32_MAX };
    if (border) memcpy(d.BorderColor, border, 16);
    ID3D11SamplerState *s = NULL;
    if (FAILED(ID3D11Device_CreateSamplerState(dev, &d, &s))) printf("CreateSamplerState %d %d failed\n", f, a);
    ID3D11DeviceContext_PSSetSamplers(ctx, 0, 1, &s);
    return s;
}

static ID3D11ShaderResourceView *srv_of(ID3D11Texture2D *t) {
    ID3D11ShaderResourceView *v = NULL;
    if (FAILED(ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)t, NULL, &v))) return NULL;
    return v;
}

static ID3D11Texture2D *texture(DXGI_FORMAT f, int w, int h, int mips, const void *data, int pitch) {
    D3D11_TEXTURE2D_DESC d = { w, h, mips, 1, f, { 1, 0 }, D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, 0, 0 };
    D3D11_SUBRESOURCE_DATA sd = { data, pitch, 0 };
    ID3D11Texture2D *t = NULL;
    if (FAILED(ID3D11Device_CreateTexture2D(dev, &d, data ? &sd : NULL, &t))) return NULL;
    return t;
}

static void bind_srv(ID3D11ShaderResourceView *v) { ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &v); }

/* the 64x64 target as a picture (and its alpha as gray, with alpha set) */
static void shot(const char *name, int tol, int alpha) {
    D3D11_MAPPED_SUBRESOURCE m;
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)stage, (ID3D11Resource *)rt);
    if (FAILED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)stage, 0, D3D11_MAP_READ, 0, &m))) { printf("%s: Map failed\n", name); return; }
    static unsigned char px[W * H * 4], pa[W * H * 4];
    for (int y = 0; y < H; y++)
        for (int x = 0; x < W; x++) {
            const unsigned char *s = (const unsigned char *)m.pData + y * m.RowPitch + x * 4;
            unsigned char *d = px + (y * W + x) * 4, *a = pa + (y * W + x) * 4;
            d[0] = s[2]; d[1] = s[1]; d[2] = s[0]; d[3] = 0;
            a[0] = a[1] = a[2] = s[3];
        }
    ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)stage, 0);
    probe_image(name, px, W, H, W * 4, tol);
    if (alpha) {
        char n[64];
        snprintf(n, sizeof n, "%s_alpha", name);
        probe_image(n, pa, W, H, W * 4, tol);
    }
}

/* one pixel of the target, RGBA bytes */
static DWORD pixel(int x, int y) {
    D3D11_MAPPED_SUBRESOURCE m;
    D3D11_BOX b = { x, y, 0, x + 1, y + 1, 1 };
    ID3D11DeviceContext_CopySubresourceRegion(ctx, (ID3D11Resource *)stage, 0, 0, 0, 0, (ID3D11Resource *)rt, 0, &b);
    if (FAILED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)stage, 0, D3D11_MAP_READ, 0, &m))) return 0xdeadbeef;
    DWORD v = *(DWORD *)m.pData;
    ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)stage, 0);
    return v;
}

static ID3D11BlendState *blend_state;
static ID3D11DepthStencilState *ds_state;
static ID3D11RasterizerState *rs_state;

static void set_blend(BOOL on, D3D11_BLEND s, D3D11_BLEND d, D3D11_BLEND_OP op, D3D11_BLEND sa, D3D11_BLEND da, D3D11_BLEND_OP opa, UINT8 mask) {
    D3D11_BLEND_DESC b = { 0 };
    b.RenderTarget[0] = (D3D11_RENDER_TARGET_BLEND_DESC){ on, s, d, op, sa, da, opa, mask };
    if (blend_state) ID3D11BlendState_Release(blend_state);
    blend_state = NULL;
    if (FAILED(ID3D11Device_CreateBlendState(dev, &b, &blend_state))) printf("CreateBlendState %d %d %d failed\n", s, d, op);
    float f[4] = { 0.25f, 0.5f, 0.75f, 0.5f };
    ID3D11DeviceContext_OMSetBlendState(ctx, blend_state, f, 0xffffffff);
}

static void set_depth(BOOL z, D3D11_COMPARISON_FUNC f, BOOL write, BOOL stencil, D3D11_COMPARISON_FUNC sf, D3D11_STENCIL_OP pass, UINT ref) {
    D3D11_DEPTH_STENCIL_DESC d = { z, write ? D3D11_DEPTH_WRITE_MASK_ALL : D3D11_DEPTH_WRITE_MASK_ZERO, f, stencil, 0xff, 0xff,
                                   { D3D11_STENCIL_OP_KEEP, D3D11_STENCIL_OP_KEEP, pass, sf }, { D3D11_STENCIL_OP_KEEP, D3D11_STENCIL_OP_KEEP, pass, sf } };
    if (ds_state) ID3D11DepthStencilState_Release(ds_state);
    ds_state = NULL;
    if (FAILED(ID3D11Device_CreateDepthStencilState(dev, &d, &ds_state))) puts("CreateDepthStencilState failed");
    ID3D11DeviceContext_OMSetDepthStencilState(ctx, ds_state, ref);
}

static void set_raster(D3D11_FILL_MODE fill, D3D11_CULL_MODE cull, BOOL ccw, BOOL scissor, INT bias, float slope, BOOL clip) {
    D3D11_RASTERIZER_DESC r = { fill, cull, ccw, bias, 0, slope, clip, scissor, FALSE, FALSE };
    if (rs_state) ID3D11RasterizerState_Release(rs_state);
    rs_state = NULL;
    if (FAILED(ID3D11Device_CreateRasterizerState(dev, &r, &rs_state))) puts("CreateRasterizerState failed");
    ID3D11DeviceContext_RSSetState(ctx, rs_state);
}

static void viewport(float x, float y, float w, float h, float zn, float zf) {
    D3D11_VIEWPORT v = { x, y, w, h, zn, zf };
    ID3D11DeviceContext_RSSetViewports(ctx, 1, &v);
}

static void reset(void) {
    set_blend(FALSE, D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD, D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD, 0xf);
    set_depth(FALSE, D3D11_COMPARISON_LESS, TRUE, FALSE, D3D11_COMPARISON_ALWAYS, D3D11_STENCIL_OP_KEEP, 0);
    set_raster(D3D11_FILL_SOLID, D3D11_CULL_NONE, FALSE, FALSE, 0, 0, TRUE);
    viewport(0, 0, W, H, 0, 1);
    ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv, dsv);
    use(vs, layout, ps_color);
}

static void begin(float r, float g, float b, float a) {
    float c[4] = { r, g, b, a };
    ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv, c);
    ID3D11DeviceContext_ClearDepthStencilView(ctx, dsv, D3D11_CLEAR_DEPTH | D3D11_CLEAR_STENCIL, 1.0f, 0);
}

static DWORD checker[16];

static void pictures(void) {
    reset();
    /* clears: float -> UNORM rounding of each channel (read back as bytes) */
    {
        static const float v[] = { 0.0f, 1.0f, 0.5f, 0.25f, 1.0f / 3, 2.0f / 3, 0.501960814f, 0.498039216f, 0.00196f, 0.001953125f,
                                   0.00196078f, 0.998f, 0.9980392f, 0.9990196f, -0.5f, 1.5f, 127.5f / 255, 128.5f / 255, 0.2f, 0.7f };
        int n = sizeof v / sizeof v[0];
        for (int i = 0; i < n; i += 4) {
            float c[4] = { v[i], v[(i + 1) % n], v[(i + 2) % n], v[(i + 3) % n] };
            ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv, c);
            printf("clear %.9g %.9g %.9g %.9g -> %08lx\n", c[0], c[1], c[2], c[3], pixel(5, 5));
        }
        for (int i = 0; i < 256; i += 17) {
            float c[4] = { i / 255.0f, (i + 0.49f) / 255.0f, (i + 0.51f) / 255.0f, (i - 0.5f) / 255.0f };
            ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv, c);
            printf("clear byte %d -> %08lx\n", i, pixel(0, 0));
        }
    }
    /* rasterization: a Gouraud triangle, edges on pixel centres and halves (top-left rule), a
     * fan sharing edges (additive: no pixel twice), flat and noperspective attributes, slivers */
    {
        begin(0, 0, 0, 1);
        struct v2 t[3] = { { 2, 2, {1, 0, 0, 1} }, { 40, 10, {0, 1, 0, 1} }, { 10, 40, {0, 0, 1, 1} } };
        draw_buf(t, sizeof t[0], 3, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        use(vs, layout, ps_flat);
        struct v2 f[3] = { { 44, 2, {1, 1, 0, 1} }, { 62, 2, {0, 1, 1, 1} }, { 53, 20, {1, 0, 1, 1} } };
        draw_buf(f, sizeof f[0], 3, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        use(vs, layout, ps_color);
        set_blend(TRUE, D3D11_BLEND_ONE, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, D3D11_BLEND_ONE, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, 0xf);
        struct v2 fan[8];
        fan[0] = (struct v2){ 30, 46, {0.25f, 0.25f, 0.25f, 1} };
        for (int i = 0; i < 7; i++) {
            float a = i * 6.2831853f / 6;
            fan[i + 1] = (struct v2){ 30 + 15.25f * cosf(a), 46 + 15.25f * sinf(a), {0.25f, 0.25f, 0.25f, 1} };
        }
        struct v2 tl[18];
        for (int i = 0; i < 6; i++) { tl[i * 3] = fan[0]; tl[i * 3 + 1] = fan[i + 1]; tl[i * 3 + 2] = fan[i + 2]; }
        draw_buf(tl, sizeof tl[0], 18, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        reset();
        quad(50.5f, 30, 51.5f, 62, 0xff808080, 0, 0, 0, 0);
        quad(53, 30, 55, 62, 0xff808080, 0, 0, 0, 0);
        quad(56.75f, 30.25f, 58.25f, 61.75f, 0xff808080, 0, 0, 0, 0);
        quad(60, 30.5f, 60.4f, 40, 0xffffffff, 0, 0, 0, 0); /* covers no centre */
        shot("raster", 1, 0);
    }
    /* perspective: a quad with w varying (perspective-correct vs noperspective colours) */
    {
        begin(0, 0, 0, 1);
        use(vsw, layoutw, ps_color);
        struct v4 q[4] = { { 2, 2, 0.5f, 1, {1, 0, 0, 1} }, { 30, 2, 0.5f, 4, {0, 1, 0, 1} }, { 2, 30, 0.5f, 1, {0, 0, 1, 1} }, { 30, 30, 0.5f, 4, {1, 1, 1, 1} } };
        draw_buf(q, sizeof q[0], 4, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        use(vsw, layoutw, ps_noper);
        for (int i = 0; i < 4; i++) q[i].x += 32;
        draw_buf(q, sizeof q[0], 4, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        /* SV_Position as the pixel shader sees it */
        use(vsz, layoutz, ps_pos);
        struct v3 p[4] = { { 2, 34, 0.25f }, { 62, 34, 0.75f }, { 2, 62, 0.25f }, { 62, 62, 0.75f } };
        draw_buf(p, sizeof p[0], 4, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        shot("perspective", 1, 0);
        reset();
    }
    /* lines and points */
    {
        begin(0, 0, 0, 1);
        struct v2 l[] = {
            { 2.5f, 2.5f, {1, 1, 1, 1} }, { 30.5f, 2.5f, {1, 1, 1, 1} }, { 2.5f, 5.5f, {1, 0, 0, 1} }, { 30.5f, 12.5f, {0, 0, 1, 1} },
            { 2.5f, 15.5f, {0, 1, 0, 1} }, { 12.5f, 40.5f, {0, 1, 0, 1} }, { 20, 20, {1, 1, 0, 1} }, { 40, 30, {1, 1, 0, 1} },
            { 34, 2, {0, 1, 1, 1} }, { 34, 30, {0, 1, 1, 1} }, { 62.5f, 2.5f, {1, 0, 1, 1} }, { 36.5f, 28.5f, {1, 0, 1, 1} },
        };
        draw_buf(l, sizeof l[0], sizeof l / sizeof l[0], D3D11_PRIMITIVE_TOPOLOGY_LINELIST);
        struct v2 s[6] = { { 2, 50, {1, 1, 1, 1} }, { 10, 44, {1, 0, 0, 1} }, { 18, 58, {0, 1, 0, 1} }, { 26, 46, {0, 0, 1, 1} }, { 34, 60, {1, 1, 0, 1} }, { 42, 50, {1, 1, 1, 1} } };
        draw_buf(s, sizeof s[0], 6, D3D11_PRIMITIVE_TOPOLOGY_LINESTRIP);
        struct v2 p[6] = { { 50.5f, 44.5f, {1, 1, 1, 1} }, { 52, 44, {1, 1, 1, 1} }, { 54.25f, 44.75f, {1, 1, 1, 1} }, { 56.5f, 46, {1, 0, 0, 1} }, { 58.9f, 46.1f, {0, 1, 0, 1} }, { 61.5f, 61.5f, {0, 0, 1, 1} } };
        draw_buf(p, sizeof p[0], 6, D3D11_PRIMITIVE_TOPOLOGY_POINTLIST);
        shot("lines", 0, 0);
    }
    /* rasterizer state: culling and winding, wireframe, scissor, viewport offsets and depth
     * range, clipping to z */
    {
        begin(0, 0, 0.125f, 1);
        struct v2 cw[3] = { { 2, 2, {1, 0, 0, 1} }, { 14, 2, {1, 0, 0, 1} }, { 2, 14, {1, 0, 0, 1} } };
        struct v2 ccw[3] = { { 2, 2, {0, 1, 0, 1} }, { 2, 14, {0, 1, 0, 1} }, { 14, 2, {0, 1, 0, 1} } };
        static const D3D11_CULL_MODE cm[3] = { D3D11_CULL_NONE, D3D11_CULL_BACK, D3D11_CULL_FRONT };
        for (int c = 0; c < 3; c++)
            for (int front = 0; front < 2; front++) {
                set_raster(D3D11_FILL_SOLID, cm[c], front, FALSE, 0, 0, TRUE);
                float dx = c * 20 + front * 8, dy = front * 2;
                struct v2 a[3], b[3];
                for (int i = 0; i < 3; i++) { a[i] = cw[i]; a[i].x += dx; a[i].y += dy; b[i] = ccw[i]; b[i].x += dx; b[i].y += dy + 14; }
                draw_buf(a, sizeof a[0], 3, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                draw_buf(b, sizeof b[0], 3, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            }
        set_raster(D3D11_FILL_WIREFRAME, D3D11_CULL_NONE, FALSE, FALSE, 0, 0, TRUE);
        struct v2 w[3] = { { 4, 36, {1, 1, 1, 1} }, { 28, 40, {1, 1, 1, 1} }, { 10, 60, {1, 1, 1, 1} } };
        draw_buf(w, sizeof w[0], 3, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        set_raster(D3D11_FILL_SOLID, D3D11_CULL_NONE, FALSE, TRUE, 0, 0, TRUE);
        D3D11_RECT sc = { 40, 36, 52, 50 };
        ID3D11DeviceContext_RSSetScissorRects(ctx, 1, &sc);
        quad(34, 32, 62, 62, 0xffffff00, 0, 0, 0, 0);
        set_raster(D3D11_FILL_SOLID, D3D11_CULL_NONE, FALSE, FALSE, 0, 0, TRUE);
        /* a viewport at a fractional offset, smaller: the quad maps to its box */
        viewport(36.5f, 52.25f, 20, 10, 0, 1);
        quad(0, 0, 64, 64, 0xff00ffff, 0, 0, 0, 0);
        viewport(0, 0, W, H, 0, 1);
        /* z outside 0..1 clipped (or not, with DepthClipEnable off) */
        use(vsz, layoutz, ps_color);
        set_depth(TRUE, D3D11_COMPARISON_ALWAYS, TRUE, FALSE, D3D11_COMPARISON_ALWAYS, D3D11_STENCIL_OP_KEEP, 0);
        quadz(58, 2, 62, 10, 1.5f, 0xffff0000);
        set_raster(D3D11_FILL_SOLID, D3D11_CULL_NONE, FALSE, FALSE, 0, 0, FALSE);
        quadz(58, 12, 62, 20, 1.5f, 0xff00ff00);
        quadz(58, 22, 62, 30, -0.5f, 0xff0000ff);
        shot("rasterizer", 1, 0);
        reset();
    }
    /* sampling: 4x4 texture, point and linear, enlarged and shrunk, modulated */
    for (int i = 0; i < 16; i++) checker[i] = 0xff000000 | (i & 3) * 0x55 << 16 | (i >> 2) * 0x55 << 8 | ((i ^ (i >> 2)) & 1) * 0xff;
    {
        DWORD abgr[16];
        for (int i = 0; i < 16; i++) abgr[i] = (checker[i] & 0xff00ff00) | (checker[i] >> 16 & 255) | (checker[i] & 255) << 16;
        ID3D11Texture2D *t = texture(DXGI_FORMAT_R8G8B8A8_UNORM, 4, 4, 1, abgr, 16);
        ID3D11ShaderResourceView *v = srv_of(t);
        begin(0.0625f, 0.0625f, 0.0625f, 1);
        use(vs, layout, ps_tex);
        bind_srv(v);
        ID3D11SamplerState *pt = sampler(D3D11_FILTER_MIN_MAG_MIP_POINT, D3D11_TEXTURE_ADDRESS_WRAP, NULL);
        quad(2, 2, 6, 6, 0xffffffff, 0, 0, 1, 1);
        quad(8, 2, 32, 26, 0xffffffff, 0, 0, 1, 1);
        ID3D11SamplerState *li = sampler(D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_TEXTURE_ADDRESS_WRAP, NULL);
        quad(36, 2, 60, 26, 0xffffffff, 0, 0, 1, 1);
        ID3D11SamplerState *lc = sampler(D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_TEXTURE_ADDRESS_CLAMP, NULL);
        quad(8, 32, 32, 56, 0xffffffff, 0, 0, 1, 1);
        quad(36, 32, 42, 38, 0xff80ff40, 0, 0, 1, 1);
        quad(44, 32, 60, 48, 0xff80ff40, 0.125f, 0.125f, 0.875f, 0.875f);
        ID3D11SamplerState *mx = sampler(D3D11_FILTER_MIN_POINT_MAG_LINEAR_MIP_POINT, D3D11_TEXTURE_ADDRESS_CLAMP, NULL);
        quad(2, 50, 14, 62, 0xffffffff, 0, 0, 1, 1);
        ID3D11SamplerState *an = sampler(D3D11_FILTER_ANISOTROPIC, D3D11_TEXTURE_ADDRESS_CLAMP, NULL);
        quad(44, 50, 60, 62, 0xffffffff, 0, 0, 1, 1);
        /* Load: texel fetch by integer coordinates */
        use(vs, layout, ps_load);
        quad(2, 28, 6, 32, 0xffffffff, 0, 0, 0.5f, 0.5f);
        shot("sampling", 3, 0);
        /* address modes: texcoords -1..2 */
        begin(0, 0, 0, 1);
        use(vs, layout, ps_tex);
        static const D3D11_TEXTURE_ADDRESS_MODE modes[5] = { D3D11_TEXTURE_ADDRESS_WRAP, D3D11_TEXTURE_ADDRESS_MIRROR, D3D11_TEXTURE_ADDRESS_CLAMP,
                                                             D3D11_TEXTURE_ADDRESS_BORDER, D3D11_TEXTURE_ADDRESS_MIRROR_ONCE };
        static const float border[4] = { 0.25f, 0.75f, 0.875f, 1 };
        for (int m = 0; m < 6; m++) {
            ID3D11SamplerState *s = sampler(m == 5 ? D3D11_FILTER_MIN_MAG_MIP_LINEAR : D3D11_FILTER_MIN_MAG_MIP_POINT, modes[m == 5 ? 3 : m], border);
            float x = (m % 3) * 21, y = (m / 3) * 32;
            quad(x + 1, y + 1, x + 21, y + 31, 0xffffffff, -1, -1, 2, 2);
            if (s) ID3D11SamplerState_Release(s);
        }
        shot("address", 0, 0);
        for (ID3D11SamplerState **s = (ID3D11SamplerState *[]){ pt, li, lc, mx, an, NULL }; *s; s++) ID3D11SamplerState_Release(*s);
        ID3D11ShaderResourceView_Release(v);
        ID3D11Texture2D_Release(t);
        reset();
    }
    /* mipmaps: 16x16 with a colour per level (UpdateSubresource each), SampleLevel and
     * SampleBias, trilinear minification; GenerateMips' box filter */
    {
        D3D11_TEXTURE2D_DESC d = { 16, 16, 5, 1, DXGI_FORMAT_R8G8B8A8_UNORM, { 1, 0 }, D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, 0, 0 };
        ID3D11Texture2D *t = NULL;
        ID3D11Device_CreateTexture2D(dev, &d, NULL, &t);
        static const DWORD lc[5] = { 0xff0000ff, 0xff00ff00, 0xffff0000, 0xff00ffff, 0xffffffff };
        for (int l = 0; l < 5; l++) {
            DWORD px[256];
            int s = 16 >> l;
            for (int i = 0; i < s * s; i++) px[i] = ((i % s + i / s) & 1) ? lc[l] : (lc[l] & 0xff7f7f7f);
            ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)t, l, NULL, px, s * 4, 0);
        }
        ID3D11ShaderResourceView *v = srv_of(t);
        begin(0, 0, 0, 1);
        bind_srv(v);
        ID3D11SamplerState *s = sampler(D3D11_FILTER_MIN_MAG_MIP_POINT, D3D11_TEXTURE_ADDRESS_WRAP, NULL);
        use(vs, layout, ps_texlod);
        for (int l = 0; l < 6; l++) {
            konst(0, 0, 0, 0, l * 0.75f);
            quad(l * 10 + 1, 1, l * 10 + 9, 9, 0xffffffff, 0, 0, 1, 1);
        }
        use(vs, layout, ps_tex);
        ID3D11SamplerState *tri = sampler(D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_TEXTURE_ADDRESS_WRAP, NULL);
        quad(1, 12, 9, 20, 0xffffffff, 0, 0, 1, 1);    /* 16 -> 8: level 1 */
        quad(12, 12, 18, 18, 0xffffffff, 0, 0, 1, 1);  /* 16 -> 6: between 1 and 2 */
        quad(20, 12, 23, 15, 0xffffffff, 0, 0, 1, 1);
        struct v2 slant[4] = { { 26, 12, {1, 1, 1, 1}, 0, 0 }, { 62, 12, {1, 1, 1, 1}, 4, 0 }, { 26, 20, {1, 1, 1, 1}, 0, 1 }, { 62, 20, {1, 1, 1, 1}, 4, 1 } };
        draw_buf(slant, sizeof slant[0], 4, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        use(vs, layout, ps_texbias);
        for (int b = 0; b < 4; b++) {
            konst(0, 0, 0, 0, b - 1.0f);
            quad(b * 10 + 1, 24, b * 10 + 9, 32, 0xffffffff, 0, 0, 1, 1);
        }
        /* GenerateMips of a render-target texture with odd content */
        D3D11_TEXTURE2D_DESC g = { 8, 8, 0, 1, DXGI_FORMAT_R8G8B8A8_UNORM, { 1, 0 }, D3D11_USAGE_DEFAULT,
                                   D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET, 0, D3D11_RESOURCE_MISC_GENERATE_MIPS };
        ID3D11Texture2D *gt = NULL;
        HRESULT hr = ID3D11Device_CreateTexture2D(dev, &g, NULL, &gt);
        printf("CreateTexture2D GENERATE_MIPS %#lx\n", hr);
        if (gt) {
            DWORD px[64];
            for (int i = 0; i < 64; i++) px[i] = 0xff000000 | (i * 37 & 255) | (i * 91 & 255) << 8 | (i * 4) << 16;
            ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)gt, 0, NULL, px, 32, 0);
            ID3D11ShaderResourceView *gv = srv_of(gt);
            ID3D11DeviceContext_GenerateMips(ctx, gv);
            bind_srv(gv);
            sampler(D3D11_FILTER_MIN_MAG_MIP_POINT, D3D11_TEXTURE_ADDRESS_WRAP, NULL);
            use(vs, layout, ps_texlod);
            for (int l = 0; l < 4; l++) {
                konst(0, 0, 0, 0, l);
                quad(l * 16 + 1, 36, l * 16 + 15, 50, 0xffffffff, 0, 0, 1, 1);
            }
            ID3D11ShaderResourceView_Release(gv);
            ID3D11Texture2D_Release(gt);
        }
        shot("mipmaps", 3, 0);
        ID3D11SamplerState_Release(s);
        ID3D11SamplerState_Release(tri);
        ID3D11ShaderResourceView_Release(v);
        ID3D11Texture2D_Release(t);
        reset();
    }
    /* texture formats, 8x8 each drawn 1:1 with Load, rgb over gray and alpha in the second
     * picture */
    {
        unsigned char raw[8 * 8 * 16];
        for (int i = 0; i < (int)sizeof raw; i++) raw[i] = (unsigned char)(i * 29 + (i >> 4) * 7 + 3);
        unsigned short h16[8 * 8 * 4];
        static const unsigned short hv[] = { 0x0000, 0x3c00, 0x3800, 0x3555, 0x7bff, 0x0001, 0x8000, 0xbc00, 0x2e66, 0x3a00, 0x4000, 0x3bff, 0x0400, 0x3c01, 0x1400, 0x3666 };
        for (int i = 0; i < 256; i++) h16[i] = hv[(i * 7 + i / 16) % 16];
        float f32[64];
        for (int i = 0; i < 64; i++) f32[i] = i / 63.0f * 1.25f - 0.125f;
        static const unsigned char bc1[32] = { 0x00, 0xf8, 0x1f, 0x00, 0xe4, 0xe4, 0x1b, 0x1b,  0xe0, 0x07, 0x00, 0x00, 0x00, 0x55, 0xaa, 0xff,
                                               0x1f, 0x00, 0xe0, 0xff, 0x27, 0x27, 0x27, 0x27,  0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0xff, 0x00 };
        static const unsigned char bc3[64] = {
            0xff, 0x00, 0x88, 0xc6, 0x3e, 0x88, 0xc6, 0x3e,  0x00, 0xf8, 0x1f, 0x00, 0xe4, 0xe4, 0x1b, 0x1b,
            0x40, 0xc0, 0x24, 0x49, 0x92, 0x24, 0x49, 0x92,  0xe0, 0x07, 0x00, 0x00, 0x00, 0x55, 0xaa, 0xff,
            0x10, 0x20, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00,  0x1f, 0x00, 0xe0, 0xff, 0x27, 0x27, 0x27, 0x27,
            0x80, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0xff, 0x00 };
        static const unsigned char bc2[64] = {
            0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe,  0x00, 0xf8, 0x1f, 0x00, 0xe4, 0xe4, 0x1b, 0x1b,
            0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00,  0xe0, 0x07, 0x00, 0x00, 0x00, 0x55, 0xaa, 0xff,
            0xf0, 0xf0, 0xf0, 0xf0, 0x0f, 0x0f, 0x0f, 0x0f,  0x1f, 0x00, 0xe0, 0xff, 0x27, 0x27, 0x27, 0x27,
            0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88,  0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0xff, 0x00 };
        static const unsigned char bc4[32] = { 0xff, 0x00, 0x88, 0xc6, 0x3e, 0x88, 0xc6, 0x3e,  0x40, 0xc0, 0x24, 0x49, 0x92, 0x24, 0x49, 0x92,
                                               0x10, 0x20, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00,  0x80, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00 };
        static const unsigned char bc5[64] = {
            0xff, 0x00, 0x88, 0xc6, 0x3e, 0x88, 0xc6, 0x3e,  0x40, 0xc0, 0x24, 0x49, 0x92, 0x24, 0x49, 0x92,
            0x10, 0x20, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00,  0x80, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x20, 0xe0, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,  0xe0, 0x20, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11,
            0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff,  0x7f, 0x81, 0xb6, 0x6d, 0xdb, 0xb6, 0x6d, 0xdb };
        static const struct { DXGI_FORMAT f; const char *n; int kind, pitch; } fm[] = {
            { DXGI_FORMAT_R8G8B8A8_UNORM, "R8G8B8A8_UNORM", 0, 32 }, { DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, "R8G8B8A8_UNORM_SRGB", 0, 32 },
            { DXGI_FORMAT_B8G8R8A8_UNORM, "B8G8R8A8_UNORM", 0, 32 }, { DXGI_FORMAT_B8G8R8X8_UNORM, "B8G8R8X8_UNORM", 0, 32 },
            { DXGI_FORMAT_B8G8R8A8_UNORM_SRGB, "B8G8R8A8_UNORM_SRGB", 0, 32 }, { DXGI_FORMAT_R8G8B8A8_SNORM, "R8G8B8A8_SNORM", 0, 32 },
            { DXGI_FORMAT_B5G6R5_UNORM, "B5G6R5_UNORM", 0, 16 }, { DXGI_FORMAT_B5G5R5A1_UNORM, "B5G5R5A1_UNORM", 0, 16 },
            { DXGI_FORMAT_R10G10B10A2_UNORM, "R10G10B10A2_UNORM", 0, 32 }, { DXGI_FORMAT_R11G11B10_FLOAT, "R11G11B10_FLOAT", 0, 32 },
            { DXGI_FORMAT_R8_UNORM, "R8_UNORM", 0, 8 }, { DXGI_FORMAT_R8_SNORM, "R8_SNORM", 0, 8 }, { DXGI_FORMAT_A8_UNORM, "A8_UNORM", 0, 8 },
            { DXGI_FORMAT_R8G8_UNORM, "R8G8_UNORM", 0, 16 }, { DXGI_FORMAT_R8G8_SNORM, "R8G8_SNORM", 0, 16 },
            { DXGI_FORMAT_R16_UNORM, "R16_UNORM", 0, 16 }, { DXGI_FORMAT_R16G16_UNORM, "R16G16_UNORM", 0, 32 },
            { DXGI_FORMAT_R16G16B16A16_UNORM, "R16G16B16A16_UNORM", 0, 64 }, { DXGI_FORMAT_R16G16B16A16_SNORM, "R16G16B16A16_SNORM", 0, 64 },
            { DXGI_FORMAT_R16G16B16A16_FLOAT, "R16G16B16A16_FLOAT", 1, 64 }, { DXGI_FORMAT_R16_FLOAT, "R16_FLOAT", 1, 16 },
            { DXGI_FORMAT_R32_FLOAT, "R32_FLOAT", 2, 32 }, { DXGI_FORMAT_R9G9B9E5_SHAREDEXP, "R9G9B9E5_SHAREDEXP", 0, 32 },
            { DXGI_FORMAT_BC1_UNORM, "BC1_UNORM", 3, 16 }, { DXGI_FORMAT_BC1_UNORM_SRGB, "BC1_UNORM_SRGB", 3, 16 },
            { DXGI_FORMAT_BC2_UNORM, "BC2_UNORM", 4, 32 }, { DXGI_FORMAT_BC3_UNORM, "BC3_UNORM", 5, 32 },
            { DXGI_FORMAT_BC4_UNORM, "BC4_UNORM", 6, 16 }, { DXGI_FORMAT_BC4_SNORM, "BC4_SNORM", 6, 16 },
            { DXGI_FORMAT_BC5_UNORM, "BC5_UNORM", 7, 32 }, { DXGI_FORMAT_BC5_SNORM, "BC5_SNORM", 7, 32 },
        };
        const void *srcs[8] = { raw, h16, f32, bc1, bc2, bc3, bc4, bc5 };
        begin(0.5f, 0.5f, 0.5f, 0.5f);
        use(vs, layout, ps_load);
        for (int i = 0; i < (int)(sizeof fm / sizeof fm[0]); i++) {
            ID3D11Texture2D *t = texture(fm[i].f, 8, 8, 1, srcs[fm[i].kind], fm[i].pitch);
            ID3D11ShaderResourceView *v = t ? srv_of(t) : NULL;
            printf("texture %s %s\n", fm[i].n, v ? "ok" : "failed");
            if (v) {
                bind_srv(v);
                float x = (i % 7) * 9 + 1, y = (i / 7) * 9 + 1;
                quad(x, y, x + 8, y + 8, 0xffffffff, 0, 0, 1, 1);
                ID3D11ShaderResourceView_Release(v);
            }
            if (t) ID3D11Texture2D_Release(t);
        }
        shot("formats", 1, 1);
        reset();
    }
    /* blending over a gradient: factors, ops, separate alpha, blend factor, write masks */
    {
        begin(0, 0, 0, 0);
        struct v2 g[4] = { { 0, 0, {0, 0, 1, 0} }, { 64, 0, {1, 0, 0, 0.5f} }, { 0, 64, {0, 1, 0, 0.5f} }, { 64, 64, {1, 1, 1, 1} } };
        draw_buf(g, sizeof g[0], 4, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        static const int bl[16][6] = {
            { D3D11_BLEND_SRC_ALPHA, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_OP_ADD, D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD },
            { D3D11_BLEND_ONE, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, D3D11_BLEND_ONE, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD },
            { D3D11_BLEND_DEST_COLOR, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD, D3D11_BLEND_DEST_ALPHA, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD },
            { D3D11_BLEND_ONE, D3D11_BLEND_ONE, D3D11_BLEND_OP_REV_SUBTRACT, D3D11_BLEND_ONE, D3D11_BLEND_ONE, D3D11_BLEND_OP_REV_SUBTRACT },
            { D3D11_BLEND_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_SUBTRACT, D3D11_BLEND_ONE, D3D11_BLEND_ONE, D3D11_BLEND_OP_SUBTRACT },
            { D3D11_BLEND_ONE, D3D11_BLEND_ONE, D3D11_BLEND_OP_MIN, D3D11_BLEND_ONE, D3D11_BLEND_ONE, D3D11_BLEND_OP_MAX },
            { D3D11_BLEND_ONE, D3D11_BLEND_ONE, D3D11_BLEND_OP_MAX, D3D11_BLEND_ONE, D3D11_BLEND_ONE, D3D11_BLEND_OP_MIN },
            { D3D11_BLEND_INV_DEST_COLOR, D3D11_BLEND_SRC_COLOR, D3D11_BLEND_OP_ADD, D3D11_BLEND_INV_DEST_ALPHA, D3D11_BLEND_SRC_ALPHA, D3D11_BLEND_OP_ADD },
            { D3D11_BLEND_BLEND_FACTOR, D3D11_BLEND_INV_BLEND_FACTOR, D3D11_BLEND_OP_ADD, D3D11_BLEND_BLEND_FACTOR, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD },
            { D3D11_BLEND_SRC_ALPHA_SAT, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD },
            { D3D11_BLEND_DEST_ALPHA, D3D11_BLEND_INV_DEST_ALPHA, D3D11_BLEND_OP_ADD, D3D11_BLEND_ZERO, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD },
            { D3D11_BLEND_INV_SRC_COLOR, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_OP_ADD, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD },
            { D3D11_BLEND_ZERO, D3D11_BLEND_SRC_COLOR, D3D11_BLEND_OP_ADD, D3D11_BLEND_ZERO, D3D11_BLEND_SRC_ALPHA, D3D11_BLEND_OP_ADD },
            { D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD, D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD },
            { D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD, D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD },
            { D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD, D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD },
        };
        static const UINT8 mask[16] = { 0xf, 0xf, 0xf, 0xf, 0xf, 0xf, 0xf, 0xf, 0xf, 0xf, 0xf, 0xf, 0xf, 0x1, 0x6, 0x8 };
        for (int i = 0; i < 16; i++) {
            set_blend(TRUE, bl[i][0], bl[i][1], bl[i][2], bl[i][3], bl[i][4], bl[i][5], mask[i]);
            float x = (i & 3) * 16, y = (i >> 2) * 16;
            quad(x + 2, y + 2, x + 14, y + 14, 0x80c06020 + (i << 24), 0, 0, 0, 0);
        }
        shot("blend", 2, 1);
        reset();
        /* discard on alpha */
        begin(0, 0, 0, 1);
        use(vs, layout, ps_discard);
        struct v2 a[4] = { { 2, 2, {1, 1, 1, 0} }, { 62, 2, {1, 1, 1, 1} }, { 2, 62, {1, 0, 0, 0} }, { 62, 62, {0, 0, 1, 1} } };
        draw_buf(a, sizeof a[0], 4, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        shot("discard", 1, 0);
        reset();
    }
    /* depth funcs, write mask, stencil ops and test, depth bias */
    {
        begin(0, 0, 0.125f, 1);
        use(vsz, layoutz, ps_color);
        static const D3D11_COMPARISON_FUNC fn[8] = { D3D11_COMPARISON_NEVER, D3D11_COMPARISON_LESS, D3D11_COMPARISON_EQUAL, D3D11_COMPARISON_LESS_EQUAL,
                                                     D3D11_COMPARISON_GREATER, D3D11_COMPARISON_NOT_EQUAL, D3D11_COMPARISON_GREATER_EQUAL, D3D11_COMPARISON_ALWAYS };
        for (int i = 0; i < 8; i++) {
            float x = i * 8;
            set_depth(TRUE, D3D11_COMPARISON_ALWAYS, TRUE, FALSE, D3D11_COMPARISON_ALWAYS, D3D11_STENCIL_OP_KEEP, 0);
            quadz(x, 0, x + 8, 16, 0.5f, 0xff404040);
            set_depth(TRUE, fn[i], TRUE, FALSE, D3D11_COMPARISON_ALWAYS, D3D11_STENCIL_OP_KEEP, 0);
            quadz(x, 0, x + 8, 4, 0.25f, 0xffff0000);
            quadz(x, 4, x + 8, 8, 0.5f, 0xff00ff00);
            quadz(x, 8, x + 8, 12, 0.75f, 0xff0000ff);
            set_depth(TRUE, fn[i], FALSE, FALSE, D3D11_COMPARISON_ALWAYS, D3D11_STENCIL_OP_KEEP, 0);
            quadz(x + 2, 0, x + 6, 16, 0.5f, 0xffffff00);
        }
        /* stencil: overlapping quads increment, then colour where it equals 1 / 2 / 3 */
        set_depth(FALSE, D3D11_COMPARISON_ALWAYS, FALSE, TRUE, D3D11_COMPARISON_ALWAYS, D3D11_STENCIL_OP_INCR_SAT, 0);
        set_blend(FALSE, D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD, D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD, 0);
        quadz(2, 20, 30, 40, 0.5f, 0);
        quadz(16, 26, 44, 46, 0.5f, 0);
        quadz(24, 32, 50, 60, 0.5f, 0);
        set_blend(FALSE, D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD, D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD, 0xf);
        static const DWORD sc[3] = { 0xff800000, 0xff008000, 0xff0080ff };
        for (int r = 1; r <= 3; r++) {
            set_depth(FALSE, D3D11_COMPARISON_ALWAYS, FALSE, TRUE, D3D11_COMPARISON_EQUAL, D3D11_STENCIL_OP_KEEP, r);
            quadz(0, 18, 64, 62, 0.5f, sc[r - 1]);
        }
        set_depth(FALSE, D3D11_COMPARISON_ALWAYS, FALSE, TRUE, D3D11_COMPARISON_LESS, D3D11_STENCIL_OP_INVERT, 1);
        quadz(52, 18, 62, 62, 0.5f, 0xffffffff);
        /* depth bias: a coplanar quad pulled in front */
        set_depth(TRUE, D3D11_COMPARISON_LESS, TRUE, FALSE, D3D11_COMPARISON_ALWAYS, D3D11_STENCIL_OP_KEEP, 0);
        quadz(2, 50, 20, 62, 0.5f, 0xff808080);
        set_raster(D3D11_FILL_SOLID, D3D11_CULL_NONE, FALSE, FALSE, -100, 0, TRUE);
        quadz(8, 52, 26, 60, 0.5f, 0xffff00ff);
        shot("depth_stencil", 1, 0);
        reset();
    }
    /* copies: CopySubresourceRegion, UpdateSubresource with a box, a dynamic texture through
     * Map(WRITE_DISCARD), a staging upload, ClearRenderTargetView of a second target sampled */
    {
        begin(0, 0, 0, 1);
        DWORD px[64];
        for (int i = 0; i < 64; i++) px[i] = 0xff000000 | (i * 4) | (255 - i * 4) << 8 | (i % 8 * 32) << 16;
        ID3D11Texture2D *a = texture(DXGI_FORMAT_R8G8B8A8_UNORM, 8, 8, 1, px, 32);
        D3D11_BOX box = { 2, 2, 0, 6, 5, 1 };
        ID3D11DeviceContext_CopySubresourceRegion(ctx, (ID3D11Resource *)rt, 0, 4, 4, 0, (ID3D11Resource *)a, 0, NULL);
        ID3D11DeviceContext_CopySubresourceRegion(ctx, (ID3D11Resource *)rt, 0, 20, 4, 0, (ID3D11Resource *)a, 0, &box);
        DWORD red[12];
        for (int i = 0; i < 12; i++) red[i] = 0xff2040ff;
        D3D11_BOX ub = { 1, 3, 0, 5, 6, 1 };
        ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)a, 0, &ub, red, 16, 0);
        ID3D11DeviceContext_CopySubresourceRegion(ctx, (ID3D11Resource *)rt, 0, 30, 4, 0, (ID3D11Resource *)a, 0, NULL);
        D3D11_TEXTURE2D_DESC dd = { 8, 8, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, { 1, 0 }, D3D11_USAGE_DYNAMIC, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_WRITE, 0 };
        ID3D11Texture2D *dyn = NULL;
        ID3D11Device_CreateTexture2D(dev, &dd, NULL, &dyn);
        ID3D11ShaderResourceView *dv = dyn ? srv_of(dyn) : NULL;
        use(vs, layout, ps_load);
        ID3D11SamplerState *s = sampler(D3D11_FILTER_MIN_MAG_MIP_POINT, D3D11_TEXTURE_ADDRESS_CLAMP, NULL);
        for (int k = 0; k < 3 && dv; k++) {
            D3D11_MAPPED_SUBRESOURCE m;
            HRESULT hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)dyn, 0, D3D11_MAP_WRITE_DISCARD, 0, &m);
            if (k == 0) printf("Map WRITE_DISCARD %#lx pitch>=32 %d\n", hr, m.RowPitch >= 32);
            if (FAILED(hr)) break;
            for (int y = 0; y < 8; y++)
                for (int x = 0; x < 8; x++) ((DWORD *)((char *)m.pData + y * m.RowPitch))[x] = 0xff000000 | (k * 100) << 16 | (x * 32) << 8 | y * 32;
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)dyn, 0);
            bind_srv(dv);
            quad(4 + k * 10, 20, 12 + k * 10, 28, 0xffffffff, 0, 0, 1, 1);
        }
        /* a second render target, cleared, drawn into, then sampled */
        D3D11_TEXTURE2D_DESC td = { 16, 16, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, { 1, 0 }, D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET, 0, 0 };
        ID3D11Texture2D *t2 = NULL;
        ID3D11RenderTargetView *rv2 = NULL;
        ID3D11Device_CreateTexture2D(dev, &td, NULL, &t2);
        ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)t2, NULL, &rv2);
        float cc[4] = { 0.75f, 0.25f, 0.5f, 1 };
        ID3D11DeviceContext_ClearRenderTargetView(ctx, rv2, cc);
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rv2, NULL);
        viewport(0, 0, 16, 16, 0, 1);
        use(vs, layout, ps_color);
        quad(0, 0, 32, 32, 0xff00ff00, 0, 0, 0, 0); /* (the shader maps 64 to the viewport) */
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv, dsv);
        viewport(0, 0, W, H, 0, 1);
        ID3D11ShaderResourceView *v2 = srv_of(t2);
        use(vs, layout, ps_tex);
        bind_srv(v2);
        quad(4, 34, 36, 62, 0xffffffff, 0, 0, 1, 1);
        ID3D11ShaderResourceView *none = NULL;
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &none);
        ID3D11DeviceContext_CopySubresourceRegion(ctx, (ID3D11Resource *)rt, 0, 44, 34, 0, (ID3D11Resource *)t2, 0, NULL);
        shot("copies", 0, 0);
        if (v2) ID3D11ShaderResourceView_Release(v2);
        if (rv2) ID3D11RenderTargetView_Release(rv2);
        if (t2) ID3D11Texture2D_Release(t2);
        if (dv) ID3D11ShaderResourceView_Release(dv);
        if (dyn) ID3D11Texture2D_Release(dyn);
        ID3D11SamplerState_Release(s);
        ID3D11Texture2D_Release(a);
        reset();
    }
}

/* ---- shader arithmetic ---- */

#define N 8 /* an N x N target: N*N*4 results per operation */

static const char *MATH =
    "cbuffer c : register(b0) { float4 A[64]; float4 B[64]; float4 C[64]; }\n"
    "float4 vs(uint id : SV_VertexID) : SV_Position { return float4(id == 1 ? 3 : -1, id == 2 ? -3 : 1, 0.5, 1); }\n"
    "float4 f(float4 p : SV_Position) : SV_Target { uint i = (uint)p.y * 8 + (uint)p.x; float4 a = A[i], b = B[i], c = C[i]; return OP; }\n"
    "uint4 u(float4 p : SV_Position) : SV_Target { uint i = (uint)p.y * 8 + (uint)p.x;\n"
    "  uint4 a = asuint(A[i]), b = asuint(B[i]), c = asuint(C[i]); int4 sa = asint(A[i]), sb = asint(B[i]); return OP; }\n";

static ID3D11Texture2D *mf, *mu, *mfs, *mus;
static ID3D11RenderTargetView *mfv, *muv;
static ID3D11VertexShader *mvs;
static ID3D11Buffer *mcb;
static struct { float a[256], b[256], c[256]; } in;
#define in_a in.a
#define in_b in.b
#define in_c in.c

static unsigned lcg = 12345;
static unsigned rnd(void) { lcg = lcg * 1103515245u + 12345u; return lcg >> 1 ^ lcg << 17; }
static float fbits(unsigned u) { float f; memcpy(&f, &u, 4); return f; }
static unsigned bitsf(float f) { unsigned u; memcpy(&u, &f, 4); return u; }

/* runs OP over the inputs; out: 256 results as bits */
static int run(const char *op, int uint_target, unsigned *out) {
    D3D_SHADER_MACRO defs[2] = { { "OP", op }, { NULL, NULL } };
    ID3D11PixelShader *p = ps_of(MATH, uint_target ? "u" : "f", defs);
    if (!p) return 0;
    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)mcb, 0, NULL, &in, 0, 0);
    ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, uint_target ? &muv : &mfv, NULL);
    viewport(0, 0, N, N, 0, 1);
    ID3D11DeviceContext_IASetInputLayout(ctx, NULL);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ID3D11DeviceContext_VSSetShader(ctx, mvs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, p, NULL, 0);
    ID3D11DeviceContext_Draw(ctx, 3, 0);
    ID3D11Texture2D *s = uint_target ? mus : mfs;
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)s, (ID3D11Resource *)(uint_target ? mu : mf));
    D3D11_MAPPED_SUBRESOURCE m;
    if (FAILED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)s, 0, D3D11_MAP_READ, 0, &m))) { printf("math %s: Map failed\n", op); ID3D11PixelShader_Release(p); return 0; }
    for (int y = 0; y < N; y++) memcpy(out + y * N * 4, (char *)m.pData + y * m.RowPitch, N * 16);
    ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)s, 0);
    ID3D11PixelShader_Release(p);
    return 1;
}

/* NaNs as one pattern (their payloads are the hardware's) */
static unsigned canon(unsigned u) { return (u & 0x7fffffff) > 0x7f800000 ? 0x7fc00000 : u; }

static void inputs_float(int set) {
    static const unsigned special[] = {
        0x00000000, 0x80000000, 0x3f800000, 0xbf800000, 0x3f000000, 0x3fc00000, 0x40200000, 0xc0200000, 0x40600000, 0xbfc00000,
        0x00000001, 0x80000001, 0x007fffff, 0x00800000, 0x7f7fffff, 0xff7fffff, 0x7f800000, 0xff800000, 0x7fc00000, 0x501502f9,
        0xd01502f9, 0x4f000000, 0xcf000000, 0x4f800000, 0x4f7fffff, 0x3dcccccd, 0x40490fdb, 0x3eaaaaab, 0x33d6bf95, 0x477fe000,
        0x477ff000, 0x33800000, 0x3effffff, 0x4b800001, 0x4b000000, 0xcb000001, 0x3f7fffff, 0x3f800001, 0x42c80000, 0xc2c80000,
        0x41200000, 0x3c23d70a, 0x38d1b717, 0x4479c000, 0xc479c000, 0x3a83126f, 0x40c90fdb, 0xc0490fdb, 0x3fc90fdb, 0x42f60000,
    };
    int ns = sizeof special / sizeof special[0];
    for (int i = 0; i < 256; i++) {
        unsigned a, b;
        if (set == 0) { a = special[i % ns]; b = special[(i / ns + i * 7) % ns]; }
        else if (set == 1) { a = rnd(); b = rnd(); }
        else {
            /* moderate values: -64..64 (random), for functions defined with an absolute error */
            a = bitsf(((int)(rnd() % 1000001) - 500000) / 7812.5f);
            b = bitsf(((int)(rnd() % 1000001) - 500000) / 7812.5f);
        }
        in_a[i] = fbits(a); in_b[i] = fbits(b); in_c[i] = fbits(rnd() & 0x80000000 ? rnd() : special[rnd() % ns]);
    }
}

static void inputs_int(void) {
    static const unsigned special[] = { 0, 1, 2, 3, 31, 32, 33, 63, 0xffffffff, 0xfffffffe, 0x7fffffff, 0x80000000, 0x80000001, 0x55555555,
                                        0xaaaaaaaa, 0x0000ffff, 0xffff0000, 0x12345678, 0xfedcba98, 0x00010000, 0x00800000, 0x3f800000, 7, 100 };
    int ns = sizeof special / sizeof special[0];
    for (int i = 0; i < 256; i++) {
        unsigned a = i < ns * 6 ? special[i % ns] : rnd(), b = i < ns * 6 ? special[(i / ns * 5 + i) % ns] : rnd() >> (rnd() % 32);
        in_a[i] = fbits(a); in_b[i] = fbits(b); in_c[i] = fbits(rnd());
    }
}

/* exact: every result as bits; approx (ulps of a float, rel) or (absolute error, abs) */
static void math_exact(const char *name, const char *op, int u) {
    unsigned out[256];
    if (!run(op, u, out)) return;
    for (int i = 0; i < 256; i++)
        printf("%s %08x %08x %08x -> %08x\n", name, bitsf(in_a[i]), bitsf(in_b[i]), bitsf(in_c[i]), u ? out[i] : canon(out[i]));
}

static void math_ulps(const char *name, const char *op, unsigned long long ulps, double abs) {
    unsigned out[256];
    if (!run(op, 0, out)) return;
    for (int i = 0; i < 256; i++) {
        unsigned o = canon(out[i]);
        char label[64];
        snprintf(label, sizeof label, "%s:%08x:%08x", name, bitsf(in_a[i]), bitsf(in_b[i]));
        double d = fbits(o);
        unsigned long long n = ulps << 29;
        if (abs > 0) {
            /* (out + 4: its ulp is 2^-50, so the tolerance is absolute) */
            if (isfinite(d)) d += 4;
            n = (unsigned long long)(abs * 1125899906842624.0);
        }
        unsigned long long b;
        memcpy(&b, &d, 8);
        printf("ulp %llu %s %08lx%08lx\n", n, label, (unsigned long)(b >> 32), (unsigned long)b);
    }
}

static void math(void) {
    D3D11_TEXTURE2D_DESC d = { N, N, 1, 1, DXGI_FORMAT_R32G32B32A32_FLOAT, { 1, 0 }, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, 0, 0 };
    ID3D11Device_CreateTexture2D(dev, &d, NULL, &mf);
    d.Format = DXGI_FORMAT_R32G32B32A32_UINT;
    ID3D11Device_CreateTexture2D(dev, &d, NULL, &mu);
    d.Usage = D3D11_USAGE_STAGING; d.BindFlags = 0; d.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    ID3D11Device_CreateTexture2D(dev, &d, NULL, &mus);
    d.Format = DXGI_FORMAT_R32G32B32A32_FLOAT;
    ID3D11Device_CreateTexture2D(dev, &d, NULL, &mfs);
    if (!mf || !mu || !mfs || !mus) { puts("math targets failed"); return; }
    ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)mf, NULL, &mfv);
    ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)mu, NULL, &muv);
    mvs = vs_of(MATH, "vs", NULL);
    D3D11_BUFFER_DESC bd = { 3 * 64 * 16, D3D11_USAGE_DEFAULT, D3D11_BIND_CONSTANT_BUFFER, 0, 0, 0 };
    ID3D11Device_CreateBuffer(dev, &bd, NULL, &mcb);
    ID3D11DeviceContext_PSSetConstantBuffers(ctx, 0, 1, &mcb);
    set_blend(FALSE, D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD, D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_OP_ADD, 0xf);
    set_depth(FALSE, D3D11_COMPARISON_ALWAYS, FALSE, FALSE, D3D11_COMPARISON_ALWAYS, D3D11_STENCIL_OP_KEEP, 0);
    set_raster(D3D11_FILL_SOLID, D3D11_CULL_NONE, FALSE, FALSE, 0, 0, TRUE);

    /* IEEE operations the spec makes exact (denormals may be flushed: Windows' choice shows) */
    static const struct { const char *n, *op; int u; } exact[] = {
        { "add", "a + b" }, { "sub", "a - b" }, { "mul", "a * b" }, { "min", "min(a, b)" }, { "max", "max(a, b)" },
        { "floor", "floor(a)" }, { "ceil", "ceil(a)" }, { "round", "round(a)" }, { "trunc", "trunc(a)" }, { "frac", "frac(a)" },
        { "abs", "abs(a)" }, { "neg", "-a" }, { "sat", "saturate(a)" }, { "lt", "a < b ? 1.0 : 0.0" }, { "eq", "a == b ? 1.0 : 0.0" },
        { "ne", "a != b ? 1.0 : 0.0" }, { "ge", "a >= b ? 1.0 : 0.0" }, { "sign", "sign(a)" }, { "step", "step(a, b)" },
        { "movc", "a > 0 ? b : c" }, { "clamp", "clamp(c, min(a, b), max(a, b))" }, { "dot2", "dot(a.xy, b.xy).xxxx" },
        { "lerp", "lerp(a, b, 0.5)" }, { "isnan", "isnan(a) ? 1.0 : 0.0" }, { "isinf", "isinf(a) ? 1.0 : 0.0" },
        { "itof", "float4(asint(a))" }, { "utof", "float4(asuint(a))" }, { "f16rt", "f16tof32(f32tof16(a))" },
        { "f16dec", "f16tof32(asuint(a) & 0xffff)" },
        /* (results as bits: uint target, a = the float's bits) */
        { "ftoi", "uint4(int4(asfloat(a)))", 1 }, { "ftou", "uint4(asfloat(a))", 1 }, { "f32tof16", "f32tof16(asfloat(a))", 1 },
    };
    /* approximate ones: the functional spec's error bounds (ulps of a float, or absolute) */
    static const struct { const char *n, *op; unsigned ulps; } approx[] = {
        { "div", "a / b", 3 }, { "rcp", "rcp(a)", 1 }, { "rsq", "rsqrt(a)", 2 }, { "sqrt", "sqrt(a)", 1 },
        { "exp2", "exp2(a)", 3 }, { "log2", "log2(a)", 3 }, { "exp", "exp(a)", 256 }, { "log", "log(a)", 8 }, { "mad", "mad(a, b, c)", 1 },
    };
    /* on moderate inputs (-64..64): an absolute error, or ulps */
    static const struct { const char *n, *op; unsigned ulps; double abs; } trig[] = {
        { "sin", "sin(a)", 0, 0.0008 }, { "cos", "cos(a)", 0, 0.0008 }, { "tan", "tan(a * 0.01)", 0, 0.002 }, { "atan", "atan(a)", 0, 0.001 },
        { "atan2", "atan2(a, b)", 0, 0.002 }, { "asin", "asin(frac(a) * 2 - 1)", 0, 0.002 }, { "acos", "acos(frac(a) * 2 - 1)", 0, 0.002 },
        { "sincos", "sin(a) * cos(b)", 0, 0.002 }, { "tanh", "tanh(a * 0.1)", 0, 0.002 }, { "smooth", "smoothstep(-8, 8, a)", 0, 0.0001 },
        { "normalize", "normalize(float4(a.xyz, 1))", 0, 0.0001 }, { "length", "length(a.xy * 0.125).xxxx", 0, 0.0001 },
        { "fmod", "fmod(a, b)", 0, 0.001 }, { "pow", "pow(abs(a) * 0.1, b * 0.05)", 64, 0 }, { "exp", "exp(a * 0.5)", 16, 0 },
        { "log", "log(abs(a))", 0, 0.00001 }, { "ldexp", "ldexp(a, floor(b * 0.25))", 0, 0 }, { "fdiv", "a / b", 3, 0 },
    };
    for (int set = 0; set < 2; set++) {
        inputs_float(set);
        for (int i = 0; i < (int)(sizeof exact / sizeof exact[0]); i++) {
            char n[32];
            snprintf(n, sizeof n, "f%d.%s", set, exact[i].n);
            math_exact(n, exact[i].op, exact[i].u);
        }
        for (int i = 0; i < (int)(sizeof approx / sizeof approx[0]); i++) {
            char n[32];
            snprintf(n, sizeof n, "f%d.%s", set, approx[i].n);
            math_ulps(n, approx[i].op, approx[i].ulps, 0);
        }
    }
    inputs_float(2);
    for (int i = 0; i < (int)(sizeof trig / sizeof trig[0]); i++) {
        char n[32];
        snprintf(n, sizeof n, "t.%s", trig[i].n);
        math_ulps(n, trig[i].op, trig[i].ulps, trig[i].abs);
    }
    static const char *ints[][2] = {
        { "iadd", "a + b" }, { "isub", "a - b" }, { "imul", "a * b" }, { "udiv", "b ? a / b : 0xdead" }, { "urem", "b ? a % b : 0xdead" },
        { "idiv", "uint4(sb != 0 && !(sa == 0x80000000 && sb == -1) ? sa / sb : 0x1dead)" },
        { "irem", "uint4(sb != 0 && !(sa == 0x80000000 && sb == -1) ? sa % sb : 0x1dead)" },
        { "shl", "a << b" }, { "ushr", "a >> b" }, { "ishr", "uint4(sa >> sb)" }, { "and", "a & b" }, { "or", "a | b" },
        { "xor", "a ^ b" }, { "not", "~a" }, { "ineg", "uint4(-sa)" }, { "countbits", "countbits(a)" },
        { "firstbithigh", "firstbithigh(a)" }, { "firstbitlow", "firstbitlow(a)" }, { "ifirstbithigh", "uint4(firstbithigh(sa))" },
        { "reversebits", "reversebits(a)" }, { "umin", "min(a, b)" }, { "umax", "max(a, b)" }, { "imin", "uint4(min(sa, sb))" },
        { "imax", "uint4(max(sa, sb))" }, { "ult", "a < b ? 1 : 0" }, { "ilt", "sa < sb ? 1 : 0" }, { "ubfe", "(a >> 5) & 0x3ff" },
        { "ibfe", "uint4((sa << 7) >> 20)" }, { "bfi", "(a & ~0x0ff0) | ((b << 4) & 0x0ff0)" }, { "iabs", "uint4(abs(sa))" },
        { "itof", "asuint(float4(sa))" }, { "utof", "asuint(float4(a))" }, { "ftoi", "uint4(int4(asfloat(a)))" }, { "ftou", "uint4(asfloat(a))" },
        { "f32tof16", "f32tof16(asfloat(a))" }, { "f16tof32", "asuint(f16tof32(a))" }, { "movc", "a > b ? c : a ^ b" },
        { "sel_index", "uint4(A[a.x & 63].x != 0, B[b.y & 63].y != 0, asuint(C[c.z & 63].z) & 7, a.w & 63)" },
    };
    inputs_int();
    for (int i = 0; i < (int)(sizeof ints / sizeof ints[0]); i++) {
        char n[32];
        snprintf(n, sizeof n, "i.%s", ints[i][0]);
        math_exact(n, ints[i][1], 1);
    }
    ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv, dsv);
}

int main(void) {
    static const D3D_FEATURE_LEVEL want = D3D_FEATURE_LEVEL_11_0;
    D3D_FEATURE_LEVEL got = 0;
    HRESULT hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_WARP, NULL, 0, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) { printf("skip D3D11CreateDevice %#lx\n", hr); return 0; }
    printf("feature level %#x\n", got);

    /* what games ask */
    {
        static const struct { DXGI_FORMAT f; const char *n; } fm[] = {
            { DXGI_FORMAT_R8G8B8A8_UNORM, "R8G8B8A8_UNORM" }, { DXGI_FORMAT_B8G8R8A8_UNORM, "B8G8R8A8_UNORM" },
            { DXGI_FORMAT_B8G8R8X8_UNORM, "B8G8R8X8_UNORM" }, { DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, "R8G8B8A8_UNORM_SRGB" },
            { DXGI_FORMAT_B5G6R5_UNORM, "B5G6R5_UNORM" }, { DXGI_FORMAT_B5G5R5A1_UNORM, "B5G5R5A1_UNORM" },
            { DXGI_FORMAT_B4G4R4A4_UNORM, "B4G4R4A4_UNORM" }, { DXGI_FORMAT_R10G10B10A2_UNORM, "R10G10B10A2_UNORM" },
            { DXGI_FORMAT_R16G16B16A16_FLOAT, "R16G16B16A16_FLOAT" }, { DXGI_FORMAT_R32G32B32A32_FLOAT, "R32G32B32A32_FLOAT" },
            { DXGI_FORMAT_R32_FLOAT, "R32_FLOAT" }, { DXGI_FORMAT_R8_UNORM, "R8_UNORM" }, { DXGI_FORMAT_A8_UNORM, "A8_UNORM" },
            { DXGI_FORMAT_BC1_UNORM, "BC1_UNORM" }, { DXGI_FORMAT_BC3_UNORM, "BC3_UNORM" }, { DXGI_FORMAT_BC7_UNORM, "BC7_UNORM" },
            { DXGI_FORMAT_D24_UNORM_S8_UINT, "D24_UNORM_S8_UINT" }, { DXGI_FORMAT_D32_FLOAT, "D32_FLOAT" }, { DXGI_FORMAT_D16_UNORM, "D16_UNORM" },
            { DXGI_FORMAT_R32G32B32_FLOAT, "R32G32B32_FLOAT" }, { DXGI_FORMAT_R16_UINT, "R16_UINT" }, { DXGI_FORMAT_R32_UINT, "R32_UINT" },
            { DXGI_FORMAT_YUY2, "YUY2" }, { DXGI_FORMAT_NV12, "NV12" },
        };
        /* the bits a game decides by: texture2d, render target, blendable, depth, mips, vertex/index buffer */
        UINT keep = D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_RENDER_TARGET | D3D11_FORMAT_SUPPORT_BLENDABLE |
                    D3D11_FORMAT_SUPPORT_DEPTH_STENCIL | D3D11_FORMAT_SUPPORT_MIP | D3D11_FORMAT_SUPPORT_IA_VERTEX_BUFFER |
                    D3D11_FORMAT_SUPPORT_IA_INDEX_BUFFER | D3D11_FORMAT_SUPPORT_SHADER_SAMPLE;
        for (int i = 0; i < (int)(sizeof fm / sizeof fm[0]); i++) {
            UINT s = 0;
            HRESULT r = ID3D11Device_CheckFormatSupport(dev, fm[i].f, &s);
            UINT q = 0;
            ID3D11Device_CheckMultisampleQualityLevels(dev, fm[i].f, 4, &q);
            printf("format %s %s %#x msaa4 %d\n", fm[i].n, SUCCEEDED(r) ? "ok" : "no", s & keep, q > 0);
        }
        D3D11_FEATURE_DATA_THREADING th = { 0 };
        ID3D11Device_CheckFeatureSupport(dev, D3D11_FEATURE_THREADING, &th, sizeof th);
        printf("threading concurrent creates %d command lists %d\n", th.DriverConcurrentCreates, th.DriverCommandLists);
        D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS ho = { 0 };
        ID3D11Device_CheckFeatureSupport(dev, D3D11_FEATURE_D3D10_X_HARDWARE_OPTIONS, &ho, sizeof ho);
        printf("compute on 10.x %d\n", ho.ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x);
    }

    D3D11_TEXTURE2D_DESC d = { W, H, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, { 1, 0 }, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, 0, 0 };
    ID3D11Device_CreateTexture2D(dev, &d, NULL, &rt);
    d.Usage = D3D11_USAGE_STAGING; d.BindFlags = 0; d.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    ID3D11Device_CreateTexture2D(dev, &d, NULL, &stage);
    d.Format = DXGI_FORMAT_D24_UNORM_S8_UINT; d.Usage = D3D11_USAGE_DEFAULT; d.BindFlags = D3D11_BIND_DEPTH_STENCIL; d.CPUAccessFlags = 0;
    ID3D11Device_CreateTexture2D(dev, &d, NULL, &zbuf);
    if (!rt || !stage || !zbuf) { puts("targets failed"); return 1; }
    ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)rt, NULL, &rtv);
    ID3D11Device_CreateDepthStencilView(dev, (ID3D11Resource *)zbuf, NULL, &dsv);

    ID3DBlob *b2 = NULL, *b3 = NULL, *b4 = NULL;
    vs = vs_of(SRC, "vs", &b2);
    vsz = vs_of(SRC, "vsz", &b3);
    vsw = vs_of(SRC, "vsw", &b4);
    if (!vs || !vsz || !vsw) return 1;
    D3D11_INPUT_ELEMENT_DESC e2[3] = { { "POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0 },
                                       { "COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 8, D3D11_INPUT_PER_VERTEX_DATA, 0 },
                                       { "TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 24, D3D11_INPUT_PER_VERTEX_DATA, 0 } };
    D3D11_INPUT_ELEMENT_DESC e3[3] = { { "POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0 },
                                       { "COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 12, D3D11_INPUT_PER_VERTEX_DATA, 0 },
                                       { "TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 28, D3D11_INPUT_PER_VERTEX_DATA, 0 } };
    D3D11_INPUT_ELEMENT_DESC e4[3] = { { "POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0 },
                                       { "COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 16, D3D11_INPUT_PER_VERTEX_DATA, 0 },
                                       { "TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 32, D3D11_INPUT_PER_VERTEX_DATA, 0 } };
    ID3D11Device_CreateInputLayout(dev, e2, 3, ID3D10Blob_GetBufferPointer(b2), ID3D10Blob_GetBufferSize(b2), &layout);
    ID3D11Device_CreateInputLayout(dev, e3, 3, ID3D10Blob_GetBufferPointer(b3), ID3D10Blob_GetBufferSize(b3), &layoutz);
    ID3D11Device_CreateInputLayout(dev, e4, 3, ID3D10Blob_GetBufferPointer(b4), ID3D10Blob_GetBufferSize(b4), &layoutw);
    ps_color = ps_of(SRC, "color", NULL); ps_tex = ps_of(SRC, "tex", NULL); ps_texlod = ps_of(SRC, "texlod", NULL);
    ps_texbias = ps_of(SRC, "texbias", NULL); ps_load = ps_of(SRC, "load", NULL); ps_konst = ps_of(SRC, "konst", NULL);
    ps_flat = ps_of(SRC, "flat", NULL); ps_noper = ps_of(SRC, "noper", NULL); ps_discard = ps_of(SRC, "discard_", NULL);
    ps_pos = ps_of(SRC, "pos", NULL);
    D3D11_BUFFER_DESC cbd = { 32, D3D11_USAGE_DEFAULT, D3D11_BIND_CONSTANT_BUFFER, 0, 0, 0 };
    ID3D11Device_CreateBuffer(dev, &cbd, NULL, &cb);
    ID3D11DeviceContext_PSSetConstantBuffers(ctx, 0, 1, &cb);

    pictures();
    math();
    return 0;
}
