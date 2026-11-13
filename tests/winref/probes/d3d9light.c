/* Direct3D 9 fixed-function lighting: the vertex colors the lighting equation gives (material
 * diffuse / ambient / specular / emissive, the AMBIENT render state, directional, point and spot
 * lights with range, attenuation and cone falloff, a face turned from the light, specular with
 * and without LOCALVIEWER and SPECULARENABLE, COLORVERTEX and the material sources, a vertex
 * without a normal, NORMALIZENORMALS under a scaled world). Each case draws one quad facing the
 * camera, lit the same at its four corners, and prints the middle pixel.
 * (小悪魔ディーラーのキャンディ体験版 draws its sprites lit: diffuse 0, emissive white.)
 * libs: -ld3d9
 * arch: 32
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <math.h>
#include <d3d9.h>

#define W 8
#define H 8

static IDirect3DDevice9 *dev;
static IDirect3DSurface9 *back, *bsys;

/* position, normal, diffuse, specular */
struct v { float x, y, z, nx, ny, nz; DWORD c0, c1; };

static void show(const char *name) {
    D3DLOCKED_RECT lr;
    if (FAILED(IDirect3DDevice9_GetRenderTargetData(dev, back, bsys))) { printf("%s: GetRenderTargetData failed\n", name); return; }
    if (FAILED(IDirect3DSurface9_LockRect(bsys, &lr, NULL, D3DLOCK_READONLY))) { printf("%s: LockRect failed\n", name); return; }
    probe_image(name, (char *)lr.pBits + 4 * lr.Pitch + 4 * 4, 1, 1, lr.Pitch, 3);
    IDirect3DSurface9_UnlockRect(bsys);
}

/* the quad x, y in -1..1 at z = 2 (normal toward the camera, -z), drawn with fvf */
static void quad(const char *name, DWORD fvf, DWORD c0, DWORD c1) {
    struct v q[4] = {
        { -1, -1, 2, 0, 0, -1, c0, c1 }, { -1, 1, 2, 0, 0, -1, c0, c1 }, { 1, -1, 2, 0, 0, -1, c0, c1 }, { 1, 1, 2, 0, 0, -1, c0, c1 },
    };
    /* the vertex layout fvf asks for, packed */
    float buf[4 * 8];
    int n = 0;
    for (int i = 0; i < 4; i++) {
        buf[n++] = q[i].x; buf[n++] = q[i].y; buf[n++] = q[i].z;
        if (fvf & D3DFVF_NORMAL) { buf[n++] = q[i].nx; buf[n++] = q[i].ny; buf[n++] = q[i].nz; }
        if (fvf & D3DFVF_DIFFUSE) memcpy(&buf[n++], &q[i].c0, 4);
        if (fvf & D3DFVF_SPECULAR) memcpy(&buf[n++], &q[i].c1, 4);
    }
    IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0x123456, 1, 0);
    IDirect3DDevice9_BeginScene(dev);
    IDirect3DDevice9_SetFVF(dev, fvf);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, buf, n / 4 * 4);
    IDirect3DDevice9_EndScene(dev);
    show(name);
}

static D3DCOLORVALUE cv(float r, float g, float b, float a) { D3DCOLORVALUE c = { r, g, b, a }; return c; }

static void material(D3DCOLORVALUE d, D3DCOLORVALUE a, D3DCOLORVALUE s, D3DCOLORVALUE e, float power) {
    D3DMATERIAL9 m = { d, a, s, e, power };
    IDirect3DDevice9_SetMaterial(dev, &m);
}

