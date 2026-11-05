/* Direct3D 11: newer engines (and Direct2D's back end), at 1280x720. Shaders are shader model 4
 * bytecode from tests/bench/dxbc.py (d3d11_shaders.h): a quad placed by a constant buffer,
 * texture x color, a crossfade and a blur. Each case reports ms per frame.
 *
 *   vn               background, 3 characters, message window, typing text (GDI into a DIB, then
 *                    UpdateSubresource), each quad with its own constant buffer update (Map DISCARD)
 *   fade             crossfade of two full-screen pictures (two textures, a constant)
 *   sprites_batched  2000 particles in one dynamic vertex buffer (Map DISCARD), one Draw
 *   sprites_calls    2000 particles, one Map of the constant buffer and one Draw each
 *   rt               the scene into a render target, 4 blur passes between two render targets,
 *                    then to the back buffer
 *   upload           a 1280x720 texture rewritten every frame by UpdateSubresource (movies)
 *   upload_map       the same through a dynamic texture (Map WRITE_DISCARD)
 *
 * Build: i686-w64-mingw32-gcc -O2 d3d11.c -o d3d11.exe -mwindows -ld3d11 -ldxguid -lgdi32 */
#define COBJMACROS
#include "bench.h"
#include <d3d11.h>
#include "d3d11_shaders.h"

typedef struct { float x, y, u, v, r, g, b, a; } V;
#define SPRITES 2000

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;
static IDXGISwapChain *sc;
static ID3D11RenderTargetView *backbuf, *rtv[2];
static ID3D11ShaderResourceView *t_bg, *t_bg2, *t_chr[3], *t_win, *t_text, *t_spr, *t_movie, *t_movie2, *srv[2];
static ID3D11Texture2D *text_tex, *movie_tex, *movie2_tex;
static ID3D11Buffer *quad_vb, *sprite_vb, *cb, *pcb; /* (the vertex and the pixel shader's constants) */
static ID3D11VertexShader *vs;
static ID3D11PixelShader *ps_sprite_, *ps_fade_, *ps_blur_;
static ID3D11InputLayout *layout;
static ID3D11BlendState *alpha, *opaque;
static ID3D11SamplerState *linear;
static HDC text_dc;
static unsigned *text_bits, *movie;
static const UINT stride = sizeof(V), zero = 0;

static ID3D11ShaderResourceView *texture(int w, int h, const unsigned *pixels, D3D11_USAGE usage, UINT bind, ID3D11Texture2D **out)
{
    D3D11_TEXTURE2D_DESC d = { w, h, 1, 1, DXGI_FORMAT_B8G8R8A8_UNORM, { 1, 0 }, usage, D3D11_BIND_SHADER_RESOURCE | bind,
                               usage == D3D11_USAGE_DYNAMIC ? D3D11_CPU_ACCESS_WRITE : 0, 0 };
    D3D11_SUBRESOURCE_DATA init = { pixels, w * 4, 0 };
    ID3D11Texture2D *t;
    ID3D11ShaderResourceView *v;
    BCHECK(ID3D11Device_CreateTexture2D(dev, &d, pixels ? &init : NULL, &t));
    BCHECK(ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)t, NULL, &v));
    if (out) *out = t;
    return v;
}

static ID3D11Buffer *buffer(UINT size, UINT bind, D3D11_USAGE usage, const void *data)
{
    D3D11_BUFFER_DESC d = { size, usage, bind, usage == D3D11_USAGE_DYNAMIC ? D3D11_CPU_ACCESS_WRITE : 0, 0, 0 };
    D3D11_SUBRESOURCE_DATA init = { data, 0, 0 };
    ID3D11Buffer *b;
    BCHECK(ID3D11Device_CreateBuffer(dev, &d, data ? &init : NULL, &b));
    return b;
}

static void constants(ID3D11Buffer *b, float x, float y, float z, float w)
{
    D3D11_MAPPED_SUBRESOURCE m;
    if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)b, 0, D3D11_MAP_WRITE_DISCARD, 0, &m)))
    {
        float *f = m.pData;
        f[0] = x, f[1] = y, f[2] = z, f[3] = w;
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)b, 0);
    }
}

/* a quad in pixels: the constant buffer maps the unit quad there */
static void draw(ID3D11ShaderResourceView *t, float x, float y, float w, float h)
{
    constants(cb, x / SCREEN_W * 2 - 1, 1 - y / SCREEN_H * 2, w / SCREEN_W * 2, -h / SCREEN_H * 2);
    ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &t);
    ID3D11DeviceContext_Draw(ctx, 4, 0);
}

