/* Direct3D 10 check for ippai: d3d11test drawn through d3d10.dll (the same picture): a swap chain
 * (D3D10CreateDeviceAndSwapChain), HLSL compiled with D3D10CompileShader, a spinning colored
 * triangle (vertex buffer, constant buffer written with ID3D10Buffer::Map/DISCARD every frame)
 * over a textured quad (texture from initial data, sampler, D3D10_BLEND_DESC alpha blending),
 * integer viewports, GetDesc and ID3D10Device1. Prints each step and quits after 60 s. With
 * "still" the triangle stays put (tests/gfx.mjs).
 * Build: i686-w64-mingw32-gcc -O2 d3d10test.c -o d3d10test.exe -mwindows -ld3d10 -ldxguid */
#define COBJMACROS
#include <windows.h>
#include <d3d10_1.h>
#include <d3d10.h>
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

static ID3D10Blob *compile(const char *entry, const char *target)
{
    ID3D10Blob *code = NULL, *err = NULL;
    HRESULT hr = D3D10CompileShader(hlsl, sizeof(hlsl) - 1, "test", NULL, NULL, entry, target, 0, &code, &err);
    printf("D3D10CompileShader(%s, %s) -> %#lx\n", entry, target, (unsigned long)hr);
    if (err) printf("%s\n", (const char *)ID3D10Blob_GetBufferPointer(err));
    fflush(stdout);
    return SUCCEEDED(hr) ? code : NULL;
}

