/* DirectDraw and Direct3D 7 object lifetimes, as Windows counts them: references of separate
 * DirectDraw objects, of each IDirectDraw version and IDirect3D7 of one object, of surfaces
 * handed out by GetGDISurface / GetDDInterface; what an IDirect3DDevice7 holds (its render
 * target after CreateDevice and SetRenderTarget, textures after SetTexture, state blocks) and
 * drawing after the caller dropped its own reference of the render target; vertex buffer
 * sizes at the edges (no vertices, no position, counts whose byte size passes 32 bits) and
 * light indices far past the ones set.
 * libs: -lddraw -ldxguid -lgdi32
 * arch: 32 (DirectDraw games are 32-bit; sake has no x64 DirectDraw) */
#define COBJMACROS
#include "../probe.h"
#include <ddraw.h>
#include <d3d.h>

/* the references of an object: AddRef then Release */
static ULONG refs(void *p) {
    IUnknown *u = p;
    IUnknown_AddRef(u);
    return IUnknown_Release(u);
}

static IDirectDrawSurface7 *surface(IDirectDraw7 *dd, DWORD caps, int w, int h) {
    DDSURFACEDESC2 d = { sizeof d };
    d.dwFlags = DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT | DDSD_PIXELFORMAT;
    d.dwWidth = w;
    d.dwHeight = h;
    d.ddsCaps.dwCaps = caps;
    d.ddpfPixelFormat.dwSize = sizeof d.ddpfPixelFormat;
    d.ddpfPixelFormat.dwFlags = DDPF_RGB;
    d.ddpfPixelFormat.dwRGBBitCount = 32;
    d.ddpfPixelFormat.dwRBitMask = 0xff0000;
    d.ddpfPixelFormat.dwGBitMask = 0xff00;
    d.ddpfPixelFormat.dwBBitMask = 0xff;
    IDirectDrawSurface7 *s = NULL;
    HRESULT hr = IDirectDraw7_CreateSurface(dd, &d, &s, NULL);
    if (FAILED(hr)) printf("CreateSurface caps %#lx: %#lx\n", caps, hr);
    return s;
}

static void show(const char *name, IDirectDrawSurface7 *s) {
    DDSURFACEDESC2 d = { sizeof d };
    HRESULT hr = IDirectDrawSurface7_Lock(s, NULL, &d, DDLOCK_WAIT | DDLOCK_READONLY, NULL);
    if (FAILED(hr)) { printf("%s: Lock %#lx\n", name, hr); return; }
    unsigned char *px = malloc(d.dwWidth * d.dwHeight * 4);
    for (DWORD y = 0; y < d.dwHeight; y++)
        for (DWORD x = 0; x < d.dwWidth; x++) {
            DWORD v = ((DWORD *)((unsigned char *)d.lpSurface + y * d.lPitch))[x];
            unsigned char *o = px + (y * d.dwWidth + x) * 4;
            o[0] = v; o[1] = v >> 8; o[2] = v >> 16; o[3] = 0;
        }
    IDirectDrawSurface7_Unlock(s, NULL);
    probe_image(name, px, d.dwWidth, d.dwHeight, d.dwWidth * 4, 2);
    free(px);
}

