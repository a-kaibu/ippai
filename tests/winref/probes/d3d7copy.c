/* Direct3D 7 drawing a texture one texel to a pixel, as KiriKiri 2 presents every frame: a quad of
 * pretransformed vertices at pixel edges (x - 0.5, y - 0.5) with u, v from 0 to 1 over a texture
 * of the target's size, MODULATE(TEXTURE, CURRENT: the white diffuse), alpha SELECTARG1
 * (TEXTURE), bilinear filtering, clamped. Every pixel is its texel (sake copies such quads
 * instead of filtering them: Raster::copy_plan). Also a part of the texture to another place
 * (texels from (8, 4) to the target's (16, 8)), point and bilinear, where the pixels around it
 * keep the clear color, and the same part half a texel off (bilinear blends neighbours there:
 * not a copy).
 * libs: -lddraw -ldxguid -lgdi32
 * arch: 32 (DirectDraw games are 32-bit; sake has no x64 DirectDraw) */
#define COBJMACROS
#include "../probe.h"
#include <ddraw.h>
#include <d3d.h>

#define W 64
#define H 48

static IDirectDrawSurface7 *surface(IDirectDraw7 *dd, DWORD caps, DWORD amask) {
    DDSURFACEDESC2 d = { sizeof d };
    d.dwFlags = DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT | DDSD_PIXELFORMAT;
    d.dwWidth = W;
    d.dwHeight = H;
    d.ddsCaps.dwCaps = caps;
    d.ddpfPixelFormat.dwSize = sizeof d.ddpfPixelFormat;
    d.ddpfPixelFormat.dwFlags = DDPF_RGB | (amask ? DDPF_ALPHAPIXELS : 0);
    d.ddpfPixelFormat.dwRGBBitCount = 32;
    d.ddpfPixelFormat.dwRBitMask = 0xff0000;
    d.ddpfPixelFormat.dwGBitMask = 0xff00;
    d.ddpfPixelFormat.dwBBitMask = 0xff;
    d.ddpfPixelFormat.dwRGBAlphaBitMask = amask;
    IDirectDrawSurface7 *s = NULL;
    HRESULT hr = IDirectDraw7_CreateSurface(dd, &d, &s, NULL);
    if (FAILED(hr)) printf("CreateSurface caps %#lx: %#lx\n", caps, hr);
    return s;
}

static DWORD texel(int x, int y) {
    DWORD v = (DWORD)(x * 2654435761u) ^ (DWORD)(y * 40503u * 977u);
    v ^= v >> 13;
    v *= 0x5bd1e995;
    return v ^ (v >> 15);
}

static int fill(IDirectDrawSurface7 *t) {
    DDSURFACEDESC2 d = { sizeof d };
    if (FAILED(IDirectDrawSurface7_Lock(t, NULL, &d, DDLOCK_WAIT, NULL))) return 0;
    for (int y = 0; y < H; y++)
        for (int x = 0; x < W; x++) ((DWORD *)((char *)d.lpSurface + y * d.lPitch))[x] = texel(x, y);
    IDirectDrawSurface7_Unlock(t, NULL);
    return 1;
}

/* the target's pixels (x8r8g8b8) */
static int pixels(IDirectDrawSurface7 *rt, DWORD *out) {
    DDSURFACEDESC2 d = { sizeof d };
    if (FAILED(IDirectDrawSurface7_Lock(rt, NULL, &d, DDLOCK_WAIT | DDLOCK_READONLY, NULL))) return 0;
    for (int y = 0; y < H; y++) memcpy(out + y * W, (char *)d.lpSurface + y * d.lPitch, W * 4);
    IDirectDrawSurface7_Unlock(rt, NULL);
    return 1;
}

struct tv { float x, y, z, rhw; DWORD c; float u, v; };

