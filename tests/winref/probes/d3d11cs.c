/* Direct3D 11 compute shaders (feature level 11_0; WARP on the Windows runner), compiled with the
 * system's D3DCompile as cs_5_0: the thread ID semantics, typed UAVs of 32-bit integers, floats
 * and UNORM bytes (stores, and loads of what the UAV held), groupshared memory with barriers,
 * Dispatch and DispatchIndirect, a dispatch with nothing bound, the runtime unbinding a
 * compute shader's SRV of a texture bound as its UAV, and the NVIDIA Image Scaling SDK's shader
 * (tests/compute/nis, as 天獄のヴィネア compiles it at run time: scaler and sharpen-only, SDR and
 * HDR, its own include through ID3DInclude) on a noise picture.
 * libs: -ld3d11 -ld3dcompiler_47
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <d3d11.h>
#include <d3dcompiler.h>
#include <math.h>

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;

/* this probe's resources (d3d11cs.rc): the NIS sources and coefficient tables */
static const void *res(const char *name, DWORD *size) {
    HRSRC r = FindResourceA(NULL, name, (LPCSTR)RT_RCDATA);
    if (!r) return NULL;
    *size = SizeofResource(NULL, r);
    return LockResource(LoadResource(NULL, r));
}

static HRESULT WINAPI inc_open(ID3DInclude *self, D3D_INCLUDE_TYPE type, LPCSTR name, LPCVOID parent, LPCVOID *data, UINT *bytes) {
    DWORD n = 0;
    const void *p = !strcmp(name, "NIS_Scaler.h") ? res("NIS_SCALER", &n) : NULL;
    if (!p) return E_FAIL;
    *data = p;
    *bytes = n;
    return S_OK;
}
static HRESULT WINAPI inc_close(ID3DInclude *self, LPCVOID data) { return S_OK; }
static ID3DIncludeVtbl inc_vtbl = { inc_open, inc_close };
static ID3DInclude inc = { &inc_vtbl };

static ID3D11ComputeShader *cs_of(const char *src, size_t len, const D3D_SHADER_MACRO *defs, const char *what) {
    ID3DBlob *b = NULL, *err = NULL;
    HRESULT hr = D3DCompile(src, len, "probe", defs, &inc, "main", "cs_5_0", 0, 0, &b, &err);
    if (FAILED(hr)) {
        printf("D3DCompile %s failed %#lx\n", what, hr);
        if (err) ID3D10Blob_Release(err);
        return NULL;
    }
    if (err) ID3D10Blob_Release(err);
    ID3D11ComputeShader *s = NULL;
    hr = ID3D11Device_CreateComputeShader(dev, ID3D10Blob_GetBufferPointer(b), ID3D10Blob_GetBufferSize(b), NULL, &s);
    ID3D10Blob_Release(b);
    if (FAILED(hr)) printf("CreateComputeShader %s failed %#lx\n", what, hr);
    return s;
}
#define CS(src, what) cs_of(src, strlen(src), NULL, what)

static ID3D11Texture2D *tex(int w, int h, DXGI_FORMAT f, UINT bind, const void *data, int pitch) {
    D3D11_TEXTURE2D_DESC d = { w, h, 1, 1, f, { 1, 0 }, D3D11_USAGE_DEFAULT, bind, 0, 0 };
    D3D11_SUBRESOURCE_DATA sd = { data, pitch, 0 };
    ID3D11Texture2D *t = NULL;
    HRESULT hr = ID3D11Device_CreateTexture2D(dev, &d, data ? &sd : NULL, &t);
    if (FAILED(hr)) printf("CreateTexture2D %dx%d format %d bind %#x failed %#lx\n", w, h, f, bind, hr);
    return t;
}
static ID3D11UnorderedAccessView *uav_of(ID3D11Texture2D *t) {
    ID3D11UnorderedAccessView *v = NULL;
    if (t && FAILED(ID3D11Device_CreateUnorderedAccessView(dev, (ID3D11Resource *)t, NULL, &v))) printf("CreateUnorderedAccessView failed\n");
    return v;
}
static ID3D11ShaderResourceView *srv_of(ID3D11Texture2D *t) {
    ID3D11ShaderResourceView *v = NULL;
    if (t && FAILED(ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)t, NULL, &v))) printf("CreateShaderResourceView failed\n");
    return v;
}
static ID3D11Buffer *cbuf(const void *data, int size) {
    D3D11_BUFFER_DESC d = { (size + 15) & ~15, D3D11_USAGE_DEFAULT, D3D11_BIND_CONSTANT_BUFFER, 0, 0, 0 };
    static unsigned char pad[4096];
    memset(pad, 0, sizeof pad);
    memcpy(pad, data, size);
    D3D11_SUBRESOURCE_DATA sd = { pad, 0, 0 };
    ID3D11Buffer *b = NULL;
    if (FAILED(ID3D11Device_CreateBuffer(dev, &d, &sd, &b))) printf("CreateBuffer (constants) failed\n");
    return b;
}