struct tri { float x, y; DWORD color; };
struct quad { float x, y, u, v; };

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    int still = strstr(cmd, "still") != NULL;
    WNDCLASSA wc = { 0, wndproc, 0, 0, inst, 0, LoadCursor(0, IDC_ARROW), 0, 0, "d3d10test" };
    DXGI_SWAP_CHAIN_DESC sd = { 0 };
    ID3D10Device *dev;
    ID3D10Device1 *dev1;
    IDXGISwapChain *sc;
    ID3D10Texture2D *back, *tex;
    ID3D10RenderTargetView *rtv;
    ID3D10ShaderResourceView *srv;
    ID3D10SamplerState *smp;
    ID3D10BlendState *blend;
    ID3D10Buffer *vb_tri, *vb_quad, *cb;
    ID3D10VertexShader *vs_tri, *vs_quad;
    ID3D10PixelShader *ps_tri, *ps_quad;
    ID3D10InputLayout *il_tri, *il_quad;
    ID3D10Blob *b_vs_tri, *b_ps_tri, *b_vs_quad, *b_ps_quad;
    DWORD start = GetTickCount(), frames = 0;
    HWND hwnd;
    MSG msg;
    UINT stride, offset = 0;

    RegisterClassA(&wc);
    hwnd = CreateWindowA("d3d10test", "d3d10test", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0, 656, 519, 0, 0, inst, 0);
    sd.BufferDesc.Width = 640;
    sd.BufferDesc.Height = 480;
    sd.BufferDesc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    sd.SampleDesc.Count = 1;
    sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.BufferCount = 1;
    sd.OutputWindow = hwnd;
    sd.Windowed = TRUE;
    CHECK(D3D10CreateDeviceAndSwapChain(NULL, D3D10_DRIVER_TYPE_HARDWARE, NULL, 0, D3D10_SDK_VERSION, &sd, &sc, &dev));
    CHECK(ID3D10Device_QueryInterface(dev, &IID_ID3D10Device1, (void **)&dev1));
    printf("feature level %#x\n", ID3D10Device1_GetFeatureLevel(dev1));
    CHECK(IDXGISwapChain_GetBuffer(sc, 0, &IID_ID3D10Texture2D, (void **)&back));
    CHECK(ID3D10Device_CreateRenderTargetView(dev, (ID3D10Resource *)back, NULL, &rtv));

    if (!(b_vs_tri = compile("vs_tri", "vs_4_0")) || !(b_ps_tri = compile("ps_tri", "ps_4_0")) ||
        !(b_vs_quad = compile("vs_quad", "vs_4_0")) || !(b_ps_quad = compile("ps_quad", "ps_4_0")))
        return 1;
    CHECK(ID3D10Device_CreateVertexShader(dev, ID3D10Blob_GetBufferPointer(b_vs_tri), ID3D10Blob_GetBufferSize(b_vs_tri), &vs_tri));
    CHECK(ID3D10Device_CreatePixelShader(dev, ID3D10Blob_GetBufferPointer(b_ps_tri), ID3D10Blob_GetBufferSize(b_ps_tri), &ps_tri));
    CHECK(ID3D10Device_CreateVertexShader(dev, ID3D10Blob_GetBufferPointer(b_vs_quad), ID3D10Blob_GetBufferSize(b_vs_quad), &vs_quad));
    CHECK(ID3D10Device_CreatePixelShader(dev, ID3D10Blob_GetBufferPointer(b_ps_quad), ID3D10Blob_GetBufferSize(b_ps_quad), &ps_quad));
    {
        D3D10_INPUT_ELEMENT_DESC e_tri[] = {
            { "POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D10_INPUT_PER_VERTEX_DATA, 0 },
            { "COLOR", 0, DXGI_FORMAT_R8G8B8A8_UNORM, 0, 8, D3D10_INPUT_PER_VERTEX_DATA, 0 },
        };
        D3D10_INPUT_ELEMENT_DESC e_quad[] = {
            { "POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D10_INPUT_PER_VERTEX_DATA, 0 },
            { "TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 8, D3D10_INPUT_PER_VERTEX_DATA, 0 },
        };
        CHECK(ID3D10Device_CreateInputLayout(dev, e_tri, 2, ID3D10Blob_GetBufferPointer(b_vs_tri), ID3D10Blob_GetBufferSize(b_vs_tri), &il_tri));
        CHECK(ID3D10Device_CreateInputLayout(dev, e_quad, 2, ID3D10Blob_GetBufferPointer(b_vs_quad), ID3D10Blob_GetBufferSize(b_vs_quad), &il_quad));
    }
    {
        /* RGBA bytes: red, green, blue */
        static const struct tri t[] = { { 0, 0.7f, 0xff0000ff }, { 0.6f, -0.5f, 0xff00ff00 }, { -0.6f, -0.5f, 0xffff0000 } };
        static const struct quad q[] = { { -0.9f, 0.9f, 0, 0 }, { 0.9f, 0.9f, 1, 0 }, { -0.9f, -0.9f, 0, 1 }, { 0.9f, -0.9f, 1, 1 } };
        D3D10_BUFFER_DESC bd = { sizeof(t), D3D10_USAGE_IMMUTABLE, D3D10_BIND_VERTEX_BUFFER, 0, 0 };
        D3D10_SUBRESOURCE_DATA init = { t, 0, 0 };
        CHECK(ID3D10Device_CreateBuffer(dev, &bd, &init, &vb_tri));
        bd.ByteWidth = sizeof(q);
        init.pSysMem = q;
        CHECK(ID3D10Device_CreateBuffer(dev, &bd, &init, &vb_quad));
        bd.ByteWidth = 32;
        bd.Usage = D3D10_USAGE_DYNAMIC;
        bd.BindFlags = D3D10_BIND_CONSTANT_BUFFER;
        bd.CPUAccessFlags = D3D10_CPU_ACCESS_WRITE;
        CHECK(ID3D10Device_CreateBuffer(dev, &bd, NULL, &cb));
        memset(&bd, 0xcc, sizeof(bd));
        ID3D10Buffer_GetDesc(cb, &bd);
        printf("buffer desc %u %u %#x %#x %#x\n", bd.ByteWidth, bd.Usage, bd.BindFlags, bd.CPUAccessFlags, bd.MiscFlags);
    }
    {
        /* 64x64 checkerboard, half transparent dark blue / opaque yellow (RGBA) */
        static DWORD px[64 * 64];
        D3D10_TEXTURE2D_DESC td = { 64, 64, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, { 1, 0 }, D3D10_USAGE_DEFAULT, D3D10_BIND_SHADER_RESOURCE, 0, 0 };
        D3D10_SUBRESOURCE_DATA init = { px, 64 * 4, 0 };
        D3D10_SAMPLER_DESC ss = { D3D10_FILTER_MIN_MAG_MIP_POINT, D3D10_TEXTURE_ADDRESS_CLAMP, D3D10_TEXTURE_ADDRESS_CLAMP,
                                  D3D10_TEXTURE_ADDRESS_CLAMP, 0, 1, D3D10_COMPARISON_NEVER, { 0 }, 0, 3.402823466e+38f };
        D3D10_BLEND_DESC bl = { 0 }, got;
        int x, y;
        for (y = 0; y < 64; y++)
            for (x = 0; x < 64; x++) px[y * 64 + x] = ((x / 8 + y / 8) & 1) ? 0xff00ffff : 0x80800000;
        CHECK(ID3D10Device_CreateTexture2D(dev, &td, &init, &tex));
        CHECK(ID3D10Device_CreateShaderResourceView(dev, (ID3D10Resource *)tex, NULL, &srv));
        CHECK(ID3D10Device_CreateSamplerState(dev, &ss, &smp));
        bl.BlendEnable[0] = TRUE;
        bl.SrcBlend = D3D10_BLEND_SRC_ALPHA;
        bl.DestBlend = D3D10_BLEND_INV_SRC_ALPHA;
        bl.BlendOp = D3D10_BLEND_OP_ADD;
        bl.SrcBlendAlpha = D3D10_BLEND_ONE;
        bl.DestBlendAlpha = D3D10_BLEND_ZERO;
        bl.BlendOpAlpha = D3D10_BLEND_OP_ADD;
        bl.RenderTargetWriteMask[0] = D3D10_COLOR_WRITE_ENABLE_ALL;
        CHECK(ID3D10Device_CreateBlendState(dev, &bl, &blend));
        ID3D10BlendState_GetDesc(blend, &got);
        printf("blend desc %s\n", memcmp(&bl, &got, sizeof(bl)) ? "differs" : "same");
    }

    while (GetTickCount() - start < 60000)
    {
        static const float clear[4] = { 0.1f, 0.3f, 0.1f, 1 };
        D3D10_VIEWPORT vp = { 0, 0, 640, 480, 0, 1 };
        void *m;
        float a = still ? 0 : (GetTickCount() - start) / 1000.0f, *f;
        while (PeekMessageA(&msg, 0, 0, 0, PM_REMOVE))
        {
            if (msg.message == WM_QUIT) return 0;
            DispatchMessageA(&msg);
        }
        ID3D10Device_OMSetRenderTargets(dev, 1, &rtv, NULL);
        ID3D10Device_RSSetViewports(dev, 1, &vp);
        ID3D10Device_ClearRenderTargetView(dev, rtv, clear);
        ID3D10Device_IASetPrimitiveTopology(dev, D3D10_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        /* quad */
        stride = sizeof(struct quad);
        ID3D10Device_IASetInputLayout(dev, il_quad);
        ID3D10Device_IASetVertexBuffers(dev, 0, 1, &vb_quad, &stride, &offset);
        ID3D10Device_VSSetShader(dev, vs_quad);
        ID3D10Device_PSSetShader(dev, ps_quad);
        ID3D10Device_PSSetShaderResources(dev, 0, 1, &srv);
        ID3D10Device_PSSetSamplers(dev, 0, 1, &smp);
        ID3D10Device_OMSetBlendState(dev, blend, NULL, 0xffffffff);
        ID3D10Device_Draw(dev, 4, 0);
        /* triangle */
        if (SUCCEEDED(ID3D10Buffer_Map(cb, D3D10_MAP_WRITE_DISCARD, 0, &m)))
        {
            f = m;
            f[0] = cosf(a); f[1] = sinf(a); f[2] = f[3] = 0;
            f[4] = f[5] = f[6] = f[7] = 1;
            ID3D10Buffer_Unmap(cb);
        }
        stride = sizeof(struct tri);
        ID3D10Device_IASetPrimitiveTopology(dev, D3D10_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ID3D10Device_IASetInputLayout(dev, il_tri);
        ID3D10Device_IASetVertexBuffers(dev, 0, 1, &vb_tri, &stride, &offset);
        ID3D10Device_VSSetShader(dev, vs_tri);
        ID3D10Device_VSSetConstantBuffers(dev, 0, 1, &cb);
        ID3D10Device_PSSetShader(dev, ps_tri);
        ID3D10Device_PSSetConstantBuffers(dev, 0, 1, &cb);
        ID3D10Device_OMSetBlendState(dev, NULL, NULL, 0xffffffff);
        ID3D10Device_Draw(dev, 3, 0);
        IDXGISwapChain_Present(sc, 1, 0);
        if (++frames % 60 == 0) { printf("frame %lu\n", frames); fflush(stdout); }
    }
    printf("done, %lu frames\n", frames);
    return 0;
}
