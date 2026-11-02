/* Direct3D 11 check for vnvm: a swap chain, HLSL compiled at run time (D3DCompile), a spinning
 * colored triangle (vertex buffer, constant buffer updated with Map/DISCARD every frame) over a
 * textured quad (texture from initial data, sampler, alpha blending), cleared every frame.
 * Prints each step and quits after 60 s. With "still" the triangle stays put (tests/gfx.mjs).
 * Build: i686-w64-mingw32-gcc -O2 d3d11test.c -o d3d11test.exe -mwindows -ld3d11 -ld3dcompiler_47 -ldxguid */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <d3dcompiler.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

static LRESULT CALLBACK wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_DESTROY) PostQuitMessage(0);
    return DefWindowProcA(hwnd, msg, wp, lp);
}

#define CHECK(x) do { HRESULT hr_ = (x); printf("%s -> %#lx\n", #x, (unsigned long)hr_); fflush(stdout); if (FAILED(hr_)) return 1; } while (0)

static const char hlsl[] =
    "cbuffer C : register(b0) { float4 rot; float4 tint; };\n"
    "struct V { float4 pos : SV_Position; float4 col : COLOR; float2 uv : TEXCOORD; };\n"
    "Texture2D t : register(t0); SamplerState s : register(s0);\n"
    "V vs_tri(float2 p : POSITION, float4 c : COLOR) {\n"
    "  V o; o.pos = float4(p.x * rot.x - p.y * rot.y, p.x * rot.y + p.y * rot.x, 0.5, 1); o.col = c; o.uv = 0; return o; }\n"
    "float4 ps_tri(V i) : SV_Target { return i.col * tint; }\n"
    "V vs_quad(float2 p : POSITION, float2 uv : TEXCOORD) {\n"
    "  V o; o.pos = float4(p, 0.5, 1); o.col = 1; o.uv = uv; return o; }\n"
    "float4 ps_quad(V i) : SV_Target { return t.Sample(s, i.uv); }\n";

static ID3DBlob *compile(const char *entry, const char *target)
{
    ID3DBlob *code = NULL, *err = NULL;
    HRESULT hr = D3DCompile(hlsl, sizeof(hlsl) - 1, "test", NULL, NULL, entry, target, 0, 0, &code, &err);
    printf("D3DCompile(%s, %s) -> %#lx\n", entry, target, (unsigned long)hr);
    if (err) printf("%s\n", (const char *)ID3D10Blob_GetBufferPointer(err));
    fflush(stdout);
    return SUCCEEDED(hr) ? code : NULL;
}