static void setup_2d(ID3D11RenderTargetView *target, ID3D11PixelShader *ps, ID3D11BlendState *blend)
{
    D3D11_VIEWPORT vp = { 0, 0, SCREEN_W, SCREEN_H, 0, 1 };
    ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &target, NULL);
    ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
    ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &quad_vb, &stride, &zero);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_VSSetConstantBuffers(ctx, 0, 1, &cb);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_PSSetConstantBuffers(ctx, 0, 1, &pcb);
    ID3D11DeviceContext_PSSetSamplers(ctx, 0, 1, &linear);
    ID3D11DeviceContext_OMSetBlendState(ctx, blend, NULL, 0xffffffff);
}

static void clear(ID3D11RenderTargetView *v)
{
    static const float black[4] = { 0, 0, 0, 1 };
    ID3D11DeviceContext_ClearRenderTargetView(ctx, v, black);
}

static void scene(void)
{
    int i;
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H);
    for (i = 0; i < 3; i++) draw(t_chr[i], 40 + i * 410, 80, 400, 640);
    draw(t_win, 40, 500, 1200, 200);
}

static void frame_vn(int f)
{
    const WCHAR *line = bench_lines[(f / 40) % BENCH_LINES];
    int n = f % 40 + 1, len = lstrlenW(line), i;
    memset(text_bits, 0, 1024 * 64 * 4);
    ExtTextOutW(text_dc, 0, 16, 0, NULL, line, n < len ? n : len, NULL);
    GdiFlush();
    for (i = 0; i < 1024 * 64; i++) text_bits[i] = (text_bits[i] & 0xff) << 24 | 0xffffff;
    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)text_tex, 0, NULL, text_bits, 1024 * 4, 0);
    setup_2d(backbuf, ps_sprite_, alpha);
    clear(backbuf);
    scene();
    draw(t_text, 80, 530, 1024, 64);
    IDXGISwapChain_Present(sc, 0, 0);
}

static void frame_fade(int f)
{
    setup_2d(backbuf, ps_fade_, opaque);
    ID3D11DeviceContext_PSSetShaderResources(ctx, 1, 1, &t_bg2);
    constants(pcb, (f % 120) / 119.0f, 0, 0, 0);
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H);
    IDXGISwapChain_Present(sc, 0, 0);
}

static void sprite_vertices(V *v, int i, int f)
{
    float x = (float)((i * 211 + f * (1 + i % 5)) % (SCREEN_W - 32)), y = (float)((i * 97 + f * (1 + i % 3)) % (SCREEN_H - 32));
    float x0 = x / SCREEN_W * 2 - 1, y0 = 1 - y / SCREEN_H * 2, x1 = (x + 32) / SCREEN_W * 2 - 1, y1 = 1 - (y + 32) / SCREEN_H * 2;
    float a = 1 - (i % 64) / 255.0f;
    V q[6] = { { x0, y0, 0, 0, 1, 1, 1, a }, { x1, y0, 1, 0, 1, 1, 1, a }, { x0, y1, 0, 1, 1, 1, 1, a },
               { x0, y1, 0, 1, 1, 1, 1, a }, { x1, y0, 1, 0, 1, 1, 1, a }, { x1, y1, 1, 1, 1, 1, 1, a } };
    memcpy(v, q, sizeof(q));
}

static void frame_sprites_batched(int f)
{
    D3D11_MAPPED_SUBRESOURCE m;
    int i;
    setup_2d(backbuf, ps_sprite_, alpha);
    clear(backbuf);
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H);
    if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)sprite_vb, 0, D3D11_MAP_WRITE_DISCARD, 0, &m)))
    {
        for (i = 0; i < SPRITES; i++) sprite_vertices((V *)m.pData + i * 6, i, f);
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)sprite_vb, 0);
    }
    constants(cb, 0, 0, 1, 1);
    ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &sprite_vb, &stride, &zero);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &t_spr);
    ID3D11DeviceContext_Draw(ctx, SPRITES * 6, 0);
    IDXGISwapChain_Present(sc, 0, 0);
}

static void frame_sprites_calls(int f)
{
    int i;
    setup_2d(backbuf, ps_sprite_, alpha);
    clear(backbuf);
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H);
    for (i = 0; i < SPRITES; i++)
        draw(t_spr, (float)((i * 211 + f * (1 + i % 5)) % (SCREEN_W - 32)), (float)((i * 97 + f * (1 + i % 3)) % (SCREEN_H - 32)), 32, 32);
    IDXGISwapChain_Present(sc, 0, 0);
}