/* pixels (x0, y0)-(x1, y1) showing texels from (tx, ty) on, `sub` texels further */
static void quad(IDirect3DDevice7 *dev, int x0, int y0, int x1, int y1, int tx, int ty, float sub) {
    float u0 = (tx + sub) / W, v0 = (ty + sub) / H, u1 = (tx + sub + x1 - x0) / W, v1 = (ty + sub + y1 - y0) / H;
    struct tv q[4] = {
        { x0 - 0.5f, y0 - 0.5f, 0.5f, 1, 0xffffffff, u0, v0 }, { x1 - 0.5f, y0 - 0.5f, 0.5f, 1, 0xffffffff, u1, v0 },
        { x0 - 0.5f, y1 - 0.5f, 0.5f, 1, 0xffffffff, u0, v1 }, { x1 - 0.5f, y1 - 0.5f, 0.5f, 1, 0xffffffff, u1, v1 },
    };
    IDirect3DDevice7_BeginScene(dev);
    HRESULT hr = IDirect3DDevice7_DrawPrimitive(dev, D3DPT_TRIANGLESTRIP, D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1, q, 4, 0);
    IDirect3DDevice7_EndScene(dev);
    if (FAILED(hr)) printf("DrawPrimitive %#lx\n", hr);
}

static void report(const char *name, IDirectDrawSurface7 *rt, int x0, int y0, int x1, int y1, int tx, int ty, DWORD clear) {
    static DWORD px[W * H];
    if (!pixels(rt, px)) { printf("%s: Lock failed\n", name); return; }
    int same = 0, kept = 0, inside = (x1 - x0) * (y1 - y0);
    for (int y = 0; y < H; y++)
        for (int x = 0; x < W; x++) {
            DWORD v = px[y * W + x] & 0xffffff;
            if (x >= x0 && x < x1 && y >= y0 && y < y1) same += v == (texel(x - x0 + tx, y - y0 + ty) & 0xffffff);
            else kept += v == (clear & 0xffffff);
        }
    printf("%s: outside kept %d of %d\n", name, kept, W * H - inside);
    /* (the texels themselves: a GPU's filter may round one step off, the picture allows it) */
    printf("%s: inside texels %s\n", name, same == inside ? "all" : same > inside / 2 ? "most" : "few");
    probe_image(name, px, W, H, W * 4, 1);
}