/* a texture's texels: bytes per texel `bpp`, into out (w*h*bpp) */
static int readback(ID3D11Texture2D *t, int w, int h, DXGI_FORMAT f, int bpp, void *out) {
    D3D11_TEXTURE2D_DESC d = { w, h, 1, 1, f, { 1, 0 }, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ, 0 };
    ID3D11Texture2D *s = NULL;
    if (FAILED(ID3D11Device_CreateTexture2D(dev, &d, NULL, &s))) return 0;
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)s, (ID3D11Resource *)t);
    D3D11_MAPPED_SUBRESOURCE m;
    int ok = SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)s, 0, D3D11_MAP_READ, 0, &m));
    if (ok)
        for (int y = 0; y < h; y++) memcpy((char *)out + y * w * bpp, (char *)m.pData + y * m.RowPitch, w * bpp);
    if (ok) ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)s, 0);
    ID3D11Texture2D_Release(s);
    if (!ok) printf("Map failed\n");
    return ok;
}

/* RGBA bytes as two pictures: the colors and the alpha */
static void rgba_image(const char *name, const unsigned char *px, int w, int h, int tol) {
    unsigned char *c = malloc(w * h * 4), *a = malloc(w * h * 4);
    for (int i = 0; i < w * h; i++) {
        c[i * 4] = px[i * 4 + 2], c[i * 4 + 1] = px[i * 4 + 1], c[i * 4 + 2] = px[i * 4], c[i * 4 + 3] = 0;
        a[i * 4] = a[i * 4 + 1] = a[i * 4 + 2] = px[i * 4 + 3], a[i * 4 + 3] = 0;
    }
    probe_image(name, c, w, h, w * 4, tol);
    char n[96];
    snprintf(n, sizeof n, "%s_alpha", name);
    probe_image(n, a, w, h, w * 4, tol);
    free(c);
    free(a);
}

static void words(const char *name, const unsigned *v, int n, int per_line) {
    for (int i = 0; i < n; i += per_line) {
        printf("%s %d:", name, i);
        for (int k = i; k < i + per_line && k < n; k++) printf(" %x", v[k]);
        printf("\n");
    }
}

static void unbind(void) {
    ID3D11UnorderedAccessView *nu[2] = { 0 };
    ID3D11ShaderResourceView *ns[4] = { 0 };
    ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 2, nu, NULL);
    ID3D11DeviceContext_CSSetShaderResources(ctx, 0, 4, ns);
}

static const char *IDS =
    "RWTexture2D<uint4> u : register(u0);\n"
    "[numthreads(4, 2, 1)] void main(uint3 t : SV_DispatchThreadID, uint3 g : SV_GroupID, uint3 l : SV_GroupThreadID, uint i : SV_GroupIndex) {\n"
    "  u[t.xy] = uint4(t.x | t.y << 8 | t.z << 16, g.x | g.y << 8 | g.z << 16, l.x | l.y << 8 | l.z << 16, i); }\n";

/* an inclusive prefix sum in groupshared memory (Hillis-Steele, a barrier per step) */
static const char *SCAN =
    "RWTexture2D<uint> u : register(u1); groupshared uint a[64], b[64];\n"
    "[numthreads(64, 1, 1)] void main(uint3 t : SV_DispatchThreadID, uint i : SV_GroupIndex, uint3 g : SV_GroupID) {\n"
    "  a[i] = (i * 7 + g.y * 3) % 13; GroupMemoryBarrierWithGroupSync();\n"
    "  [loop] for (uint s = 1; s < 64; s <<= 1) {\n"
    "    b[i] = i >= s ? a[i] + a[i - s] : a[i]; GroupMemoryBarrierWithGroupSync();\n"
    "    a[i] = b[i]; GroupMemoryBarrierWithGroupSync(); }\n"
    "  u[t.xy] = a[63 - i] * 1000 + a[i]; }\n";

