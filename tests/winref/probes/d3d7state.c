/* Direct3D 7 and DirectDraw answers Wine's ddraw tests (d3d.c, visual.c, dsurface.c) look at,
 * as Windows gives them: BeginScene / EndScene out of order; SetLight's checks (light types,
 * a point or spot light's attenuation below 0, NaN) and the light LightEnable makes of an index
 * never set; SetMaterial / GetCaps / EnumDevices without their pointers; EnumSurfaces of what
 * exists; a 24-bit depth buffer's pitch and bit depth; DDBLT_DEPTHFILL into a depth buffer (and
 * into a surface that is not one); GetVertexBufferDesc leaving dwSize alone; SetRenderTarget
 * to a depth buffer and to a surface without DDSCAPS_3DDEVICE, and the viewport after it; cube
 * map faces without DDSCAPS2_CUBEMAP; and two pictures: linear vertex fog of untransformed
 * vertices, and untransformed vertices past the far plane (clipping on and off).
 * libs: -lddraw -ldxguid -lgdi32
 * arch: 32 (DirectDraw games are 32-bit; sake has no x64 DirectDraw) */
#define COBJMACROS
#include "../probe.h"
#include <ddraw.h>
#include <d3d.h>

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

static IDirectDrawSurface7 *zbuffer(IDirectDraw7 *dd, DWORD mem, int bits, DWORD mask, int w, int h) {
    DDSURFACEDESC2 d = { sizeof d };
    d.dwFlags = DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT | DDSD_PIXELFORMAT;
    d.dwWidth = w;
    d.dwHeight = h;
    d.ddsCaps.dwCaps = DDSCAPS_ZBUFFER | mem;
    d.ddpfPixelFormat.dwSize = sizeof d.ddpfPixelFormat;
    d.ddpfPixelFormat.dwFlags = DDPF_ZBUFFER;
    d.ddpfPixelFormat.dwZBufferBitDepth = bits;
    d.ddpfPixelFormat.dwZBitMask = mask;
    IDirectDrawSurface7 *s = NULL;
    HRESULT hr = IDirectDraw7_CreateSurface(dd, &d, &s, NULL);
    printf("CreateSurface z%d %#lx\n", bits, hr);
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

static HRESULT WINAPI count_cb(IDirectDrawSurface7 *s, DDSURFACEDESC2 *d, void *ctx) {
    (*(int *)ctx)++;
    IDirectDrawSurface7_Release(s);
    return DDENUMRET_OK;
}

static int surfaces(IDirectDraw7 *dd) {
    int n = 0;
    HRESULT hr = IDirectDraw7_EnumSurfaces(dd, DDENUMSURFACES_DOESEXIST | DDENUMSURFACES_ALL, NULL, &n, count_cb);
    if (FAILED(hr)) printf("EnumSurfaces %#lx\n", hr);
    return n;
}

static HRESULT WINAPI dev_cb(char *desc, char *name, D3DDEVICEDESC7 *d, void *ctx) {
    (*(int *)ctx)++;
    return E_INVALIDARG; /* (any answer but DDENUMRET_OK stops it) */
}

static void lights(IDirect3DDevice7 *dev) {
    D3DLIGHT7 l;
    printf("LightEnable 20 %#lx\n", IDirect3DDevice7_LightEnable(dev, 20, TRUE));
    memset(&l, 0xcc, sizeof l);
    printf("GetLight 20 %#lx\n", IDirect3DDevice7_GetLight(dev, 20, &l));
    const float *f = (const float *)&l.dcvDiffuse;
    printf("  type %d", l.dltType);
    for (int i = 0; i < 25; i++) printf(" %g", f[i]);
    printf("\n");
    printf("GetLight 21 (never set) %#lx\n", IDirect3DDevice7_GetLight(dev, 21, &l));
    float zero = 0, one = 1;
    struct { const char *what; D3DLIGHTTYPE type; float att0; } cases[] = {
        { "type 0", 0, 0 }, { "type 12345", 12345, 0 }, { "point att0 -1", D3DLIGHT_POINT, -1 },
        { "spot att0 -inf", D3DLIGHT_SPOT, -one / zero }, { "spot att0 0", D3DLIGHT_SPOT, 0 },
        { "spot att0 +inf", D3DLIGHT_SPOT, one / zero }, { "spot att0 nan", D3DLIGHT_SPOT, zero / zero },
        { "directional att0 -1", D3DLIGHT_DIRECTIONAL, -1 },
    };
    for (unsigned i = 0; i < sizeof cases / sizeof *cases; i++) {
        memset(&l, 0, sizeof l);
        l.dltType = cases[i].type;
        l.dcvDiffuse.r = l.dcvDiffuse.g = l.dcvDiffuse.b = 1;
        l.dvDirection.z = 1;
        l.dvAttenuation0 = cases[i].att0;
        printf("SetLight %s %#lx\n", cases[i].what, IDirect3DDevice7_SetLight(dev, 100 + i, &l));
    }
    printf("SetLight NULL %#lx\n", IDirect3DDevice7_SetLight(dev, 120, NULL));
}

static void scenes(IDirect3DDevice7 *dev) {
    printf("EndScene first %#lx\n", IDirect3DDevice7_EndScene(dev));
    printf("BeginScene %#lx\n", IDirect3DDevice7_BeginScene(dev));
    printf("BeginScene again %#lx\n", IDirect3DDevice7_BeginScene(dev));
    printf("EndScene %#lx\n", IDirect3DDevice7_EndScene(dev));
    printf("EndScene again %#lx\n", IDirect3DDevice7_EndScene(dev));
}

struct uv { float x, y, z; DWORD c, s; };

static void quad(IDirect3DDevice7 *dev, float x0, float y0, float x1, float y1, float z, DWORD c, DWORD s) {
    struct uv q[4] = { { x0, y0, z, c, s }, { x1, y0, z, c, s }, { x0, y1, z, c, s }, { x1, y1, z, c, s } };
    HRESULT hr = IDirect3DDevice7_DrawPrimitive(dev, D3DPT_TRIANGLESTRIP, D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_SPECULAR, q, 4, 0);
    if (FAILED(hr)) printf("DrawPrimitive %#lx\n", hr);
}

static void pictures(IDirect3DDevice7 *dev, IDirectDrawSurface7 *rt) {
    float start = 0, end = 1;
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_LIGHTING, FALSE);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_ZENABLE, FALSE);
    /* linear vertex fog of untransformed vertices: identity transforms, z 0.25 / 0.5 / 0.75 / 1.5
     * (eye depth = z); then FOGVERTEXMODE NONE: the specular alpha is the fog factor */
    IDirect3DDevice7_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0xff000000, 1, 0);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_FOGENABLE, TRUE);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_FOGCOLOR, 0xff00ff00);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_FOGSTART, *(DWORD *)&start);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_FOGEND, *(DWORD *)&end);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_FOGTABLEMODE, D3DFOG_NONE);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_FOGVERTEXMODE, D3DFOG_LINEAR);
    IDirect3DDevice7_BeginScene(dev);
    quad(dev, -1, 1, -0.5f, -1, 0.25f, 0xffff0000, 0xff000000);
    quad(dev, -0.5f, 1, 0, -1, 0.5f, 0xffff0000, 0xff000000);
    quad(dev, 0, 1, 0.5f, -1, 0.75f, 0xffff0000, 0xff000000);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_FOGVERTEXMODE, D3DFOG_NONE);
    quad(dev, 0.5f, 1, 1, 0, 0.5f, 0xffff0000, 0x80000000);
    quad(dev, 0.5f, 0, 1, -1, 0.5f, 0xffff0000, 0x00000000);
    IDirect3DDevice7_EndScene(dev);
    show("fog", rt);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_FOGENABLE, FALSE);

    /* past the far plane (z 2 with an identity projection): left half clipping on, right half
     * off; the middle stripe at z 0.5 is drawn */
    IDirect3DDevice7_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0xff000080, 1, 0);
    IDirect3DDevice7_BeginScene(dev);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_CLIPPING, TRUE);
    quad(dev, -1, 1, 0, -1, 2, 0xffff0000, 0);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_CLIPPING, FALSE);
    quad(dev, 0, 1, 1, -1, 2, 0xffff0000, 0);
    quad(dev, -1, 0.25f, 1, -0.25f, 0.5f, 0xffffffff, 0);
    IDirect3DDevice7_SetRenderState(dev, D3DRENDERSTATE_CLIPPING, TRUE);
    IDirect3DDevice7_EndScene(dev);
    show("far", rt);
}