static void frame_rt(int f)
{
    static const float off[4][2] = { { 1.0f / SCREEN_W, 0 }, { 0, 1.0f / SCREEN_H }, { 2.0f / SCREEN_W, 0 }, { 0, 2.0f / SCREEN_H } };
    ID3D11ShaderResourceView *none = NULL;
    int p;
    (void)f;
    setup_2d(rtv[0], ps_sprite_, alpha);
    clear(rtv[0]);
    scene();
    for (p = 0; p < 4; p++)
    {
        int to = (p + 1) & 1;
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &none);
        setup_2d(rtv[to], ps_blur_, opaque);
        constants(pcb, off[p][0], off[p][1], 0, 0);
        draw(srv[p & 1], 0, 0, SCREEN_W, SCREEN_H);
    }
    ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &none);
    setup_2d(backbuf, ps_sprite_, opaque);
    draw(srv[0], 0, 0, SCREEN_W, SCREEN_H);
    IDXGISwapChain_Present(sc, 0, 0);
}

static void frame_upload(int f)
{
    pic_movie(movie, SCREEN_W, SCREEN_H, f);
    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)movie_tex, 0, NULL, movie, SCREEN_W * 4, 0);
    setup_2d(backbuf, ps_sprite_, opaque);
    draw(t_movie, 0, 0, SCREEN_W, SCREEN_H);
    IDXGISwapChain_Present(sc, 0, 0);
}