static void light(DWORD i, D3DLIGHTTYPE t, D3DCOLORVALUE d, D3DCOLORVALUE s, D3DCOLORVALUE a, D3DVECTOR pos, D3DVECTOR dir, float range, float a0, float a1, float a2, float falloff, float theta, float phi) {
    D3DLIGHT9 l;
    memset(&l, 0, sizeof l);
    l.Type = t; l.Diffuse = d; l.Specular = s; l.Ambient = a; l.Position = pos; l.Direction = dir;
    l.Range = range; l.Attenuation0 = a0; l.Attenuation1 = a1; l.Attenuation2 = a2; l.Falloff = falloff; l.Theta = theta; l.Phi = phi;
    HRESULT hr = IDirect3DDevice9_SetLight(dev, i, &l);
    if (FAILED(hr)) printf("SetLight %lu %#lx\n", i, hr);
    IDirect3DDevice9_LightEnable(dev, i, TRUE);
}

static void off(void) {
    for (DWORD i = 0; i < 8; i++) IDirect3DDevice9_LightEnable(dev, i, FALSE);
}

static D3DVECTOR vec(float x, float y, float z) { D3DVECTOR v = { x, y, z }; return v; }

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "d3d9light", WS_POPUP, 0, 0, W, H, 0, 0, wc.hInstance, 0);
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) { puts("skip no Direct3D 9"); return 0; }
    D3DPRESENT_PARAMETERS pp = { 0 };
    pp.BackBufferWidth = W; pp.BackBufferHeight = H; pp.BackBufferFormat = D3DFMT_X8R8G8B8; pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD; pp.hDeviceWindow = hwnd; pp.Windowed = TRUE;
    HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev);
    if (FAILED(hr)) { printf("skip CreateDevice %#lx\n", hr); return 0; }
    D3DCAPS9 caps;
    IDirect3DDevice9_GetDeviceCaps(dev, &caps);
    printf("MaxActiveLights >= 8 %d\n", caps.MaxActiveLights >= 8);
    IDirect3DDevice9_GetBackBuffer(dev, 0, 0, D3DBACKBUFFER_TYPE_MONO, &back);
    IDirect3DDevice9_CreateOffscreenPlainSurface(dev, W, H, D3DFMT_X8R8G8B8, D3DPOOL_SYSTEMMEM, &bsys, NULL);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, FALSE);
    /* camera at the origin looking along +z; x, y -1..1 at z = 2 fill the screen (z / 4) */
    D3DMATRIX proj = { { { 0.5f, 0, 0, 0, 0, 0.5f, 0, 0, 0, 0, 0.25f, 0, 0, 0, 0, 1 } } };
    /* (w = 1: the quad's corners are at -0.5, 0.5: the middle pixel is inside) */
    IDirect3DDevice9_SetTransform(dev, D3DTS_PROJECTION, &proj);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_LIGHTING, TRUE);
    /* the stage: the diffuse color, then specular added (SPECULARENABLE) */
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLORARG1, D3DTA_DIFFUSE);
    const DWORD XN = D3DFVF_XYZ | D3DFVF_NORMAL;
    D3DCOLORVALUE z = cv(0, 0, 0, 0), white = cv(1, 1, 1, 1);

    /* the sprite trick: no diffuse, white emissive, one directional light, grey ambient */
    IDirect3DDevice9_SetRenderState(dev, D3DRS_AMBIENT, 0xff505050);
    material(cv(0, 0, 0, 0.5f), z, z, white, 0);
    light(0, D3DLIGHT_DIRECTIONAL, white, white, z, vec(0, 0, 0), vec(0.6667f, -0.6667f, 0.3333f), 0, 0, 0, 0, 0, 0, 0);
    quad("emissive", XN, 0, 0);
    /* no lights: emissive + AMBIENT * material ambient */
    off();
    IDirect3DDevice9_SetRenderState(dev, D3DRS_AMBIENT, 0xff406080);
    material(z, cv(1, 0.5f, 0.25f, 1), z, cv(0.1f, 0.2f, 0.3f, 1), 0);
    quad("ambient", XN, 0, 0);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_AMBIENT, 0);
    /* a directional light straight on, at 60 degrees, from behind (its ambient part only) */
    material(cv(0.5f, 0.5f, 0.5f, 1), cv(1, 1, 1, 1), z, z, 0);
    light(0, D3DLIGHT_DIRECTIONAL, cv(1, 0.5f, 0.25f, 1), z, z, vec(0, 0, 0), vec(0, 0, 1), 0, 0, 0, 0, 0, 0, 0);
    quad("directional", XN, 0, 0);
    light(0, D3DLIGHT_DIRECTIONAL, cv(1, 1, 1, 1), z, z, vec(0, 0, 0), vec(0.8660254f, 0, 0.5f), 0, 0, 0, 0, 0, 0, 0);
    quad("directional60", XN, 0, 0);
    light(0, D3DLIGHT_DIRECTIONAL, cv(1, 1, 1, 1), z, cv(0.2f, 0.3f, 0.4f, 1), vec(0, 0, 0), vec(0, 0, -1), 0, 0, 0, 0, 0, 0, 0);
    quad("behind", XN, 0, 0);
    /* two lights add up */
    light(0, D3DLIGHT_DIRECTIONAL, cv(0.25f, 0, 0.5f, 1), z, z, vec(0, 0, 0), vec(0, 0, 1), 0, 0, 0, 0, 0, 0, 0);
    light(3, D3DLIGHT_DIRECTIONAL, cv(0.5f, 0.25f, 0, 1), z, z, vec(0, 0, 0), vec(0, 0, 1), 0, 0, 0, 0, 0, 0, 0);
    quad("two", XN, 0, 0);
    off();
    /* a point light on the quad's axis, 2 in front of it: distance sqrt(6) to each corner */
    material(cv(1, 1, 1, 1), z, z, z, 0);
    light(0, D3DLIGHT_POINT, white, z, z, vec(0, 0, 0), vec(0, 0, 0), 10, 1, 0, 0, 0, 0, 0);
    quad("point", XN, 0, 0);
    light(0, D3DLIGHT_POINT, white, z, z, vec(0, 0, 0), vec(0, 0, 0), 10, 0, 0, 1, 0, 0, 0);
    quad("point_att2", XN, 0, 0);
    light(0, D3DLIGHT_POINT, white, z, z, vec(0, 0, 0), vec(0, 0, 0), 10, 0.5f, 0.25f, 0, 0, 0, 0);
    quad("point_att1", XN, 0, 0);
    light(0, D3DLIGHT_POINT, white, z, z, vec(0, 0, 0), vec(0, 0, 0), 2, 1, 0, 0, 0, 0, 0);
    quad("point_range", XN, 0, 0);
    /* a spot light from the camera: the corners 35.26 degrees off its axis */
    light(0, D3DLIGHT_SPOT, white, z, z, vec(0, 0, 0), vec(0, 0, 1), 10, 1, 0, 0, 1, 1.4f, 1.8f);
    quad("spot_inside", XN, 0, 0);
    light(0, D3DLIGHT_SPOT, white, z, z, vec(0, 0, 0), vec(0, 0, 1), 10, 1, 0, 0, 1, 0.4f, 1.6f);
    quad("spot_falloff1", XN, 0, 0);
    light(0, D3DLIGHT_SPOT, white, z, z, vec(0, 0, 0), vec(0, 0, 1), 10, 1, 0, 0, 2, 0.4f, 1.6f);
    quad("spot_falloff2", XN, 0, 0);
    light(0, D3DLIGHT_SPOT, white, z, z, vec(0, 0, 0), vec(0, 0, 1), 10, 1, 0, 0, 1, 0.2f, 0.6f);
    quad("spot_outside", XN, 0, 0);
    off();
    /* specular: a light from the camera's side, 30 degrees off the axis; power 8 */
    IDirect3DDevice9_SetRenderState(dev, D3DRS_SPECULARENABLE, TRUE);
    material(z, z, cv(1, 0.5f, 0.25f, 1), z, 8);
    light(0, D3DLIGHT_DIRECTIONAL, z, white, z, vec(0, 0, 0), vec(0.5f, 0, 0.8660254f), 0, 0, 0, 0, 0, 0, 0);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_LOCALVIEWER, FALSE);
    quad("specular", XN, 0, 0);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_LOCALVIEWER, TRUE);
    quad("specular_local", XN, 0, 0);
    light(0, D3DLIGHT_DIRECTIONAL, z, white, z, vec(0, 0, 0), vec(0, 0, 1), 0, 0, 0, 0, 0, 0, 0);
    quad("specular_on_axis_local", XN, 0, 0);
    material(z, z, cv(1, 0.5f, 0.25f, 1), z, 0);
    quad("specular_power0", XN, 0, 0);
    material(z, z, cv(1, 0.5f, 0.25f, 1), z, 8);
    light(0, D3DLIGHT_DIRECTIONAL, z, white, z, vec(0, 0, 0), vec(0, 0, -1), 0, 0, 0, 0, 0, 0, 0);
    quad("specular_behind", XN, 0, 0);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_SPECULARENABLE, FALSE);
    light(0, D3DLIGHT_DIRECTIONAL, z, white, z, vec(0, 0, 0), vec(0, 0, 1), 0, 0, 0, 0, 0, 0, 0);
    quad("specular_disabled", XN, 0, 0);
    off();
    /* the vertex's colors: diffuse from COLOR1 (the default), with COLORVERTEX off, and every
     * source swapped */
    material(cv(0, 1, 0, 1), cv(0, 0, 0, 1), z, z, 0);
    light(0, D3DLIGHT_DIRECTIONAL, white, z, z, vec(0, 0, 0), vec(0, 0, 1), 0, 0, 0, 0, 0, 0, 0);
    quad("colorvertex", XN | D3DFVF_DIFFUSE, 0xff804020, 0);
    quad("colorvertex_no_diffuse", XN, 0xff804020, 0);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_COLORVERTEX, FALSE);
    quad("colorvertex_off", XN | D3DFVF_DIFFUSE, 0xff804020, 0);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_COLORVERTEX, TRUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_AMBIENT, 0xff808080);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_DIFFUSEMATERIALSOURCE, D3DMCS_MATERIAL);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_AMBIENTMATERIALSOURCE, D3DMCS_COLOR1);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_EMISSIVEMATERIALSOURCE, D3DMCS_COLOR2);
    quad("sources", XN | D3DFVF_DIFFUSE | D3DFVF_SPECULAR, 0xff804020, 0xff102030);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_DIFFUSEMATERIALSOURCE, D3DMCS_COLOR1);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_AMBIENTMATERIALSOURCE, D3DMCS_MATERIAL);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_EMISSIVEMATERIALSOURCE, D3DMCS_MATERIAL);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_AMBIENT, 0);
    /* no normal: the light's diffuse part is gone, emissive stays */
    material(cv(1, 1, 1, 1), z, z, cv(0.25f, 0, 0, 1), 0);
    quad("no_normal", D3DFVF_XYZ, 0, 0);
    /* a world scaled by 0.5: the normal (0, 0, -1) becomes 2 long; NORMALIZENORMALS */
    D3DMATRIX world = { { { 0.5f, 0, 0, 0, 0, 0.5f, 0, 0, 0, 0, 0.5f, 0, 0, 0, 1, 1 } } };
    IDirect3DDevice9_SetTransform(dev, D3DTS_WORLD, &world);
    material(cv(0.3f, 0.3f, 0.3f, 1), z, z, z, 0);
    light(0, D3DLIGHT_DIRECTIONAL, white, z, z, vec(0, 0, 0), vec(0, 0, 1), 0, 0, 0, 0, 0, 0, 0);
    quad("scaled", XN, 0, 0);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_NORMALIZENORMALS, TRUE);
    quad("scaled_normalized", XN, 0, 0);
    puts("done");
    return 0;
}