static void zbuffers(IDirectDraw7 *dd, DWORD mem) {
    IDirectDrawSurface7 *z = zbuffer(dd, mem, 24, 0xffffff, 64, 4);
    if (z) {
        DDSURFACEDESC2 d = { sizeof d };
        IDirectDrawSurface7_GetSurfaceDesc(z, &d);
        printf("z24 pitch %ld depth %lu mask %#lx flags %#lx\n", d.lPitch, d.ddpfPixelFormat.dwZBufferBitDepth,
               d.ddpfPixelFormat.dwZBitMask, d.ddpfPixelFormat.dwFlags);
        IDirectDrawSurface7_Release(z);
    }
    z = zbuffer(dd, mem, 16, 0xffff, 8, 8);
    if (z) {
        DDBLTFX fx = { sizeof fx };
        fx.dwFillDepth = 0x1234;
        RECT r = { 2, 2, 6, 6 };
        printf("DEPTHFILL %#lx\n", IDirectDrawSurface7_Blt(z, &r, NULL, NULL, DDBLT_DEPTHFILL | DDBLT_WAIT, &fx));
        DDSURFACEDESC2 d = { sizeof d };
        if (SUCCEEDED(IDirectDrawSurface7_Lock(z, NULL, &d, DDLOCK_WAIT | DDLOCK_READONLY, NULL))) {
            const WORD *row = (const WORD *)((const char *)d.lpSurface + 3 * d.lPitch);
            printf("depth row 3: %04x %04x %04x %04x\n", row[1], row[2], row[5], row[6]);
            IDirectDrawSurface7_Unlock(z, NULL);
        }
        IDirectDrawSurface7_Release(z);
    }
    IDirectDrawSurface7 *s = surface(dd, DDSCAPS_OFFSCREENPLAIN | mem, 8, 8);
    if (s) {
        DDBLTFX fx = { sizeof fx };
        printf("DEPTHFILL of a plain surface %#lx\n", IDirectDrawSurface7_Blt(s, NULL, NULL, NULL, DDBLT_DEPTHFILL | DDBLT_WAIT, &fx));
        IDirectDrawSurface7_Release(s);
    }
}