/* what the UAV held, read back (ld_uav_typed), as SnapTex.csh does */
static const char *RMW =
    "RWTexture2D<unorm float4> u : register(u0);\n"
    "[numthreads(1, 1, 1)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = u[t.xy].yzwx * 0.5 + 0.25; }\n";

/* exact float operations into a float UAV, from an SRV and constants */
static const char *FLOATS =
    "Texture2D<float4> t : register(t0); cbuffer C : register(b0) { float4 k; };\n"
    "RWTexture2D<float4> u : register(u0);\n"
    "[numthreads(8, 8, 1)] void main(uint3 id : SV_DispatchThreadID) {\n"
    "  float4 v = t.Load(int3(id.xy, 0));\n"
    "  u[id.xy] = float4(v.x * k.x + v.y, min(v.z, k.y) - max(v.w, k.z), floor(v.x * 255.0) + frac(v.y * 3.0), (float)(int)(v.z * 1000.0) / 8.0); }\n";

/* NIS_Config.h NVScalerUpdateConfig / NVSharpenUpdateConfig (SDK 1.0.3): the constants */
static void nis_config(unsigned *w, float sharp, int iw, int ih, int ow, int oh, int hdr, int vp) {
    sharp = sharp < 0 ? 0 : sharp > 1 ? 1 : sharp;
    float s = sharp - 0.5f;
    float MaxScale = s >= 0 ? 1.25f : 1.75f, MinScale = s >= 0 ? 1.25f : 1.0f, LimitScale = s >= 0 ? 1.25f : 1.0f;
    float kDetectRatio = 2 * 1127.f / 1024.f, kDetectThres = 64.0f / 1024.0f, kMinContrastRatio = 2.0f, kMaxContrastRatio = 10.0f;
    float kSharpStartY = 0.45f, kSharpEndY = 0.9f;
    float kSharpStrengthMin = fmaxf(0.0f, 0.4f + s * MinScale * 1.2f), kSharpStrengthMax = 1.6f + s * MaxScale * 1.8f;
    float kSharpLimitMin = fmaxf(0.1f, 0.14f + s * LimitScale * 0.32f), kSharpLimitMax = 0.5f + s * LimitScale * 0.6f;
    if (hdr) {
        kDetectThres = 32.0f / 1024.0f;
        kMinContrastRatio = 1.5f;
        kMaxContrastRatio = 5.0f;
        kSharpStrengthMin = fmaxf(0.0f, 0.4f + s * MinScale * 1.1f);
        kSharpStrengthMax = 2.2f + s * MaxScale * 1.8f;
        kSharpLimitMin = fmaxf(0.06f, 0.10f + s * LimitScale * 0.28f);
        kSharpLimitMax = 0.6f + s * LimitScale * 0.6f;
        if (hdr == 2) kSharpStartY = 0.35f, kSharpEndY = 0.55f;
        else kSharpStartY = 0.3f, kSharpEndY = 0.5f;
    }
    /* (viewports: the input's and output's whole, or an inner part when vp) */
    int ix = vp ? 3 : 0, iy = vp ? 2 : 0, ivw = vp ? iw - 7 : iw, ivh = vp ? ih - 5 : ih;
    int ox = vp ? 5 : 0, oy = vp ? 4 : 0, ovw = vp ? ow - 9 : ow, ovh = vp ? oh - 7 : oh;
    float f[18] = { kDetectRatio, kDetectThres, kMinContrastRatio, 1.0f / (kMaxContrastRatio - kMinContrastRatio),
                    1.0f, 1.0f / 255.0f, kSharpStartY, 1.0f / (kSharpEndY - kSharpStartY),
                    kSharpStrengthMin, kSharpStrengthMax - kSharpStrengthMin, kSharpLimitMin, kSharpLimitMax - kSharpLimitMin,
                    (float)ivw / ovw, (float)ivh / ovh, 1.0f / ow, 1.0f / oh, 1.0f / iw, 1.0f / ih };
    memcpy(w, f, sizeof f);
    unsigned v[10] = { ix, iy, ivw, ivh, ox, oy, ovw, ovh, 0, 0 };
    memcpy(w + 18, v, sizeof v);
}

static ID3D11Texture2D *coef_tex(const char *name) {
    DWORD n = 0;
    const void *p = res(name, &n);
    if (!p || n != 2048) {
        printf("resource %s: %lu bytes\n", name, n);
        return NULL;
    }
    return tex(2, 64, DXGI_FORMAT_R32G32B32A32_FLOAT, D3D11_BIND_SHADER_RESOURCE, p, 32);
}

