/* D3DX 9 loading from resources (d3dx9_43), beyond d3dx9effect's by-name cases: an effect in
 * RT_RCDATA by a lower-case name, by number and by "#7", and D3DXGetImageInfoFromResource on a
 * .bmp in RT_RCDATA (and one that isn't there). The ExHIBIT engine (人妻ネトリシェアリング's
 * trial) keeps its compiled effects as RCDATA and draws nothing without them.
 * Resources: d3dx9resource.rc. libs: -ld3dx9_43 */
#define COBJMACROS
#include <windows.h>
#include <d3d9.h>
#include <d3dx9.h>
#include <stdio.h>

/* a stand-in IDirect3DDevice9: only the slots effects use */
static ULONG WINAPI addref(void *d) { return 2; }
static ULONG WINAPI release(void *d) { return 1; }
static void *ovt[8] = { 0, addref, release };
static struct { void **vt; } sb = { ovt };
static HRESULT WINAPI apply(void *b) { return S_OK; }
static HRESULT WINAPI create_sb(void *d, D3DSTATEBLOCKTYPE t, void **out) { *out = &sb; ovt[5] = apply; return S_OK; }
static HRESULT WINAPI set_rs(void *d, D3DRENDERSTATETYPE s, DWORD v) { printf("  SetRenderState %u %lu\n", s, v); return S_OK; }
static void *vt[120] = { 0, addref, release };
static struct { void **vt; } dev = { vt };

static void show(const char *what, HRESULT hr, ID3DXEffect *fx)
{
    D3DXTECHNIQUE_DESC td;
    D3DXHANDLE t;
    LPCSTR s = "";
    UINT passes, i;
    printf("%s: %08lx, effect %s\n", what, hr, fx ? "given" : "NULL");
    if (FAILED(hr) || !fx) return;
    t = fx->lpVtbl->GetTechnique(fx, 0);
    fx->lpVtbl->GetTechniqueDesc(fx, t, &td);
    fx->lpVtbl->GetString(fx, fx->lpVtbl->GetAnnotationByName(fx, t, "Desc"), &s);
    printf("  technique %s, %u passes, Desc %s\n", td.Name, td.Passes, s);
    fx->lpVtbl->SetTechnique(fx, t);
    fx->lpVtbl->Begin(fx, &passes, D3DXFX_DONOTSAVESTATE);
    for (i = 0; i < passes; i++) {
        printf("  pass %u\n", i);
        fx->lpVtbl->BeginPass(fx, i);
        fx->lpVtbl->EndPass(fx);
    }
    fx->lpVtbl->End(fx);
    fx->lpVtbl->Release(fx);
}

int main(void)
{
    ID3DXEffect *fx;
    D3DXIMAGE_INFO info;
    HRESULT hr;
    vt[57] = set_rs; vt[59] = create_sb;

    fx = NULL;
    hr = D3DXCreateEffectFromResourceW((IDirect3DDevice9 *)&dev, GetModuleHandleW(NULL), L"fx", NULL, NULL, 0, NULL, &fx, NULL);
    show("by name in lower case (W), own module", hr, fx);
    fx = NULL;
    hr = D3DXCreateEffectFromResourceA((IDirect3DDevice9 *)&dev, NULL, MAKEINTRESOURCEA(7), NULL, NULL, 0, NULL, &fx, NULL);
    show("by number", hr, fx);
    fx = NULL;
    hr = D3DXCreateEffectFromResourceA((IDirect3DDevice9 *)&dev, NULL, "#7", NULL, NULL, 0, NULL, &fx, NULL);
    show("by \"#7\"", hr, fx);
    memset(&info, 0xcc, sizeof(info));
    hr = D3DXGetImageInfoFromResourceA(NULL, "PIC", &info);
    /* (not Format: Windows says D3DFMT_R8G8B8 for a 24-bit BMP, sake A8R8G8B8 for every image) */
    printf("image info: %08lx %ux%u depth %u levels %u type %u file format %u\n", hr, info.Width, info.Height,
           info.Depth, info.MipLevels, info.ResourceType, info.ImageFileFormat);
    hr = D3DXGetImageInfoFromResourceA(NULL, "NOPE", &info);
    printf("image info, missing: %08lx\n", hr);
    return 0;
}