static void device(HWND hwnd) {
    IDirectDraw7 *dd = NULL;
    if (FAILED(DirectDrawCreateEx(NULL, (void **)&dd, &IID_IDirectDraw7, NULL))) return;
    IDirectDraw7_SetCooperativeLevel(dd, hwnd, DDSCL_NORMAL);
    IDirect3D7 *d3d = NULL;
    if (FAILED(IDirectDraw7_QueryInterface(dd, &IID_IDirect3D7, (void **)&d3d))) { puts("no IDirect3D7"); return; }
    printf("EnumDevices NULL %#lx\n", IDirect3D7_EnumDevices(d3d, NULL, NULL));
    int n = 0;
    HRESULT ehr = IDirect3D7_EnumDevices(d3d, dev_cb, &n);
    printf("EnumDevices stop %#lx n %d\n", ehr, n);
    printf("surfaces at first %d\n", surfaces(dd));
    /* a software device on system-memory surfaces where there is one, else the HAL on video
     * memory (as ddrefs) */
    const GUID *devs[2] = { &IID_IDirect3DRGBDevice, &IID_IDirect3DHALDevice };
    DWORD mems[2] = { DDSCAPS_SYSTEMMEMORY, DDSCAPS_VIDEOMEMORY };
    IDirect3DDevice7 *dev = NULL;
    IDirectDrawSurface7 *rt = NULL;
    DWORD mem = 0;
    for (int i = 0; i < 2 && !dev; i++) {
        rt = surface(dd, DDSCAPS_OFFSCREENPLAIN | DDSCAPS_3DDEVICE | mems[i], 32, 32);
        if (!rt) continue;
        mem = mems[i];
        if (FAILED(IDirect3D7_CreateDevice(d3d, devs[i], rt, &dev))) {
            IDirectDrawSurface7_Release(rt);
            rt = NULL;
        }
    }
    if (!dev) { puts("no Direct3D 7 device"); return; }
    puts("CreateDevice ok");
    printf("surfaces with a render target %d\n", surfaces(dd));
    printf("GetCaps NULL %#lx\n", IDirect3DDevice7_GetCaps(dev, NULL));
    printf("SetMaterial NULL %#lx\n", IDirect3DDevice7_SetMaterial(dev, NULL));
    scenes(dev);
    lights(dev);
    zbuffers(dd, mem);

    D3DVERTEXBUFFERDESC vd = { sizeof vd, mem == DDSCAPS_SYSTEMMEMORY ? D3DVBCAPS_SYSTEMMEMORY : 0, D3DFVF_XYZ, 7 };
    IDirect3DVertexBuffer7 *vb = NULL;
    printf("CreateVertexBuffer %#lx\n", IDirect3D7_CreateVertexBuffer(d3d, &vd, &vb, 0));
    if (vb) {
        DWORD sizes[3] = { 0, sizeof vd, sizeof vd * 2 };
        for (int i = 0; i < 3; i++) {
            union { D3DVERTEXBUFFERDESC d; unsigned char b[64]; } m;
            memset(&m, 0x12, sizeof m);
            m.d.dwSize = sizes[i];
            HRESULT hr = IDirect3DVertexBuffer7_GetVertexBufferDesc(vb, &m.d);
            printf("GetVertexBufferDesc size %lu: %#lx size %lu fvf %#lx n %lu after %#x\n", sizes[i], hr, m.d.dwSize, m.d.dwFVF,
                   m.d.dwNumVertices, m.b[sizeof vd]);
        }
        IDirect3DVertexBuffer7_Release(vb);
    }

    D3DVIEWPORT7 vp = { 0, 0, 32, 32, 0, 1 };
    IDirect3DDevice7_SetViewport(dev, &vp);
    pictures(dev, rt);

    /* SetRenderTarget: a depth buffer (of DDSCAPS_3DDEVICE) is taken but refused; a surface without 3DDEVICE is not taken */
    D3DVIEWPORT7 vp2 = { 2, 3, 20, 21, 0.25f, 0.75f };
    IDirect3DDevice7_SetViewport(dev, &vp2);
    IDirectDrawSurface7 *z = zbuffer(dd, mem | DDSCAPS_3DDEVICE, 16, 0xffff, 16, 16);
    IDirectDrawSurface7 *plain = surface(dd, DDSCAPS_OFFSCREENPLAIN | mem, 16, 16);
    IDirectDrawSurface7 *rt2 = surface(dd, DDSCAPS_OFFSCREENPLAIN | DDSCAPS_3DDEVICE | mem, 16, 16);
    IDirectDrawSurface7 *cur = NULL;
    if (plain) {
        printf("SetRenderTarget plain %#lx\n", IDirect3DDevice7_SetRenderTarget(dev, plain, 0));
        IDirect3DDevice7_GetRenderTarget(dev, &cur);
        printf("  is plain %d is rt %d\n", cur == plain, cur == rt);
        if (cur) IDirectDrawSurface7_Release(cur);
    }
    if (z) {
        printf("SetRenderTarget depth buffer %#lx\n", IDirect3DDevice7_SetRenderTarget(dev, z, 0));
        cur = NULL;
        IDirect3DDevice7_GetRenderTarget(dev, &cur);
        printf("  is z %d is rt %d\n", cur == z, cur == rt);
        if (cur) IDirectDrawSurface7_Release(cur);
    }
    if (rt2) {
        printf("SetRenderTarget rt2 %#lx\n", IDirect3DDevice7_SetRenderTarget(dev, rt2, 0));
        D3DVIEWPORT7 g;
        memset(&g, 0xff, sizeof g);
        IDirect3DDevice7_GetViewport(dev, &g);
        printf("  viewport %lu %lu %lu %lu %g %g\n", g.dwX, g.dwY, g.dwWidth, g.dwHeight, g.dvMinZ, g.dvMaxZ);
        IDirect3DDevice7_SetRenderTarget(dev, rt, 0);
    }
    if (z) IDirectDrawSurface7_Release(z);
    if (plain) IDirectDrawSurface7_Release(plain);
    if (rt2) IDirectDrawSurface7_Release(rt2);

    /* cube map faces without DDSCAPS2_CUBEMAP */
    DDSURFACEDESC2 cd = { sizeof cd };
    cd.dwFlags = DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT;
    cd.dwWidth = cd.dwHeight = 16;
    cd.ddsCaps.dwCaps = DDSCAPS_TEXTURE | DDSCAPS_SYSTEMMEMORY;
    cd.ddsCaps.dwCaps2 = DDSCAPS2_CUBEMAP_POSITIVEX;
    IDirectDrawSurface7 *cube = NULL;
    printf("CreateSurface faces without CUBEMAP %#lx\n", IDirectDraw7_CreateSurface(dd, &cd, &cube, NULL));
    if (cube) IDirectDrawSurface7_Release(cube);

    IDirect3DDevice7_Release(dev);
    IDirectDrawSurface7_Release(rt);
    printf("surfaces at the end %d\n", surfaces(dd));
    IDirect3D7_Release(d3d);
    IDirectDraw7_Release(dd);
}

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "d3d7state", WS_OVERLAPPEDWINDOW, 0, 0, 320, 240, 0, 0, wc.hInstance, 0);
    device(hwnd);
    return 0;
}