static void frame_upload_map(int f)
{
    D3D11_MAPPED_SUBRESOURCE m;
    int y;
    pic_movie(movie, SCREEN_W, SCREEN_H, f);
    if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)movie2_tex, 0, D3D11_MAP_WRITE_DISCARD, 0, &m)))
    {
        for (y = 0; y < SCREEN_H; y++) memcpy((BYTE *)m.pData + y * m.RowPitch, movie + y * SCREEN_W, SCREEN_W * 4);
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)movie2_tex, 0);
    }
    setup_2d(backbuf, ps_sprite_, opaque);
    draw(t_movie2, 0, 0, SCREEN_W, SCREEN_H);
    IDXGISwapChain_Present(sc, 0, 0);
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    DXGI_SWAP_CHAIN_DESC sd = { 0 };
    D3D_FEATURE_LEVEL level;
    ID3D11Texture2D *bb;
    HWND hwnd;
    unsigned *p;
    int i;
    /* the unit quad the vertex shader's constants place */
    static const V quad[4] = {
        { 0, 0, 0, 0, 1, 1, 1, 1 }, { 1, 0, 1, 0, 1, 1, 1, 1 }, { 0, 1, 0, 1, 1, 1, 1, 1 }, { 1, 1, 1, 1, 1, 1, 1, 1 },
    };
    static const D3D11_INPUT_ELEMENT_DESC elems[3] = {
        { "POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0 },
        { "TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 8, D3D11_INPUT_PER_VERTEX_DATA, 0 },
        { "COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 16, D3D11_INPUT_PER_VERTEX_DATA, 0 },
    };
    D3D11_BLEND_DESC bd = { 0 };
    D3D11_SAMPLER_DESC smp = { D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE_ADDRESS_CLAMP,
                               D3D11_TEXTURE_ADDRESS_CLAMP, 0, 1, D3D11_COMPARISON_NEVER, { 0, 0, 0, 0 }, 0, D3D11_FLOAT32_MAX };
    (void)inst, (void)prev, (void)cmd, (void)show;
    bench_args();
    hwnd = bench_window("d3d11bench", SCREEN_W, SCREEN_H, NULL);
    sd.BufferDesc.Width = SCREEN_W;
    sd.BufferDesc.Height = SCREEN_H;
    sd.BufferDesc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    sd.SampleDesc.Count = 1;
    sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.BufferCount = 1;
    sd.OutputWindow = hwnd;
    sd.Windowed = TRUE;
    BCHECK(D3D11CreateDeviceAndSwapChain(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, NULL, 0, D3D11_SDK_VERSION, &sd, &sc, &dev, &level, &ctx));
    BCHECK(IDXGISwapChain_GetBuffer(sc, 0, &IID_ID3D11Texture2D, (void **)&bb));
    BCHECK(ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)bb, NULL, &backbuf));
    BCHECK(ID3D11Device_CreateVertexShader(dev, vs_quad, sizeof(vs_quad), NULL, &vs));
    BCHECK(ID3D11Device_CreatePixelShader(dev, ps_sprite, sizeof(ps_sprite), NULL, &ps_sprite_));
    BCHECK(ID3D11Device_CreatePixelShader(dev, ps_fade, sizeof(ps_fade), NULL, &ps_fade_));
    BCHECK(ID3D11Device_CreatePixelShader(dev, ps_blur, sizeof(ps_blur), NULL, &ps_blur_));
    BCHECK(ID3D11Device_CreateInputLayout(dev, elems, 3, vs_quad, sizeof(vs_quad), &layout));
    bd.RenderTarget[0].BlendEnable = TRUE;
    bd.RenderTarget[0].SrcBlend = D3D11_BLEND_SRC_ALPHA;
    bd.RenderTarget[0].DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
    bd.RenderTarget[0].BlendOp = D3D11_BLEND_OP_ADD;
    bd.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_ONE;
    bd.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_INV_SRC_ALPHA;
    bd.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
    bd.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL;
    BCHECK(ID3D11Device_CreateBlendState(dev, &bd, &alpha));
    bd.RenderTarget[0].BlendEnable = FALSE;
    BCHECK(ID3D11Device_CreateBlendState(dev, &bd, &opaque));
    BCHECK(ID3D11Device_CreateSamplerState(dev, &smp, &linear));
    quad_vb = buffer(sizeof(quad), D3D11_BIND_VERTEX_BUFFER, D3D11_USAGE_IMMUTABLE, quad);
    cb = buffer(16, D3D11_BIND_CONSTANT_BUFFER, D3D11_USAGE_DYNAMIC, NULL);
    pcb = buffer(16, D3D11_BIND_CONSTANT_BUFFER, D3D11_USAGE_DYNAMIC, NULL);

    p = pic_alloc(SCREEN_W, SCREEN_H);
    pic_background(p, SCREEN_W, SCREEN_H, 1);
    t_bg = texture(SCREEN_W, SCREEN_H, p, D3D11_USAGE_DEFAULT, 0, NULL);
    pic_background(p, SCREEN_W, SCREEN_H, 2);
    t_bg2 = texture(SCREEN_W, SCREEN_H, p, D3D11_USAGE_DEFAULT, 0, NULL);
    for (i = 0; i < 3; i++)
    {
        pic_character(p, 400, 640, i + 1, 0);
        t_chr[i] = texture(400, 640, p, D3D11_USAGE_DEFAULT, 0, NULL);
    }
    pic_window(p, 1200, 200, 0);
    t_win = texture(1200, 200, p, D3D11_USAGE_DEFAULT, 0, NULL);
    pic_particle(p, 32, 32, 0);
    t_spr = texture(32, 32, p, D3D11_USAGE_DEFAULT, 0, NULL);
    movie = p;
    t_text = texture(1024, 64, NULL, D3D11_USAGE_DEFAULT, 0, &text_tex);
    {
        BITMAPINFO bi = { { sizeof(BITMAPINFOHEADER), 1024, -64, 1, 32, BI_RGB } };
        text_dc = CreateCompatibleDC(NULL);
        SelectObject(text_dc, CreateDIBSection(text_dc, &bi, DIB_RGB_COLORS, (void **)&text_bits, NULL, 0));
        SelectObject(text_dc, CreateFontW(-32, 0, 0, 0, FW_NORMAL, 0, 0, 0, SHIFTJIS_CHARSET, 0, 0, ANTIALIASED_QUALITY, 0, L"MS Gothic"));
        SetBkMode(text_dc, TRANSPARENT);
        SetTextColor(text_dc, RGB(255, 255, 255));
    }

    if (bench_is("vn")) bench_frames("vn", 20, 240, frame_vn);
    if (bench_is("fade")) bench_frames("fade", 20, 240, frame_fade);
    if (bench_is("sprites_batched") || bench_is("sprites_calls"))
    {
        sprite_vb = buffer(SPRITES * 6 * sizeof(V), D3D11_BIND_VERTEX_BUFFER, D3D11_USAGE_DYNAMIC, NULL);
        if (bench_is("sprites_batched")) bench_frames("sprites_batched", 20, 240, frame_sprites_batched);
        if (bench_is("sprites_calls")) bench_frames("sprites_calls", 10, 120, frame_sprites_calls);
    }
    if (bench_is("rt"))
    {
        for (i = 0; i < 2; i++)
        {
            ID3D11Texture2D *t;
            srv[i] = texture(SCREEN_W, SCREEN_H, NULL, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, &t);
            BCHECK(ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)t, NULL, &rtv[i]));
        }
        bench_frames("rt", 20, 240, frame_rt);
    }
    if (bench_is("upload"))
    {
        t_movie = texture(SCREEN_W, SCREEN_H, NULL, D3D11_USAGE_DEFAULT, 0, &movie_tex);
        bench_frames("upload", 20, 240, frame_upload);
    }
    if (bench_is("upload_map"))
    {
        t_movie2 = texture(SCREEN_W, SCREEN_H, NULL, D3D11_USAGE_DYNAMIC, 0, &movie2_tex);
        bench_frames("upload_map", 20, 240, frame_upload_map);
    }
    bench_end();
    return 0;
}