static void objects(HWND hwnd) {
    /* two DirectDraw objects count apart */
    IDirectDraw *a = NULL, *b = NULL;
    printf("DirectDrawCreate a %#lx\n", DirectDrawCreate(NULL, &a, NULL));
    printf("DirectDrawCreate b %#lx\n", DirectDrawCreate(NULL, &b, NULL));
    if (!a || !b) return;
    printf("separate objects %d\n", a != b);
    printf("a AddRef %lu\n", IDirectDraw_AddRef(a));
    printf("b Release %lu\n", IDirectDraw_Release(b));
    printf("a refs %lu\n", refs(a));
    printf("a Release %lu\n", IDirectDraw_Release(a));
    printf("a Release %lu\n", IDirectDraw_Release(a));

    /* each interface version of one object */
    IDirectDraw7 *dd = NULL;
    printf("DirectDrawCreateEx %#lx\n", DirectDrawCreateEx(NULL, (void **)&dd, &IID_IDirectDraw7, NULL));
    if (!dd) return;
    IDirectDraw7_SetCooperativeLevel(dd, hwnd, DDSCL_NORMAL);
    IDirectDraw *d1 = NULL;
    IDirectDraw4 *d4 = NULL;
    IDirect3D7 *d3d = NULL;
    printf("QI IDirectDraw %#lx\n", IDirectDraw7_QueryInterface(dd, &IID_IDirectDraw, (void **)&d1));
    printf("QI IDirectDraw4 %#lx\n", IDirectDraw7_QueryInterface(dd, &IID_IDirectDraw4, (void **)&d4));
    printf("refs dd7 %lu dd1 %lu dd4 %lu\n", refs(dd), d1 ? refs(d1) : 0, d4 ? refs(d4) : 0);
    printf("QI IDirect3D7 %#lx\n", IDirectDraw7_QueryInterface(dd, &IID_IDirect3D7, (void **)&d3d));
    printf("refs dd7 %lu d3d7 %lu\n", refs(dd), d3d ? refs(d3d) : 0);
    IDirectDraw *d1b = NULL;
    IDirectDraw4_QueryInterface(d4, &IID_IDirectDraw, (void **)&d1b);
    printf("QI again same interface %d refs dd1 %lu\n", d1b == d1, refs(d1));
    if (d1b) IDirectDraw_Release(d1b);

    /* surfaces: what they hold of the object; GetDDInterface; GetGDISurface */
    IDirectDrawSurface7 *s = surface(dd, DDSCAPS_OFFSCREENPLAIN | DDSCAPS_SYSTEMMEMORY, 8, 8);
    printf("refs dd7 after CreateSurface %lu\n", refs(dd));
    if (s) {
        IUnknown *back = NULL;
        printf("GetDDInterface %#lx\n", IDirectDrawSurface7_GetDDInterface(s, (void **)&back));
        printf("GetDDInterface is dd7 %d\n", back == (IUnknown *)dd);
        printf("refs dd7 %lu\n", refs(dd));
        if (back) IUnknown_Release(back);
        printf("surface Release %lu\n", IDirectDrawSurface7_Release(s));
        printf("refs dd7 %lu\n", refs(dd));
    }
    DDSURFACEDESC2 pd = { sizeof pd };
    pd.dwFlags = DDSD_CAPS;
    pd.ddsCaps.dwCaps = DDSCAPS_PRIMARYSURFACE;
    IDirectDrawSurface7 *prim = NULL, *gdi = NULL;
    printf("CreateSurface primary %#lx\n", IDirectDraw7_CreateSurface(dd, &pd, &prim, NULL));
    if (prim) {
        printf("refs primary %lu\n", refs(prim));
        printf("GetGDISurface %#lx\n", IDirectDraw7_GetGDISurface(dd, &gdi));
        printf("GetGDISurface is primary %d\n", gdi == prim);
        printf("refs primary %lu\n", refs(prim));
        if (gdi) printf("gdi Release %lu\n", IDirectDrawSurface7_Release(gdi));
        printf("primary Release %lu\n", IDirectDrawSurface7_Release(prim));
    }
    if (d3d) printf("d3d7 Release %lu\n", IDirect3D7_Release(d3d));
    if (d4) printf("dd4 Release %lu\n", IDirectDraw4_Release(d4));
    if (d1) printf("dd1 Release %lu\n", IDirectDraw_Release(d1));
    printf("dd7 Release %lu\n", IDirectDraw7_Release(dd));
}

struct v { float x, y, z, rhw; DWORD c; };

static void tri(IDirect3DDevice7 *dev, DWORD color) {
    struct v t[3] = { { 2, 2, 0.5f, 1, color }, { 30, 4, 0.5f, 1, color }, { 6, 28, 0.5f, 1, color } };
    HRESULT hr = IDirect3DDevice7_DrawPrimitive(dev, D3DPT_TRIANGLELIST, D3DFVF_XYZRHW | D3DFVF_DIFFUSE, t, 3, 0);
    if (FAILED(hr)) printf("DrawPrimitive %#lx\n", hr);
}

