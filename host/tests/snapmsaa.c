/* Save states (docs/savestate.md) of Direct3D 11 4x targets: four of them (UNORM, sRGB, half
 * float, UINT) drawn once with slanted edges (samples that differ), then read back sample by
 * sample through a Texture2DMS, and resolved, every step: a state taken between the steps must
 * give the same samples after it, bit for bit. tests/savestate-browser.mjs (WebGPU).
 * Build: i686-w64-mingw32-gcc -O2 snapmsaa.c -ld3d11 -ld3dcompiler_47 */
#define COBJMACROS
#define INITGUID
#include <stdio.h>
#include <string.h>
#include <windows.h>
#include <d3d11.h>
#include <d3dcompiler.h>

#define W 32
#define H 32
#define STEPS 40

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;

static const char *SRC =
    "cbuffer c : register(b0) { int k; int3 pad; }\n"
    "Texture2DMS<float4> tf : register(t0);\n"
    "Texture2DMS<uint4> tu : register(t1);\n"
    "struct V { float4 p : SV_Position; float4 c : COLOR; };\n"
    "V vs(float2 p : POSITION, float4 c : COLOR) { V o; o.p = float4(p.x / 16 - 1, 1 - p.y / 16, 0.5, 1); o.c = c; return o; }\n"
    "float4 color(V i) : SV_Target { return i.c; }\n"
    "uint4 ucolor(V i) : SV_Target { return uint4(i.c * 255 + 0.5); }\n"
    "float4 samplef(V i) : SV_Target { return tf.Load(int2(i.p.xy), k); }\n"
    "uint4 sampleu(V i) : SV_Target { return tu.Load(int2(i.p.xy), k); }\n";

static ID3DBlob *compile(const char *entry, const char *target) {
    ID3DBlob *b = NULL, *err = NULL;
    if (FAILED(D3DCompile(SRC, strlen(SRC), "snapmsaa", NULL, NULL, entry, target, 0, 0, &b, &err))) {
        printf("D3DCompile %s: %s\n", entry, err ? (char *)ID3D10Blob_GetBufferPointer(err) : "");
        exit(1);
    }
    return b;
}
static ID3D11PixelShader *ps(const char *entry) {
    ID3DBlob *b = compile(entry, "ps_4_1");
    ID3D11PixelShader *s = NULL;
    ID3D11Device_CreatePixelShader(dev, ID3D10Blob_GetBufferPointer(b), ID3D10Blob_GetBufferSize(b), NULL, &s);
    return s;
}

struct v { float x, y; float c[4]; };

static unsigned fnv(unsigned h, const unsigned char *p, int n) {
    for (int i = 0; i < n; i++) h = (h ^ p[i]) * 0x01000193u;
    return h;
}

