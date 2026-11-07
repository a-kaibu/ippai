/* Direct3D 11 texels of every DXGI format (feature level 11_0; WARP on the Windows runner): an
 * 8x4 texture of each format made from fixed pseudo-random bytes, its creation and default
 * shader resource view, and every texel as the shader's Load returns it (float formats into
 * an R32G32B32A32_FLOAT target, integer ones into R32G32B32A32_UINT), bit for bit. This covers
 * how each format decodes: UNORM/SNORM/sRGB/float/shared-exponent conversions, the packed and
 * 2x1 formats, BC1-BC7 blocks of every mode, and the channels a format lacks (0, 0, 0, 1).
 * libs: -ld3d11 -ld3dcompiler_47
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <d3d11.h>
#include <d3dcompiler.h>

#define TW 8
#define TH 4
#define PITCH 128

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;

static const char *SRC =
    "Texture2D<float4> tf : register(t0);\n"
    "Texture2D<uint4> tu : register(t0);\n"
    "Texture2D<int4> ti : register(t0);\n"
    "float4 vs(uint id : SV_VertexID) : SV_Position {\n"
    "    float2 p = float2((id << 1) & 2, id & 2);\n"
    "    return float4(p * float2(2, -2) + float2(-1, 1), 0, 1);\n"
    "}\n"
    "float4 psf(float4 p : SV_Position) : SV_Target { return tf.Load(int3(p.xy, 0)); }\n"
    "uint4 psu(float4 p : SV_Position) : SV_Target { return tu.Load(int3(p.xy, 0)); }\n"
    "uint4 psi(float4 p : SV_Position) : SV_Target { return asuint(ti.Load(int3(p.xy, 0))); }\n";

static ID3DBlob *compile(const char *entry, const char *target) {
    ID3DBlob *b = NULL, *err = NULL;
    HRESULT hr = D3DCompile(SRC, strlen(SRC), "probe", NULL, NULL, entry, target, 0, 0, &b, &err);
    if (err) ID3D10Blob_Release(err);
    if (FAILED(hr)) {
        printf("D3DCompile %s failed %#lx\n", entry, hr);
        return NULL;
    }
    return b;
}

static ID3D11PixelShader *ps_of(const char *entry) {
    ID3DBlob *b = compile(entry, "ps_4_0");
    ID3D11PixelShader *p = NULL;
    if (b) ID3D11Device_CreatePixelShader(dev, ID3D10Blob_GetBufferPointer(b), ID3D10Blob_GetBufferSize(b), NULL, &p);
    return p;
}

/* the integer formats (read through Texture2D<uint4> / <int4>) */
static int kind(int f) {
    static const int u[] = { 3, 7, 12, 17, 21, 25, 30, 36, 42, 47, 50, 57, 62 };
    static const int s[] = { 4, 8, 14, 18, 32, 38, 43, 52, 59, 64 };
    for (unsigned i = 0; i < sizeof u / sizeof *u; i++)
        if (u[i] == f) return 1;
    for (unsigned i = 0; i < sizeof s / sizeof *s; i++)
        if (s[i] == f) return 2;
    return 0;
}

static unsigned long long xs;
static unsigned rnd(void) {
    xs ^= xs << 13;
    xs ^= xs >> 7;
    xs ^= xs << 17;
    return (unsigned)(xs >> 16);
}

/* bytes for format f: random, without the float encodings of NaN and infinity (the formats
 * that store 32-bit, 16-bit or 11/10-bit floats) */
static void fill(int f, unsigned char *d) {
    xs = 0x9e3779b97f4a7c15ull ^ (unsigned long long)f * 0x100000001b3ull;
    for (int i = 0; i < PITCH * TH; i++) d[i] = (unsigned char)rnd();
    if (f == 2 || f == 6 || f == 16 || f == 41) {
        for (int i = 0; i < PITCH * TH; i += 4) {
            DWORD v;
            memcpy(&v, d + i, 4);
            if ((v >> 23 & 0xff) == 0xff) v ^= 0x40000000;
            memcpy(d + i, &v, 4);
        }
    } else if (f == 10 || f == 34 || f == 54) {
        for (int i = 0; i < PITCH * TH; i += 2) {
            WORD v;
            memcpy(&v, d + i, 2);
            if ((v >> 10 & 0x1f) == 0x1f) v ^= 0x4000;
            memcpy(d + i, &v, 2);
        }
    } else if (f == 26) {
        for (int i = 0; i < PITCH * TH; i += 4) {
            DWORD v;
            memcpy(&v, d + i, 4);
            if ((v >> 6 & 0x1f) == 0x1f) v ^= 1u << 9;
            if ((v >> 17 & 0x1f) == 0x1f) v ^= 1u << 20;
            if ((v >> 27 & 0x1f) == 0x1f) v ^= 1u << 30;
            memcpy(d + i, &v, 4);
        }
    }
}