/* noise RGBA bytes (alpha 255), as the differential runs on WebGPU (few exact ties) */
static unsigned char *noise(int w, int h, unsigned seed) {
    unsigned char *b = malloc(w * h * 4);
    unsigned x = seed * 2654435761u;
    for (int i = 0; i < w * h * 4; i++) {
        x = (x * 1103515245u + 12345u) & 0x7fffffff;
        b[i] = i % 4 == 3 ? 255 : (x >> 16) & 255;
    }
    return b;
}

static void nis(const char *name, int scaler, int iw, int ih, int ow, int oh, float sharp, int hdr, int tg, int vp, unsigned seed) {
    DWORD n = 0;
    const char *src = res("NIS_MAIN", &n);
    if (!src) {
        printf("%s: no NIS_MAIN resource\n", name);
        return;
    }
    char s_scaler[4], s_hdr[4], s_tg[8], s_vp[4], s_bw[8], s_bh[8];
    int bw = 32, bh = scaler ? 24 : 32;
    snprintf(s_scaler, 4, "%d", scaler), snprintf(s_hdr, 4, "%d", hdr), snprintf(s_tg, 8, "%d", tg), snprintf(s_vp, 4, "%d", vp);
    snprintf(s_bw, 8, "%d", bw), snprintf(s_bh, 8, "%d", bh);
    D3D_SHADER_MACRO defs[] = { { "NIS_SCALER", s_scaler }, { "NIS_HDR_MODE", s_hdr }, { "NIS_BLOCK_WIDTH", s_bw }, { "NIS_BLOCK_HEIGHT", s_bh },
                                { "NIS_THREAD_GROUP_SIZE", s_tg }, { "NIS_VIEWPORT_SUPPORT", s_vp }, { "NIS_USE_HALF_PRECISION", "0" }, { NULL, NULL } };
    ID3D11ComputeShader *cs = cs_of(src, n, defs, name);
    if (!scaler) ow = iw, oh = ih;
    unsigned char *in = noise(iw, ih, seed);
    ID3D11Texture2D *ti = tex(iw, ih, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_BIND_SHADER_RESOURCE, in, iw * 4);
    ID3D11Texture2D *to = tex(ow, oh, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_BIND_UNORDERED_ACCESS, NULL, 0);
    ID3D11Texture2D *cs_t = coef_tex("NIS_COEF_SCALE"), *cu_t = coef_tex("NIS_COEF_USM");
    if (!cs || !ti || !to || !cs_t || !cu_t) {
        printf("%s: not run\n", name);
        free(in);
        return;
    }
    unsigned cfg[28];
    nis_config(cfg, sharp, iw, ih, ow, oh, hdr, vp);
    ID3D11Buffer *cb = cbuf(cfg, sizeof cfg);
    D3D11_SAMPLER_DESC sd = { D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE_ADDRESS_CLAMP, 0, 1, D3D11_COMPARISON_NEVER, { 0 }, 0, D3D11_FLOAT32_MAX };
    ID3D11SamplerState *smp = NULL;
    ID3D11Device_CreateSamplerState(dev, &sd, &smp);
    ID3D11ShaderResourceView *srv[3] = { srv_of(ti), srv_of(cs_t), srv_of(cu_t) };
    ID3D11UnorderedAccessView *u = uav_of(to);
    ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
    ID3D11DeviceContext_CSSetConstantBuffers(ctx, 0, 1, &cb);
    ID3D11DeviceContext_CSSetSamplers(ctx, 0, 1, &smp);
    ID3D11DeviceContext_CSSetShaderResources(ctx, 0, 3, srv);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &u, NULL);
    ID3D11DeviceContext_Dispatch(ctx, (ow + bw - 1) / bw, (oh + bh - 1) / bh, 1);
    unbind();
    unsigned char *out = malloc(ow * oh * 4);
    if (readback(to, ow, oh, DXGI_FORMAT_R8G8B8A8_UNORM, 4, out)) rgba_image(name, out, ow, oh, 2);
    free(out);
    free(in);
    for (int i = 0; i < 3; i++) ID3D11ShaderResourceView_Release(srv[i]);
    ID3D11UnorderedAccessView_Release(u);
    ID3D11SamplerState_Release(smp);
    ID3D11Buffer_Release(cb);
    ID3D11Texture2D_Release(ti), ID3D11Texture2D_Release(to), ID3D11Texture2D_Release(cs_t), ID3D11Texture2D_Release(cu_t);
    ID3D11ComputeShader_Release(cs);
}