static const struct { DXGI_FORMAT f, single; int bytes, uint; const char *name; } F[] = {
    { DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, 4, 0, "unorm" },
    { DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, 4, 0, "srgb" },
    { DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_R16G16B16A16_FLOAT, 8, 0, "half" },
    { DXGI_FORMAT_R8G8B8A8_UINT, DXGI_FORMAT_R8G8B8A8_UINT, 4, 1, "uint" },
};
#define NF 4

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    static const D3D_FEATURE_LEVEL want = D3D_FEATURE_LEVEL_11_0;
    if (FAILED(D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, &want, 1, D3D11_SDK_VERSION, &dev, NULL, &ctx))) {
        puts("no device");
        return 1;
    }
    ID3DBlob *vb = compile("vs", "vs_4_1");
    ID3D11VertexShader *vs = NULL;
    ID3D11Device_CreateVertexShader(dev, ID3D10Blob_GetBufferPointer(vb), ID3D10Blob_GetBufferSize(vb), NULL, &vs);
    D3D11_INPUT_ELEMENT_DESC el[2] = { { "POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0 },
                                       { "COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 8, D3D11_INPUT_PER_VERTEX_DATA, 0 } };
    ID3D11InputLayout *layout = NULL;
    ID3D11Device_CreateInputLayout(dev, el, 2, ID3D10Blob_GetBufferPointer(vb), ID3D10Blob_GetBufferSize(vb), &layout);
    ID3D11PixelShader *pcolor = ps("color"), *pucolor = ps("ucolor"), *psf = ps("samplef"), *psu = ps("sampleu");
    D3D11_BUFFER_DESC cbd = { 16, D3D11_USAGE_DEFAULT, D3D11_BIND_CONSTANT_BUFFER, 0, 0, 0 };
    ID3D11Buffer *cb = NULL;
    ID3D11Device_CreateBuffer(dev, &cbd, NULL, &cb);
    /* slanted triangles in two colors over a cleared background: edge pixels' samples differ */
    struct v tri[12] = { { 1, 2, { 1, 0.25f, 0, 1 } }, { 31, 9, { 0.5f, 1, 0.75f, 1 } }, { 7, 30, { 0, 0.5f, 1, 0.5f } },
                        { 30, 31, { 0.9f, 0.1f, 0.6f, 1 } }, { 13, 3, { 0.2f, 0.8f, 0.3f, 1 } }, { 31, 20, { 1, 1, 1, 0.25f } },
                        /* (and the whole target, for reading it back) */
                        { 0, 0 }, { 32, 0 }, { 0, 32 }, { 32, 0 }, { 32, 32 }, { 0, 32 } };
    D3D11_BUFFER_DESC bd = { sizeof tri, D3D11_USAGE_IMMUTABLE, D3D11_BIND_VERTEX_BUFFER, 0, 0, 0 };
    D3D11_SUBRESOURCE_DATA sd = { tri, 0, 0 };
    ID3D11Buffer *vbuf = NULL;
    ID3D11Device_CreateBuffer(dev, &bd, &sd, &vbuf);
    UINT stride = sizeof tri[0], off = 0;
    ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &vbuf, &stride, &off);
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_PSSetConstantBuffers(ctx, 0, 1, &cb);
    D3D11_VIEWPORT vp = { 0, 0, W, H, 0, 1 };
    ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);

    ID3D11Texture2D *ms[NF], *one[NF], *stage[NF];
    ID3D11RenderTargetView *msv[NF], *onev[NF];
    ID3D11ShaderResourceView *srv[NF];
    for (int i = 0; i < NF; i++) {
        D3D11_TEXTURE2D_DESC d = { W, H, 1, 1, F[i].f, { 4, 0 }, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0, 0 };
        if (FAILED(ID3D11Device_CreateTexture2D(dev, &d, NULL, &ms[i]))) { printf("4x %s failed\n", F[i].name); return 1; }
        ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)ms[i], NULL, &msv[i]);
        ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)ms[i], NULL, &srv[i]);
        d.SampleDesc.Count = 1;
        d.Format = F[i].single;
        d.BindFlags = D3D11_BIND_RENDER_TARGET;
        ID3D11Device_CreateTexture2D(dev, &d, NULL, &one[i]);
        ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)one[i], NULL, &onev[i]);
        d.Usage = D3D11_USAGE_STAGING; d.BindFlags = 0; d.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        ID3D11Device_CreateTexture2D(dev, &d, NULL, &stage[i]);
        /* drawn once: never again */
        float clear[4] = { 0.125f * (i + 1), 0, 0.25f, 1 };
        ID3D11DeviceContext_ClearRenderTargetView(ctx, msv[i], clear);
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &msv[i], NULL);
        ID3D11DeviceContext_PSSetShader(ctx, F[i].uint ? pucolor : pcolor, NULL, 0);
        ID3D11DeviceContext_Draw(ctx, 6, 0);
    }
    unsigned first = 0;
    int differ = 0;
    for (int k = 0; k < STEPS; k++) {
        unsigned hs[NF][5];
        for (int i = 0; i < NF; i++) {
            ID3D11ShaderResourceView *none[2] = { NULL, NULL };
            for (int s = 0; s < 5; s++) {
                ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 2, none);
                if (s < 4) {
                    int c[4] = { s };
                    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)cb, 0, NULL, c, 0, 0);
                    ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &onev[i], NULL);
                    ID3D11DeviceContext_PSSetShaderResources(ctx, F[i].uint, 1, &srv[i]);
                    ID3D11DeviceContext_PSSetShader(ctx, F[i].uint ? psu : psf, NULL, 0);
                    ID3D11DeviceContext_Draw(ctx, 6, 6);
                } else if (!F[i].uint) {
                    ID3D11DeviceContext_ResolveSubresource(ctx, (ID3D11Resource *)one[i], 0, (ID3D11Resource *)ms[i], 0, F[i].single);
                }
                ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)stage[i], (ID3D11Resource *)one[i]);
                D3D11_MAPPED_SUBRESOURCE m;
                if (FAILED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)stage[i], 0, D3D11_MAP_READ, 0, &m))) { puts("Map failed"); return 1; }
                unsigned h = 0x811c9dc5u;
                for (int y = 0; y < H; y++) h = fnv(h, (const unsigned char *)m.pData + y * m.RowPitch, W * F[i].bytes);
                ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)stage[i], 0);
                hs[i][s] = h;
            }
            if (hs[i][0] != hs[i][1] || hs[i][1] != hs[i][2] || hs[i][2] != hs[i][3]) differ |= 1 << i;
        }
        printf("step %d:", k);
        for (int i = 0; i < NF; i++) printf(" %s %08x %08x %08x %08x %08x", F[i].name, hs[i][0], hs[i][1], hs[i][2], hs[i][3], hs[i][4]);
        printf("\n");
        if (k == 0) first = fnv(0, (const unsigned char *)hs, sizeof hs);
        else if (fnv(0, (const unsigned char *)hs, sizeof hs) != first) printf("changed at step %d\n", k);
        Sleep(60);
    }
    printf("samples differ: %s\nend\n", differ == (1 << NF) - 1 ? "all" : "not all");
    return 0;
}