static void device(HWND hwnd) {
    IDirectDraw7 *dd = NULL;
    if (FAILED(DirectDrawCreateEx(NULL, (void **)&dd, &IID_IDirectDraw7, NULL))) return;
    IDirectDraw7_SetCooperativeLevel(dd, hwnd, DDSCL_NORMAL);
    IDirect3D7 *d3d = NULL;
    if (FAILED(IDirectDraw7_QueryInterface(dd, &IID_IDirect3D7, (void **)&d3d))) { puts("no IDirect3D7"); return; }
    /* a software device on system-memory surfaces where there is one, else the HAL on video
     * memory (Windows without a D3D7 driver offers RGB; sake takes any) */
    const GUID *devs[2] = { &IID_IDirect3DRGBDevice, &IID_IDirect3DHALDevice };
    DWORD mems[2] = { DDSCAPS_SYSTEMMEMORY, DDSCAPS_VIDEOMEMORY };
    IDirect3DDevice7 *dev = NULL;
    IDirectDrawSurface7 *rt = NULL;
    for (int i = 0; i < 2 && !dev; i++) {
        rt = surface(dd, DDSCAPS_OFFSCREENPLAIN | DDSCAPS_3DDEVICE | mems[i], 32, 32);
        if (!rt) continue;
        if (FAILED(IDirect3D7_CreateDevice(d3d, devs[i], rt, &dev))) {
            IDirectDrawSurface7_Release(rt);
            rt = NULL;
        }
    }
    if (!dev) { puts("no Direct3D 7 device"); return; }
    puts("CreateDevice ok");
    printf("refs rt after CreateDevice %lu\n", refs(rt));
    printf("refs d3d7 after CreateDevice %lu dd7 %lu\n", refs(d3d), refs(dd));
    /* the caller lets go of the render target: the device keeps it and draws into it */
    printf("rt Release %lu\n", IDirectDrawSurface7_Release(rt));
    D3DVIEWPORT7 vp = { 0, 0, 32, 32, 0, 1 };
    IDirect3DDevice7_SetViewport(dev, &vp);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_LIGHTING, FALSE);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_CULLMODE, D3DCULL_NONE);
    printf("Clear %#lx\n", IDirect3DDevice7_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0x203040, 1, 0));
    printf("BeginScene %#lx\n", IDirect3DDevice7_BeginScene(dev));
    tri(dev, 0xffff8000);
    printf("EndScene %#lx\n", IDirect3DDevice7_EndScene(dev));
    IDirectDrawSurface7 *got = NULL;
    printf("GetRenderTarget %#lx\n", IDirect3DDevice7_GetRenderTarget(dev, &got));
    if (got) {
        printf("refs rt via GetRenderTarget %lu\n", refs(got));
        show("orphan_rt", got);
        IDirectDrawSurface7_Release(got);
    }

    /* textures: what SetTexture and state blocks hold */
    IDirectDrawSurface7 *tex = surface(dd, DDSCAPS_TEXTURE | DDSCAPS_SYSTEMMEMORY, 4, 4);
    if (tex) {
        printf("refs tex %lu\n", refs(tex));
        printf("SetTexture %#lx\n", IDirect3DDevice7_SetTexture(dev, 0, tex));
        printf("  refs tex %lu\n", refs(tex));
        printf("SetTexture again %#lx\n", IDirect3DDevice7_SetTexture(dev, 0, tex));
        printf("  refs tex %lu\n", refs(tex));
        printf("SetTexture stage 1 %#lx\n", IDirect3DDevice7_SetTexture(dev, 1, tex));
        printf("  refs tex %lu\n", refs(tex));
        DWORD sb = 0;
        printf("CreateStateBlock %#lx\n", IDirect3DDevice7_CreateStateBlock(dev, D3DSBT_ALL, &sb));
        printf("  refs tex %lu\n", refs(tex));
        printf("SetTexture NULL %#lx\n", IDirect3DDevice7_SetTexture(dev, 1, NULL));
        printf("  refs tex %lu\n", refs(tex));
        IDirect3DDevice7_SetTexture(dev, 0, NULL);
        printf("refs tex with no stage %lu\n", refs(tex));
        if (sb) {
            printf("ApplyStateBlock %#lx\n", IDirect3DDevice7_ApplyStateBlock(dev, sb));
        printf("  refs tex %lu\n", refs(tex));
            IDirect3DDevice7_SetTexture(dev, 0, NULL);
            IDirect3DDevice7_SetTexture(dev, 1, NULL);
            printf("DeleteStateBlock %#lx\n", IDirect3DDevice7_DeleteStateBlock(dev, sb));
        printf("  refs tex %lu\n", refs(tex));
        }
        IDirect3DDevice7_SetTexture(dev, 0, tex);
    }

    /* SetRenderTarget: the old one goes, the new one is held */
    IDirectDrawSurface7 *rt2 = surface(dd, DDSCAPS_OFFSCREENPLAIN | DDSCAPS_3DDEVICE | DDSCAPS_SYSTEMMEMORY, 32, 32);
    if (rt2) {
        printf("refs rt2 %lu\n", refs(rt2));
        HRESULT hr = IDirect3DDevice7_SetRenderTarget(dev, rt2, 0);
        printf("SetRenderTarget %#lx refs rt2 %lu\n", hr, refs(rt2));
        if (SUCCEEDED(hr)) {
            IDirectDrawSurface7 *cur = NULL;
            IDirect3DDevice7_GetRenderTarget(dev, &cur);
            printf("GetRenderTarget is rt2 %d\n", cur == rt2);
            if (cur) IDirectDrawSurface7_Release(cur);
        }
    }

    /* vertex buffers at the edges */
    DWORD counts[5] = { 3, 0, 0x08000000, 0x10000001, 0xffffffff };
    DWORD fvfs[2] = { D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_SPECULAR | D3DFVF_TEX1, D3DFVF_DIFFUSE };
    for (int f = 0; f < 2; f++)
        for (int i = 0; i < 5; i++) {
            D3DVERTEXBUFFERDESC vd = { sizeof vd, D3DVBCAPS_SYSTEMMEMORY, fvfs[f], counts[i] };
            IDirect3DVertexBuffer7 *vb = NULL;
            HRESULT hr = IDirect3D7_CreateVertexBuffer(d3d, &vd, &vb, 0);
            printf("CreateVertexBuffer fvf %#lx count %#lx: %#lx\n", fvfs[f], counts[i], hr);
            if (vb) {
                void *p = NULL;
                DWORD size = 0;
                hr = IDirect3DVertexBuffer7_Lock(vb, DDLOCK_WAIT, &p, &size);
                printf("  Lock %#lx size %lu\n", hr, size);
                if (SUCCEEDED(hr)) IDirect3DVertexBuffer7_Unlock(vb);
                IDirect3DVertexBuffer7_Release(vb);
            }
        }

    /* light indices far apart */
    D3DLIGHT7 l = { D3DLIGHT_DIRECTIONAL };
    l.dcvDiffuse.r = l.dcvDiffuse.g = l.dcvDiffuse.b = 1;
    l.dvDirection.z = 1;
    DWORD idx[4] = { 0, 7, 100000, 0xfffffffe };
    for (int i = 0; i < 4; i++) printf("SetLight %#lx %#lx\n", idx[i], IDirect3DDevice7_SetLight(dev, idx[i], &l));
    D3DLIGHT7 g;
    printf("GetLight 3 (never set) %#lx\n", IDirect3DDevice7_GetLight(dev, 3, &g));
    printf("GetLight 100000 %#lx\n", IDirect3DDevice7_GetLight(dev, 100000, &g));
    printf("LightEnable 0x7ffffff0 (never set) %#lx\n", IDirect3DDevice7_LightEnable(dev, 0x7ffffff0, TRUE));
    BOOL on = 9;
    printf("GetLightEnable 0x7ffffff0 %#lx\n", IDirect3DDevice7_GetLightEnable(dev, 0x7ffffff0, &on));
    printf("  on %d\n", on);
    on = 9;
    printf("GetLightEnable 7 %#lx\n", IDirect3DDevice7_GetLightEnable(dev, 7, &on));
    printf("  on %d\n", on);
    printf("GetLightEnable 5 (never set) %#lx\n", IDirect3DDevice7_GetLightEnable(dev, 5, &on));

    /* the device goes: what it held goes with it */
    printf("device Release %lu\n", IDirect3DDevice7_Release(dev));
    if (tex) printf("tex Release %lu\n", IDirectDrawSurface7_Release(tex));
    if (rt2) printf("rt2 Release %lu\n", IDirectDrawSurface7_Release(rt2));
    printf("d3d7 Release %lu\n", IDirect3D7_Release(d3d));
    printf("dd7 Release %lu\n", IDirectDraw7_Release(dd));
}

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "ddrefs", WS_OVERLAPPEDWINDOW, 0, 0, 320, 240, 0, 0, wc.hInstance, 0);
    objects(hwnd);
    device(hwnd);
    return 0;
}