int main(void) {
    static const D3D_FEATURE_LEVEL want = D3D_FEATURE_LEVEL_11_0;
    D3D_FEATURE_LEVEL got = 0;
    HRESULT hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_WARP, NULL, 0, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) { printf("skip D3D11CreateDevice %#lx\n", hr); return 0; }
    printf("feature level %#x\n", got);

    /* thread IDs: 16x8 of 4 uints, groups of 4x2, Dispatch(4, 4, 1) and DispatchIndirect(2, 1, 1) */
    {
        ID3D11ComputeShader *cs = CS(IDS, "ids");
        static unsigned zero[16 * 8 * 4], v[16 * 8 * 4];
        ID3D11Texture2D *t = tex(16, 8, DXGI_FORMAT_R32G32B32A32_UINT, D3D11_BIND_UNORDERED_ACCESS, zero, 16 * 16);
        ID3D11UnorderedAccessView *u = uav_of(t);
        ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
        ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &u, NULL);
        ID3D11DeviceContext_Dispatch(ctx, 4, 4, 1);
        if (readback(t, 16, 8, DXGI_FORMAT_R32G32B32A32_UINT, 16, v)) words("ids", v, 16 * 8 * 4, 16);
        ID3D11Texture2D *t2 = tex(16, 8, DXGI_FORMAT_R32G32B32A32_UINT, D3D11_BIND_UNORDERED_ACCESS, zero, 16 * 16);
        ID3D11UnorderedAccessView *u2 = uav_of(t2);
        ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &u2, NULL);
        unsigned args[4] = { 0xdead, 2, 1, 1 };
        D3D11_BUFFER_DESC bd = { 16, D3D11_USAGE_DEFAULT, 0, 0, D3D11_RESOURCE_MISC_DRAWINDIRECT_ARGS, 0 };
        D3D11_SUBRESOURCE_DATA sd = { args, 0, 0 };
        ID3D11Buffer *ab = NULL;
        if (FAILED(ID3D11Device_CreateBuffer(dev, &bd, &sd, &ab))) printf("CreateBuffer (indirect) failed\n");
        else ID3D11DeviceContext_DispatchIndirect(ctx, ab, 4);
        unbind();
        if (readback(t2, 16, 8, DXGI_FORMAT_R32G32B32A32_UINT, 16, v)) words("indirect", v, 16 * 8 * 4, 16);
        /* nothing bound: runs, writes nowhere */
        ID3D11DeviceContext_Dispatch(ctx, 1, 1, 1);
        printf("dispatch with no UAV: ok\n");
        if (ab) ID3D11Buffer_Release(ab);
        ID3D11UnorderedAccessView_Release(u), ID3D11UnorderedAccessView_Release(u2);
        ID3D11Texture2D_Release(t), ID3D11Texture2D_Release(t2);
        ID3D11ComputeShader_Release(cs);
    }
    /* groupshared and barriers: a scan per group, 64x4 R32_UINT through u1 */
    {
        ID3D11ComputeShader *cs = CS(SCAN, "scan");
        static unsigned v[64 * 4];
        ID3D11Texture2D *t = tex(64, 4, DXGI_FORMAT_R32_UINT, D3D11_BIND_UNORDERED_ACCESS, NULL, 0);
        ID3D11UnorderedAccessView *u = uav_of(t);
        ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
        ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 1, 1, &u, NULL);
        ID3D11DeviceContext_Dispatch(ctx, 1, 4, 1);
        unbind();
        if (readback(t, 64, 4, DXGI_FORMAT_R32_UINT, 4, v)) words("scan", v, 64 * 4, 16);
        ID3D11UnorderedAccessView_Release(u);
        ID3D11Texture2D_Release(t);
        ID3D11ComputeShader_Release(cs);
    }
    /* a UNORM UAV read and written by each thread */
    {
        ID3D11ComputeShader *cs = CS(RMW, "rmw");
        static unsigned char px[16 * 16 * 4];
        for (int i = 0; i < 16 * 16 * 4; i++) px[i] = (i * 37 + 11) & 255;
        ID3D11Texture2D *t = tex(16, 16, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_BIND_UNORDERED_ACCESS, px, 64);
        ID3D11UnorderedAccessView *u = uav_of(t);
        ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
        ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &u, NULL);
        ID3D11DeviceContext_Dispatch(ctx, 16, 16, 1);
        ID3D11DeviceContext_Dispatch(ctx, 16, 8, 1);
        unbind();
        if (readback(t, 16, 16, DXGI_FORMAT_R8G8B8A8_UNORM, 4, px)) rgba_image("rmw", px, 16, 16, 1);
        ID3D11UnorderedAccessView_Release(u);
        ID3D11Texture2D_Release(t);
        ID3D11ComputeShader_Release(cs);
    }
    /* floats: an SRV and constants in, exact operations, R32G32B32A32_FLOAT out */
    {
        ID3D11ComputeShader *cs = CS(FLOATS, "floats");
        static float in[8 * 8 * 4], v[8 * 8 * 4];
        for (int i = 0; i < 8 * 8 * 4; i++) in[i] = ((i * 2654435761u) >> 8 & 0xffff) / 65536.0f;
        ID3D11Texture2D *ti = tex(8, 8, DXGI_FORMAT_R32G32B32A32_FLOAT, D3D11_BIND_SHADER_RESOURCE, in, 8 * 16);
        ID3D11Texture2D *t = tex(8, 8, DXGI_FORMAT_R32G32B32A32_FLOAT, D3D11_BIND_UNORDERED_ACCESS, NULL, 0);
        float k[4] = { 1.5f, 0.375f, 0.625f, 0 };
        ID3D11Buffer *cb = cbuf(k, 16);
        ID3D11ShaderResourceView *s = srv_of(ti);
        ID3D11UnorderedAccessView *u = uav_of(t);
        ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
        ID3D11DeviceContext_CSSetConstantBuffers(ctx, 0, 1, &cb);
        ID3D11DeviceContext_CSSetShaderResources(ctx, 0, 1, &s);
        ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &u, NULL);
        ID3D11DeviceContext_Dispatch(ctx, 1, 1, 1);
        unbind();
        if (readback(t, 8, 8, DXGI_FORMAT_R32G32B32A32_FLOAT, 16, v)) words("floats", (unsigned *)v, 8 * 8 * 4, 16);
        ID3D11ShaderResourceView_Release(s);
        ID3D11UnorderedAccessView_Release(u);
        ID3D11Buffer_Release(cb);
        ID3D11Texture2D_Release(ti), ID3D11Texture2D_Release(t);
        ID3D11ComputeShader_Release(cs);
    }
    /* binding a texture's UAV unbinds the compute shader's SRV of it; binding the SRV then
     * binds NULL */
    {
        ID3D11Texture2D *t = tex(4, 4, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_BIND_UNORDERED_ACCESS | D3D11_BIND_SHADER_RESOURCE, NULL, 0);
        ID3D11ShaderResourceView *s = srv_of(t), *got = NULL;
        ID3D11UnorderedAccessView *u = uav_of(t);
        ID3D11DeviceContext_CSSetShaderResources(ctx, 0, 1, &s);
        ID3D11DeviceContext_CSGetShaderResources(ctx, 0, 1, &got);
        printf("srv bound: %s\n", got ? "yes" : "no");
        if (got) ID3D11ShaderResourceView_Release(got), got = NULL;
        ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &u, NULL);
        ID3D11DeviceContext_CSGetShaderResources(ctx, 0, 1, &got);
        printf("srv after the uav: %s\n", got ? "yes" : "no");
        if (got) ID3D11ShaderResourceView_Release(got), got = NULL;
        ID3D11DeviceContext_CSSetShaderResources(ctx, 0, 1, &s);
        ID3D11DeviceContext_CSGetShaderResources(ctx, 0, 1, &got);
        printf("srv set again: %s\n", got ? "yes" : "no");
        if (got) ID3D11ShaderResourceView_Release(got);
        unbind();
        ID3D11ShaderResourceView_Release(s);
        ID3D11UnorderedAccessView_Release(u);
        ID3D11Texture2D_Release(t);
    }
    /* NVIDIA Image Scaling */
    nis("nis_scaler_64x48_96x72", 1, 64, 48, 96, 72, 0.5f, 0, 128, 0, 1);
    nis("nis_scaler_50x30_133x77", 1, 50, 30, 133, 77, 1.0f, 0, 256, 0, 3);
    nis("nis_scaler_hdr_64x48_80x60", 1, 64, 48, 80, 60, 0.0f, 1, 128, 0, 5);
    nis("nis_scaler_vp_60x40_90x66", 1, 60, 40, 90, 66, 0.3f, 2, 256, 1, 7);
    nis("nis_sharpen_70x50", 0, 70, 50, 0, 0, 0.5f, 0, 256, 0, 9);
    nis("nis_sharpen_vp_33x65", 0, 33, 65, 0, 0, 1.0f, 0, 128, 1, 11);
    printf("done\n");
    return 0;
}