struct tri { float x, y; DWORD color; };
struct quad { float x, y, u, v; };

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    int still = strstr(cmd, "still") != NULL;
    WNDCLASSA wc = { 0, wndproc, 0, 0, inst, 0, LoadCursor(0, IDC_ARROW), 0, 0, "d3d11test" };
    DXGI_SWAP_CHAIN_DESC sd = { 0 };
    D3D_FEATURE_LEVEL level;
    ID3D11Device *dev;
    ID3D11DeviceContext *ctx;
    IDXGISwapChain *sc;
    ID3D11Texture2D *back, *tex;
    ID3D11RenderTargetView *rtv;
    ID3D11ShaderResourceView *srv;
    ID3D11SamplerState *smp;
    ID3D11BlendState *blend;
    ID3D11Buffer *vb_tri, *vb_quad, *cb;
    ID3D11VertexShader *vs_tri, *vs_quad;
    ID3D11PixelShader *ps_tri, *ps_quad;
    ID3D11InputLayout *il_tri, *il_quad;
    ID3DBlob *b_vs_tri, *b_ps_tri, *b_vs_quad, *b_ps_quad;
    DWORD start = GetTickCount(), frames = 0;
    HWND hwnd;
    MSG msg;
    UINT stride, offset = 0;

    RegisterClassA(&wc);
    hwnd = CreateWindowA("d3d11test", "d3d11test", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0, 656, 519, 0, 0, inst, 0);
    sd.BufferDesc.Width = 640;
    sd.BufferDesc.Height = 480;
    sd.BufferDesc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    sd.SampleDesc.Count = 1;
    sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.BufferCount = 1;
    sd.OutputWindow = hwnd;
    sd.Windowed = TRUE;
    CHECK(D3D11CreateDeviceAndSwapChain(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, NULL, 0, D3D11_SDK_VERSION, &sd, &sc, &dev, &level, &ctx));
    printf("feature level %#x\n", level);
    CHECK(IDXGISwapChain_GetBuffer(sc, 0, &IID_ID3D11Texture2D, (void **)&back));
    CHECK(ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)back, NULL, &rtv));

    if (!(b_vs_tri = compile("vs_tri", "vs_4_0")) || !(b_ps_tri = compile("ps_tri", "ps_4_0")) ||
        !(b_vs_quad = compile("vs_quad", "vs_4_0")) || !(b_ps_quad = compile("ps_quad", "ps_4_0")))
        return 1;
    CHECK(ID3D11Device_CreateVertexShader(dev, ID3D10Blob_GetBufferPointer(b_vs_tri), ID3D10Blob_GetBufferSize(b_vs_tri), NULL, &vs_tri));
    CHECK(ID3D11Device_CreatePixelShader(dev, ID3D10Blob_GetBufferPointer(b_ps_tri), ID3D10Blob_GetBufferSize(b_ps_tri), NULL, &ps_tri));
    CHECK(ID3D11Device_CreateVertexShader(dev, ID3D10Blob_GetBufferPointer(b_vs_quad), ID3D10Blob_GetBufferSize(b_vs_quad), NULL, &vs_quad));
    CHECK(ID3D11Device_CreatePixelShader(dev, ID3D10Blob_GetBufferPointer(b_ps_quad), ID3D10Blob_GetBufferSize(b_ps_quad), NULL, &ps_quad));
    {
        D3D11_INPUT_ELEMENT_DESC e_tri[] = {
            { "POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0 },
            { "COLOR", 0, DXGI_FORMAT_R8G8B8A8_UNORM, 0, 8, D3D11_INPUT_PER_VERTEX_DATA, 0 },
        };
        D3D11_INPUT_ELEMENT_DESC e_quad[] = {
            { "POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0 },
            { "TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 8, D3D11_INPUT_PER_VERTEX_DATA, 0 },
        };
        CHECK(ID3D11Device_CreateInputLayout(dev, e_tri, 2, ID3D10Blob_GetBufferPointer(b_vs_tri), ID3D10Blob_GetBufferSize(b_vs_tri), &il_tri));
        CHECK(ID3D11Device_CreateInputLayout(dev, e_quad, 2, ID3D10Blob_GetBufferPointer(b_vs_quad), ID3D10Blob_GetBufferSize(b_vs_quad), &il_quad));
    }
    {
        /* RGBA bytes: red, green, blue */
        static const struct tri t[] = { { 0, 0.7f, 0xff0000ff }, { 0.6f, -0.5f, 0xff00ff00 }, { -0.6f, -0.5f, 0xffff0000 } };
        static const struct quad q[] = { { -0.9f, 0.9f, 0, 0 }, { 0.9f, 0.9f, 1, 0 }, { -0.9f, -0.9f, 0, 1 }, { 0.9f, -0.9f, 1, 1 } };
        D3D11_BUFFER_DESC bd = { sizeof(t), D3D11_USAGE_IMMUTABLE, D3D11_BIND_VERTEX_BUFFER, 0, 0, 0 };
        D3D11_SUBRESOURCE_DATA init = { t, 0, 0 };
        CHECK(ID3D11Device_CreateBuffer(dev, &bd, &init, &vb_tri));
        bd.ByteWidth = sizeof(q);
        init.pSysMem = q;
        CHECK(ID3D11Device_CreateBuffer(dev, &bd, &init, &vb_quad));
        bd.ByteWidth = 32;
        bd.Usage = D3D11_USAGE_DYNAMIC;
        bd.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
        bd.CPUAccessFlags = D3D11_CPU_ACCESS_WRITE;
        CHECK(ID3D11Device_CreateBuffer(dev, &bd, NULL, &cb));
    }
    {
        /* 64x64 checkerboard, half transparent dark blue / opaque yellow (RGBA) */
        static DWORD px[64 * 64];
        D3D11_TEXTURE2D_DESC td = { 64, 64, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, { 1, 0 }, D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, 0, 0 };
        D3D11_SUBRESOURCE_DATA init = { px, 64 * 4, 0 };
        D3D11_SAMPLER_DESC ss = { D3D11_FILTER_MIN_MAG_MIP_POINT, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE_ADDRESS_CLAMP,
                                  D3D11_TEXTURE_ADDRESS_CLAMP, 0, 1, D3D11_COMPARISON_NEVER, { 0 }, 0, D3D11_FLOAT32_MAX };
        D3D11_BLEND_DESC bl = { 0 };
        int x, y;
        for (y = 0; y < 64; y++)
            for (x = 0; x < 64; x++) px[y * 64 + x] = ((x / 8 + y / 8) & 1) ? 0xff00ffff : 0x80800000;
        CHECK(ID3D11Device_CreateTexture2D(dev, &td, &init, &tex));
        CHECK(ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)tex, NULL, &srv));
        CHECK(ID3D11Device_CreateSamplerState(dev, &ss, &smp));
        bl.RenderTarget[0].BlendEnable = TRUE;
        bl.RenderTarget[0].SrcBlend = D3D11_BLEND_SRC_ALPHA;
        bl.RenderTarget[0].DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
        bl.RenderTarget[0].BlendOp = D3D11_BLEND_OP_ADD;
        bl.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_ONE;
        bl.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_ZERO;
        bl.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
        bl.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL;
        CHECK(ID3D11Device_CreateBlendState(dev, &bl, &blend));
    }

    while (GetTickCount() - start < 60000)
    {
        static const float clear[4] = { 0.1f, 0.3f, 0.1f, 1 };
        D3D11_VIEWPORT vp = { 0, 0, 640, 480, 0, 1 };
        D3D11_MAPPED_SUBRESOURCE m;
        float a = still ? 0 : (GetTickCount() - start) / 1000.0f, *f;
        while (PeekMessageA(&msg, 0, 0, 0, PM_REMOVE))
        {
            if (msg.message == WM_QUIT) return 0;
            DispatchMessageA(&msg);
        }
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv, NULL);
        ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);
        ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv, clear);
        ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        /* quad */
        stride = sizeof(struct quad);
        ID3D11DeviceContext_IASetInputLayout(ctx, il_quad);
        ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &vb_quad, &stride, &offset);
        ID3D11DeviceContext_VSSetShader(ctx, vs_quad, NULL, 0);
        ID3D11DeviceContext_PSSetShader(ctx, ps_quad, NULL, 0);
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &srv);
        ID3D11DeviceContext_PSSetSamplers(ctx, 0, 1, &smp);
        ID3D11DeviceContext_OMSetBlendState(ctx, blend, NULL, 0xffffffff);
        ID3D11DeviceContext_Draw(ctx, 4, 0);
        /* triangle */
        if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)cb, 0, D3D11_MAP_WRITE_DISCARD, 0, &m)))
        {
            f = m.pData;
            f[0] = cosf(a); f[1] = sinf(a); f[2] = f[3] = 0;
            f[4] = f[5] = f[6] = f[7] = 1;
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)cb, 0);
        }
        stride = sizeof(struct tri);
        ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ID3D11DeviceContext_IASetInputLayout(ctx, il_tri);
        ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &vb_tri, &stride, &offset);
        ID3D11DeviceContext_VSSetShader(ctx, vs_tri, NULL, 0);
        ID3D11DeviceContext_VSSetConstantBuffers(ctx, 0, 1, &cb);
        ID3D11DeviceContext_PSSetShader(ctx, ps_tri, NULL, 0);
        ID3D11DeviceContext_PSSetConstantBuffers(ctx, 0, 1, &cb);
        ID3D11DeviceContext_OMSetBlendState(ctx, NULL, NULL, 0xffffffff);
        ID3D11DeviceContext_Draw(ctx, 3, 0);
        IDXGISwapChain_Present(sc, 1, 0);
        if (++frames % 60 == 0) { printf("frame %lu\n", frames); fflush(stdout); }
    }
    printf("done, %lu frames\n", frames);
    return 0;
}
