/* Direct3D 8 check for vnvm: a textured quad with fixed function (XYZRHW, texture written with
 * LockRect, Clear) and a spinning triangle drawn with a vs.1.1 vertex shader (with its own
 * declaration) and a ps.1.1 pixel shader. Prints each step and quits after 60 s.
 * Build: i686-w64-mingw32-gcc -O2 d3d8test.c -o d3d8test.exe -mwindows -ld3d8 */
#include <windows.h>
#include <d3d8.h>
#include <math.h>
#include <stdio.h>

static LRESULT CALLBACK wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_DESTROY) PostQuitMessage(0);
    return DefWindowProcA(hwnd, msg, wp, lp);
}

#define CHECK(x) do { HRESULT hr_ = (x); printf("%s -> %#lx\n", #x, (unsigned long)hr_); fflush(stdout); if (FAILED(hr_)) return 1; } while (0)

struct quad { float x, y, z, rhw; DWORD color; float u, v; };
struct tri { float x, y, z; DWORD color; };

/* m4x4 oPos, v0, c0 / mov oD0, v5 */
static const DWORD vs_code[] = { 0xfffe0101, 0x14, 0xc00f0000, 0x90e40000, 0xa0e40000,
    0x01, 0xd00f0000, 0x90e40005, 0xffff };
/* mov r0, v0 */
static const DWORD ps_code[] = { 0xffff0101, 0x01, 0x800f0000, 0x90e40000, 0xffff };
static const DWORD vs_decl[] = { D3DVSD_STREAM(0), D3DVSD_REG(0, D3DVSDT_FLOAT3), D3DVSD_REG(5, D3DVSDT_D3DCOLOR), D3DVSD_END() };

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    WNDCLASSA wc = { 0, wndproc, 0, 0, inst, 0, LoadCursor(0, IDC_ARROW), 0, 0, "d3d8test" };
    D3DPRESENT_PARAMETERS pp = { 0 };
    D3DDISPLAYMODE mode;
    D3DLOCKED_RECT lr;
    IDirect3D8 *d3d;
    IDirect3DDevice8 *dev;
    IDirect3DTexture8 *tex;
    IDirect3DVertexBuffer8 *vb;
    DWORD vs, ps, start = GetTickCount();
    HWND hwnd;
    MSG msg;
    BYTE *p;

    RegisterClassA(&wc);
    hwnd = CreateWindowA("d3d8test", "d3d8test", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0, 656, 519, 0, 0, inst, 0);
    d3d = Direct3DCreate8(D3D_SDK_VERSION);
    printf("Direct3DCreate8 -> %p\n", d3d);
    if (!d3d) return 1;
    CHECK(IDirect3D8_GetAdapterDisplayMode(d3d, 0, &mode));
    pp.Windowed = TRUE;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
    pp.BackBufferFormat = mode.Format;
    CHECK(IDirect3D8_CreateDevice(d3d, 0, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev));
    CHECK(IDirect3DDevice8_CreateTexture(dev, 64, 64, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &tex));
    CHECK(IDirect3DTexture8_LockRect(tex, 0, &lr, NULL, 0));
    for (int y = 0; y < 64; y++)
        for (int x = 0; x < 64; x++)
            ((DWORD *)((BYTE *)lr.pBits + y * lr.Pitch))[x] = ((x / 8 + y / 8) & 1) ? 0xffffff00 : 0xff0000ff;
    CHECK(IDirect3DTexture8_UnlockRect(tex, 0));
    CHECK(IDirect3DDevice8_CreateVertexBuffer(dev, 3 * sizeof(struct tri), D3DUSAGE_WRITEONLY, 0, D3DPOOL_MANAGED, &vb));
    CHECK(IDirect3DVertexBuffer8_Lock(vb, 0, 0, &p, 0));
    {
        struct tri t[3] = { { 0, 0.8f, 0.5f, 0xffff0000 }, { 0.7f, -0.6f, 0.5f, 0xff00ff00 }, { -0.7f, -0.6f, 0.5f, 0xff0000ff } };
        memcpy(p, t, sizeof(t));
    }
    CHECK(IDirect3DVertexBuffer8_Unlock(vb));
    CHECK(IDirect3DDevice8_CreateVertexShader(dev, vs_decl, vs_code, &vs, 0));
    CHECK(IDirect3DDevice8_CreatePixelShader(dev, ps_code, &ps));
    IDirect3DDevice8_SetRenderState(dev, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_ZENABLE, FALSE);

    for (int t = 0; GetTickCount() - start < 60000; t++)
    {
        struct quad q[4] = {
            { 40, 40, 0.5f, 1, 0xffffffff, 0, 0 }, { 280, 40, 0.5f, 1, 0xffffffff, 4, 0 },
            { 40, 280, 0.5f, 1, 0xffffffff, 0, 4 }, { 280, 280, 0.5f, 1, 0xffffffff, 4, 4 },
        };
        float a = t * 0.03f, c[16] = { cosf(a) * 0.6f, -sinf(a) * 0.6f, 0, 0.4f, sinf(a) * 0.6f, cosf(a) * 0.6f, 0, -0.2f, 0, 0, 1, 0, 0, 0, 0, 1 };
        HRESULT hr;

        while (PeekMessageA(&msg, 0, 0, 0, PM_REMOVE))
        {
            if (msg.message == WM_QUIT) return 0;
            DispatchMessageA(&msg);
        }
        IDirect3DDevice8_Clear(dev, 0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB(32, 32, 64 + (t % 128)), 1.0f, 0);
        IDirect3DDevice8_BeginScene(dev);
        /* fixed function, textured */
        IDirect3DDevice8_SetVertexShader(dev, D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1);
        IDirect3DDevice8_SetPixelShader(dev, 0);
        IDirect3DDevice8_SetTexture(dev, 0, (IDirect3DBaseTexture8 *)tex);
        IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_ADDRESSU, D3DTADDRESS_WRAP);
        IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_MAGFILTER, D3DTEXF_POINT);
        IDirect3DDevice8_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, q, sizeof(q[0]));
        /* shaders */
        IDirect3DDevice8_SetTexture(dev, 0, NULL);
        IDirect3DDevice8_SetVertexShader(dev, vs);
        IDirect3DDevice8_SetPixelShader(dev, ps);
        IDirect3DDevice8_SetVertexShaderConstant(dev, 0, c, 4);
        IDirect3DDevice8_SetStreamSource(dev, 0, vb, sizeof(struct tri));
        IDirect3DDevice8_DrawPrimitive(dev, D3DPT_TRIANGLELIST, 0, 1);
        IDirect3DDevice8_EndScene(dev);
        hr = IDirect3DDevice8_Present(dev, NULL, NULL, NULL, NULL);
        if (t % 60 == 0)
        {
            printf("frame %d at %lu ms (present %#lx)\n", t, GetTickCount() - start, (unsigned long)hr);
            fflush(stdout);
        }
        Sleep(16);
    }
    return 0;
}