int main(void) {
    D3D_FEATURE_LEVEL want = D3D_FEATURE_LEVEL_11_0, got;
    HRESULT hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_WARP, NULL, 0, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) { printf("skip D3D11CreateDevice %#lx\n", hr); return 0; }
    printf("feature level %#x\n", got);

    ID3DBlob *vb = compile("vs", "vs_4_0");
    ID3D11VertexShader *vs = NULL;
    if (!vb || FAILED(ID3D11Device_CreateVertexShader(dev, ID3D10Blob_GetBufferPointer(vb), ID3D10Blob_GetBufferSize(vb), NULL, &vs))) return 1;
    ID3D11PixelShader *ps[3] = { ps_of("psf"), ps_of("psu"), ps_of("psi") };
    if (!ps[0] || !ps[1] || !ps[2]) return 1;

    /* targets: float and uint, and a staging copy to read them */
    ID3D11Texture2D *rt[2], *st[2];
    ID3D11RenderTargetView *rtv[2];
    for (int k = 0; k < 2; k++) {
        D3D11_TEXTURE2D_DESC d = { TW, TH, 1, 1, k ? DXGI_FORMAT_R32G32B32A32_UINT : DXGI_FORMAT_R32G32B32A32_FLOAT, { 1, 0 }, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, 0, 0 };
        if (FAILED(ID3D11Device_CreateTexture2D(dev, &d, NULL, &rt[k]))) return 1;
        if (FAILED(ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)rt[k], NULL, &rtv[k]))) return 1;
        d.Usage = D3D11_USAGE_STAGING;
        d.BindFlags = 0;
        d.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        if (FAILED(ID3D11Device_CreateTexture2D(dev, &d, NULL, &st[k]))) return 1;
    }
    D3D11_VIEWPORT vp = { 0, 0, TW, TH, 0, 1 };
    ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);

    static unsigned char data[PITCH * TH];
    for (int f = 1; f <= 132; f++) {
        fill(f, data);
        D3D11_TEXTURE2D_DESC d = { TW, TH, 1, 1, (DXGI_FORMAT)f, { 1, 0 }, D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, 0, 0 };
        D3D11_SUBRESOURCE_DATA sd = { data, PITCH, 0 };
        ID3D11Texture2D *t = NULL;
        hr = ID3D11Device_CreateTexture2D(dev, &d, &sd, &t);
        if (FAILED(hr)) { printf("format %d: CreateTexture2D %#lx\n", f, hr); continue; }
        ID3D11ShaderResourceView *v = NULL;
        hr = ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)t, NULL, &v);
        if (FAILED(hr)) {
            printf("format %d: CreateShaderResourceView %#lx\n", f, hr);
            ID3D11Texture2D_Release(t);
            continue;
        }
        int k = kind(f), o = k ? 1 : 0;
        static const float zero[4] = { 0 };
        ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv[o], zero);
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv[o], NULL);
        ID3D11DeviceContext_PSSetShader(ctx, ps[k], NULL, 0);
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &v);
        ID3D11DeviceContext_Draw(ctx, 3, 0);
        ID3D11ShaderResourceView *none = NULL;
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &none);
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)st[o], (ID3D11Resource *)rt[o]);
        D3D11_MAPPED_SUBRESOURCE m;
        if (FAILED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)st[o], 0, D3D11_MAP_READ, 0, &m))) {
            printf("format %d: Map failed\n", f);
        } else {
            for (int y = 0; y < TH; y++) {
                const DWORD *p = (const DWORD *)((const char *)m.pData + y * m.RowPitch);
                printf("format %d row %d:", f, y);
                for (int x = 0; x < TW; x++) printf(" %08lx.%08lx.%08lx.%08lx", p[4 * x], p[4 * x + 1], p[4 * x + 2], p[4 * x + 3]);
                printf("\n");
            }
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)st[o], 0);
        }
        ID3D11ShaderResourceView_Release(v);
        ID3D11Texture2D_Release(t);
    }
    printf("done\n");
    return 0;
}