static void device(HWND hwnd) {
    IDirectDraw7 *dd = NULL;
    if (FAILED(DirectDrawCreateEx(NULL, (void **)&dd, &IID_IDirectDraw7, NULL))) { puts("skip no DirectDraw"); return; }
    IDirectDraw7_SetCooperativeLevel(dd, hwnd, DDSCL_NORMAL);
    IDirect3D7 *d3d = NULL;
    if (FAILED(IDirectDraw7_QueryInterface(dd, &IID_IDirect3D7, (void **)&d3d))) { puts("skip no IDirect3D7"); return; }
    /* a software device on system-memory surfaces where there is one, else the HAL on video
     * memory (as d3d7state) */
    const GUID *devs[2] = { &IID_IDirect3DRGBDevice, &IID_IDirect3DHALDevice };
    DWORD mems[2] = { DDSCAPS_SYSTEMMEMORY, DDSCAPS_VIDEOMEMORY };
    IDirect3DDevice7 *dev = NULL;
    IDirectDrawSurface7 *rt = NULL;
    DWORD mem = 0;
    for (int i = 0; i < 2 && !dev; i++) {
        rt = surface(dd, DDSCAPS_OFFSCREENPLAIN | DDSCAPS_3DDEVICE | mems[i], 0);
        if (!rt) continue;
        mem = mems[i];
        if (FAILED(IDirect3D7_CreateDevice(d3d, devs[i], rt, &dev))) {
            IDirectDrawSurface7_Release(rt);
            rt = NULL;
        }
    }
    if (!dev) { puts("skip no Direct3D 7 device"); return; }
    IDirectDrawSurface7 *tex = surface(dd, DDSCAPS_TEXTURE | mem, 0xff000000);
    if (!tex || !fill(tex)) { puts("texture failed"); return; }
    D3DVIEWPORT7 vp = { 0, 0, W, H, 0, 1 };
    IDirect3DDevice7_SetViewport(dev, &vp);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_LIGHTING, FALSE);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_ZENABLE, FALSE);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_ALPHABLENDENABLE, FALSE);
    IDirect3DDevice7_SetTexture(dev, 0, tex);
    IDirect3DDevice7_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_MODULATE);
    IDirect3DDevice7_SetTextureStageState(dev, 0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
    IDirect3DDevice7_SetTextureStageState(dev, 0, D3DTSS_COLORARG2, D3DTA_CURRENT);
    IDirect3DDevice7_SetTextureStageState(dev, 0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
    IDirect3DDevice7_SetTextureStageState(dev, 0, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
    IDirect3DDevice7_SetTextureStageState(dev, 0, D3DTSS_ADDRESS, D3DTADDRESS_CLAMP);
    IDirect3DDevice7_SetTextureStageState(dev, 1, D3DTSS_COLOROP, D3DTOP_DISABLE);
    const DWORD clear = 0xff102030;
    IDirect3DDevice7_SetTextureStageState(dev, 0, D3DTSS_MAGFILTER, D3DTFG_LINEAR);
    IDirect3DDevice7_SetTextureStageState(dev, 0, D3DTSS_MINFILTER, D3DTFN_LINEAR);
    IDirect3DDevice7_Clear(dev, 0, NULL, D3DCLEAR_TARGET, clear, 1, 0);
    quad(dev, 0, 0, W, H, 0, 0, 0);
    report("present", rt, 0, 0, W, H, 0, 0, clear);
    IDirect3DDevice7_Clear(dev, 0, NULL, D3DCLEAR_TARGET, clear, 1, 0);
    quad(dev, 16, 8, 48, 40, 8, 4, 0);
    report("part-linear", rt, 16, 8, 48, 40, 8, 4, clear);
    IDirect3DDevice7_SetTextureStageState(dev, 0, D3DTSS_MAGFILTER, D3DTFG_POINT);
    IDirect3DDevice7_SetTextureStageState(dev, 0, D3DTSS_MINFILTER, D3DTFN_POINT);
    IDirect3DDevice7_Clear(dev, 0, NULL, D3DCLEAR_TARGET, clear, 1, 0);
    quad(dev, 16, 8, 48, 40, 8, 4, 0);
    report("part-point", rt, 16, 8, 48, 40, 8, 4, clear);
    /* half a texel off, bilinear: the pixels are blends (only the count of kept ones is exact) */
    IDirect3DDevice7_SetTextureStageState(dev, 0, D3DTSS_MAGFILTER, D3DTFG_LINEAR);
    IDirect3DDevice7_SetTextureStageState(dev, 0, D3DTSS_MINFILTER, D3DTFN_LINEAR);
    IDirect3DDevice7_Clear(dev, 0, NULL, D3DCLEAR_TARGET, clear, 1, 0);
    quad(dev, 16, 8, 48, 40, 8, 4, 0.5f);
    {
        static DWORD px[W * H];
        int kept = 0, same = 0;
        if (pixels(rt, px))
            for (int y = 0; y < H; y++)
                for (int x = 0; x < W; x++) {
                    int in = x >= 16 && x < 48 && y >= 8 && y < 40;
                    DWORD v = px[y * W + x] & 0xffffff;
                    if (!in) kept += v == (clear & 0xffffff);
                    else same += v == (texel(x - 16 + 8, y - 8 + 4) & 0xffffff);
                }
        printf("half-texel: outside kept %d, inside texels unchanged %s\n", kept, same > 32 * 32 / 2 ? "most" : "few");
    }
    IDirectDrawSurface7_Release(tex);
    IDirect3DDevice7_Release(dev);
    IDirectDrawSurface7_Release(rt);
    IDirect3D7_Release(d3d);
    IDirectDraw7_Release(dd);
}

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "d3d7copy", WS_OVERLAPPEDWINDOW, 0, 0, 320, 240, 0, 0, wc.hInstance, 0);
    device(hwnd);
    return 0;
}
