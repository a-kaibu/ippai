/* Direct3D 11 depth buffers of every depth format, typeless ones too (Unity makes R16_TYPELESS
 * and R32_TYPELESS textures bound for depth and shader input): the texture, its depth-stencil
 * and shader resource views, a clear, then a draw into the left half at depth 0.5 (depth
 * function ALWAYS), and every texel as the shader's Load returns it from the view (float
 * formats: within 2 ulp of Windows', WARP on the runner).
 * libs: -ld3d11 -ld3dcompiler_47
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <d3d11.h>
#include <d3dcompiler.h>

#define TW 8
#define TH 4

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;

static const char *SRC =
    "Texture2D<float4> t : register(t0);\n"
    "float4 vs(uint id : SV_VertexID) : SV_Position {\n"
    "    float2 p = float2((id << 1) & 2, id & 2);\n"
    "    return float4(p * float2(2, -2) + float2(-1, 1), 0.5, 1);\n"
    "}\n"
    "float4 psz(float4 p : SV_Position) : SV_Target { return 0; }\n"
    "float4 psl(float4 p : SV_Position) : SV_Target { return t.Load(int3(p.xy, 0)); }\n";

static ID3DBlob *compile(const char *entry, const char *target) {
    ID3DBlob *b = NULL, *err = NULL;
    HRESULT hr = D3DCompile(SRC, strlen(SRC), "probe", NULL, NULL, entry, target, 0, 0, &b, &err);
    if (err) ID3D10Blob_Release(err);
    return SUCCEEDED(hr) ? b : NULL;
}

static ID3D11PixelShader *ps_of(const char *entry) {
    ID3DBlob *b = compile(entry, "ps_4_0");
    ID3D11PixelShader *p = NULL;
    if (b) ID3D11Device_CreatePixelShader(dev, ID3D10Blob_GetBufferPointer(b), ID3D10Blob_GetBufferSize(b), NULL, &p);
    return p;
}

int main(void) {
    D3D_FEATURE_LEVEL want = D3D_FEATURE_LEVEL_11_0, got;
    HRESULT hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_WARP, NULL, 0, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) { printf("skip D3D11CreateDevice %#lx\n", hr); return 0; }

    ID3DBlob *vb = compile("vs", "vs_4_0");
    ID3D11VertexShader *vs = NULL;
    if (!vb || FAILED(ID3D11Device_CreateVertexShader(dev, ID3D10Blob_GetBufferPointer(vb), ID3D10Blob_GetBufferSize(vb), NULL, &vs))) return 1;
    ID3D11PixelShader *psz = ps_of("psz"), *psl = ps_of("psl");
    if (!psz || !psl) return 1;

    /* a float target to read the views into, and its staging copy */
    ID3D11Texture2D *rt, *st;
    ID3D11RenderTargetView *rtv;
    D3D11_TEXTURE2D_DESC rd = { TW, TH, 1, 1, DXGI_FORMAT_R32_FLOAT, { 1, 0 }, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, 0, 0 };
    if (FAILED(ID3D11Device_CreateTexture2D(dev, &rd, NULL, &rt)) || FAILED(ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)rt, NULL, &rtv))) return 1;
    rd.Usage = D3D11_USAGE_STAGING;
    rd.BindFlags = 0;
    rd.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    if (FAILED(ID3D11Device_CreateTexture2D(dev, &rd, NULL, &st))) return 1;

    /* the read back itself: a cleared target */
    static const float clear[4] = { 0.75f, 0, 0, 0 };
    ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv, clear);
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)st, (ID3D11Resource *)rt);
    D3D11_MAPPED_SUBRESOURCE cm;
    if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)st, 0, D3D11_MAP_READ, 0, &cm))) {
        probe_ulp(0, "cleared_target", ((const float *)cm.pData)[TW * TH / 2 - 1]);
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)st, 0);
    }

    D3D11_DEPTH_STENCIL_DESC dd = { TRUE, D3D11_DEPTH_WRITE_MASK_ALL, D3D11_COMPARISON_ALWAYS, FALSE };
    ID3D11DepthStencilState *always;
    if (FAILED(ID3D11Device_CreateDepthStencilState(dev, &dd, &always))) return 1;
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);

    /* (texture format, depth view format, shader view format; 0: none) */
    static const struct { const char *name; DXGI_FORMAT tex, dsv, srv; } cases[] = {
        { "D16_UNORM", DXGI_FORMAT_D16_UNORM, DXGI_FORMAT_D16_UNORM, 0 },
        { "R16_TYPELESS", DXGI_FORMAT_R16_TYPELESS, DXGI_FORMAT_D16_UNORM, DXGI_FORMAT_R16_UNORM },
        { "D32_FLOAT", DXGI_FORMAT_D32_FLOAT, DXGI_FORMAT_D32_FLOAT, 0 },
        { "R32_TYPELESS", DXGI_FORMAT_R32_TYPELESS, DXGI_FORMAT_D32_FLOAT, DXGI_FORMAT_R32_FLOAT },
        { "D24_UNORM_S8_UINT", DXGI_FORMAT_D24_UNORM_S8_UINT, DXGI_FORMAT_D24_UNORM_S8_UINT, 0 },
        { "R24G8_TYPELESS", DXGI_FORMAT_R24G8_TYPELESS, DXGI_FORMAT_D24_UNORM_S8_UINT, DXGI_FORMAT_R24_UNORM_X8_TYPELESS },
        { "D32_FLOAT_S8X24_UINT", DXGI_FORMAT_D32_FLOAT_S8X24_UINT, DXGI_FORMAT_D32_FLOAT_S8X24_UINT, 0 },
        { "R32G8X24_TYPELESS", DXGI_FORMAT_R32G8X24_TYPELESS, DXGI_FORMAT_D32_FLOAT_S8X24_UINT, DXGI_FORMAT_R32_FLOAT_X8X24_TYPELESS },
        /* not depth formats: refused for depth */
        { "R16_FLOAT", DXGI_FORMAT_R16_FLOAT, DXGI_FORMAT_D16_UNORM, 0 },
        { "R32_FLOAT", DXGI_FORMAT_R32_FLOAT, DXGI_FORMAT_D32_FLOAT, 0 },
    };
    for (unsigned c = 0; c < sizeof cases / sizeof *cases; c++) {
        D3D11_TEXTURE2D_DESC d = { TW, TH, 1, 1, cases[c].tex, { 1, 0 }, D3D11_USAGE_DEFAULT,
                                   D3D11_BIND_DEPTH_STENCIL | (cases[c].srv ? D3D11_BIND_SHADER_RESOURCE : 0), 0, 0 };
        ID3D11Texture2D *t = NULL;
        hr = ID3D11Device_CreateTexture2D(dev, &d, NULL, &t);
        printf("%s: texture %#lx", cases[c].name, hr);
        if (FAILED(hr)) { printf("\n"); continue; }
        D3D11_DEPTH_STENCIL_VIEW_DESC dv = { cases[c].dsv, D3D11_DSV_DIMENSION_TEXTURE2D, 0 };
        ID3D11DepthStencilView *dsv = NULL;
        hr = ID3D11Device_CreateDepthStencilView(dev, (ID3D11Resource *)t, &dv, &dsv);
        printf(", depth view %#lx", hr);
        ID3D11ShaderResourceView *srv = NULL;
        if (cases[c].srv) {
            D3D11_SHADER_RESOURCE_VIEW_DESC sv = { cases[c].srv, D3D11_SRV_DIMENSION_TEXTURE2D };
            sv.Texture2D.MipLevels = 1;
            hr = ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)t, &sv, &srv);
            printf(", shader view %#lx", hr);
        }
        printf("\n");
        if (dsv) {
            /* depth 0.25 everywhere, then 0.5 in the left half */
            ID3D11DeviceContext_ClearDepthStencilView(ctx, dsv, D3D11_CLEAR_DEPTH | D3D11_CLEAR_STENCIL, 0.25f, 0);
            D3D11_VIEWPORT half = { 0, 0, TW / 2, TH, 0, 1 };
            ID3D11DeviceContext_RSSetViewports(ctx, 1, &half);
            ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv, dsv);
            ID3D11DeviceContext_OMSetDepthStencilState(ctx, always, 0);
            ID3D11DeviceContext_PSSetShader(ctx, psz, NULL, 0);
            ID3D11DeviceContext_Draw(ctx, 3, 0);
            ID3D11DeviceContext_OMSetRenderTargets(ctx, 0, NULL, NULL);
        }
        if (dsv && srv) {
            static const float zero[4] = { 0 };
            ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv, zero);
            D3D11_VIEWPORT all = { 0, 0, TW, TH, 0, 1 };
            ID3D11DeviceContext_RSSetViewports(ctx, 1, &all);
            ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv, NULL);
            ID3D11DeviceContext_OMSetDepthStencilState(ctx, NULL, 0);
            ID3D11DeviceContext_PSSetShader(ctx, psl, NULL, 0);
            ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &srv);
            ID3D11DeviceContext_Draw(ctx, 3, 0);
            ID3D11ShaderResourceView *none = NULL;
            ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &none);
            ID3D11DeviceContext_OMSetRenderTargets(ctx, 0, NULL, NULL);
            ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)st, (ID3D11Resource *)rt);
            D3D11_MAPPED_SUBRESOURCE m;
            if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)st, 0, D3D11_MAP_READ, 0, &m))) {
                /* one texel of each half per row (the halves are uniform) */
                for (int y = 0; y < TH; y++) {
                    const float *p = (const float *)((const char *)m.pData + y * m.RowPitch);
                    char l[64];
                    sprintf(l, "%s_y%d_left", cases[c].name, y);
                    probe_ulp(2, l, p[1]);
                    sprintf(l, "%s_y%d_right", cases[c].name, y);
                    probe_ulp(2, l, p[TW - 2]);
                }
                ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)st, 0);
            }
        }
        if (srv) ID3D11ShaderResourceView_Release(srv);
        if (dsv) ID3D11DepthStencilView_Release(dsv);
        ID3D11Texture2D_Release(t);
    }
    printf("done\n");
    return 0;
}
