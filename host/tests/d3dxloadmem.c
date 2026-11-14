/* D3DXLoadSurfaceFromMemory into a stand-in surface (GetDesc / LockRect / UnlockRect over a
 * buffer): 天極姫 (Eushully's engine) puts every glyph it draws with GetGlyphOutline into its
 * text texture this way, A8R8G8B8 rows with a pitch of their own, and drew white boxes while
 * the function returned E_NOTIMPL. Cases: a copy into a destination rectangle (the rest
 * untouched), X8R8G8B8 to A8R8G8B8 (opaque), A8R8G8B8 to A8, a color key, and
 * the source rectangle missing (D3DERR_INVALIDCALL).
 * libs: -ld3dx9_43 */
#define COBJMACROS
#include <windows.h>
#include <d3d9.h>
#include <d3dx9.h>
#include <stdio.h>
#include <string.h>

static struct { void **vt; } surf;
static D3DFORMAT dfmt;
static UINT dw = 8, dh = 4, dpitch = 64;
static BYTE bits[64 * 8];
static int locks, unlocks;

static ULONG WINAPI addref(void *s) { return 2; }
static ULONG WINAPI release(void *s) { return 1; }
static HRESULT WINAPI qi(void *s, REFIID r, void **o) { *o = s; return S_OK; }
static HRESULT WINAPI get_desc(void *s, D3DSURFACE_DESC *d)
{
    memset(d, 0, sizeof(*d));
    d->Format = dfmt;
    d->Type = D3DRTYPE_SURFACE;
    d->Pool = D3DPOOL_SYSTEMMEM;
    d->Width = dw;
    d->Height = dh;
    return S_OK;
}
static HRESULT WINAPI lock_rect(void *s, D3DLOCKED_RECT *lr, const RECT *r, DWORD flags)
{
    UINT bpp = dfmt == D3DFMT_A8 ? 1 : 4;
    locks++;
    lr->Pitch = dpitch;
    lr->pBits = bits + (r ? r->top * dpitch + r->left * bpp : 0);
    return S_OK;
}
static HRESULT WINAPI unlock_rect(void *s) { unlocks++; return S_OK; }
static void *vt[20] = { qi, addref, release };

static void dump(const char *what, HRESULT hr, UINT bpp)
{
    UINT x, y;
    printf("%s: %08lx\n", what, hr);
    for (y = 0; y < dh; y++) {
        printf(" ");
        for (x = 0; x < dw; x++) {
            if (bpp == 4) printf(" %08lx", *(DWORD *)(bits + y * dpitch + 4 * x));
            else printf(" %02x", bits[y * dpitch + x]);
        }
        printf("\n");
    }
}

int main(void)
{
    /* a 3x2 glyph: rows 16 bytes apart (pitch wider than the row) */
    static DWORD src[2][4] = { { 0x00ffffff, 0x80ffffff, 0xffffffff, 0xdeadbeef },
                               { 0x40102030, 0xc0a0b0c0, 0x00000000, 0xdeadbeef } };
    RECT sr = { 0, 0, 3, 2 }, dr = { 2, 1, 5, 3 };
    HRESULT hr;
    vt[12] = get_desc; vt[13] = lock_rect; vt[14] = unlock_rect;
    surf.vt = vt;

    dfmt = D3DFMT_A8R8G8B8;
    memset(bits, 0x11, sizeof(bits));
    hr = D3DXLoadSurfaceFromMemory((IDirect3DSurface9 *)&surf, NULL, &dr, src, D3DFMT_A8R8G8B8, 16, NULL, &sr,
                                   D3DX_FILTER_NONE, 0);
    dump("A8R8G8B8 into (2,1)-(5,3)", hr, 4);
    printf("locks %d unlocks %d\n", locks, unlocks);

    memset(bits, 0x11, sizeof(bits));
    hr = D3DXLoadSurfaceFromMemory((IDirect3DSurface9 *)&surf, NULL, &dr, src, D3DFMT_X8R8G8B8, 16, NULL, &sr,
                                   D3DX_FILTER_NONE, 0);
    dump("X8R8G8B8 into A8R8G8B8", hr, 4);

    memset(bits, 0x11, sizeof(bits));
    hr = D3DXLoadSurfaceFromMemory((IDirect3DSurface9 *)&surf, NULL, &dr, src, D3DFMT_A8R8G8B8, 16, NULL, &sr,
                                   D3DX_FILTER_NONE, 0xffffffff);
    dump("color key ffffffff", hr, 4);

    dfmt = D3DFMT_A8;
    memset(bits, 0x11, sizeof(bits));
    hr = D3DXLoadSurfaceFromMemory((IDirect3DSurface9 *)&surf, NULL, &dr, src, D3DFMT_A8R8G8B8, 16, NULL, &sr,
                                   D3DX_FILTER_NONE, 0);
    dump("A8R8G8B8 into A8", hr, 1);

    dfmt = D3DFMT_A8R8G8B8;
    hr = D3DXLoadSurfaceFromMemory((IDirect3DSurface9 *)&surf, NULL, &dr, src, D3DFMT_A8R8G8B8, 16, NULL, NULL,
                                   D3DX_FILTER_NONE, 0);
    printf("no source rectangle: %08lx\n", hr);
    hr = D3DXLoadSurfaceFromMemory((IDirect3DSurface9 *)&surf, NULL, &dr, NULL, D3DFMT_A8R8G8B8, 16, NULL, &sr,
                                   D3DX_FILTER_NONE, 0);
    printf("no source memory: %08lx\n", hr);
    return 0;
}
