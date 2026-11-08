/* replay.c's D3D11 pictures: the swap chain's buffer before each Present (f<n>), a texture the
 * recording mapped to read (m<call> and m<call>_alpha: the part last copied into it, as the
 * 1x1 region a probe reads one pixel through), a buffer's first bytes (data m<call>), the render
 * target after each draw (-draws: d<call>) */
#define COBJMACROS
#include <stdio.h>
#include <d3d11.h>
#include "replay.h"

static int fmt_of(DXGI_FORMAT f) {
    switch (f) {
    case DXGI_FORMAT_R8G8B8A8_TYPELESS: case DXGI_FORMAT_R8G8B8A8_UNORM: case DXGI_FORMAT_R8G8B8A8_UNORM_SRGB:
    case DXGI_FORMAT_R8G8B8A8_UINT:
        return IMG_RGBX;
    case DXGI_FORMAT_B8G8R8A8_TYPELESS: case DXGI_FORMAT_B8G8R8A8_UNORM: case DXGI_FORMAT_B8G8R8A8_UNORM_SRGB:
    case DXGI_FORMAT_B8G8R8X8_TYPELESS: case DXGI_FORMAT_B8G8R8X8_UNORM: case DXGI_FORMAT_B8G8R8X8_UNORM_SRGB:
        return IMG_BGRX;
    case DXGI_FORMAT_B5G6R5_UNORM: return IMG_565;
    case DXGI_FORMAT_B5G5R5A1_UNORM: return IMG_X555;
    default: return -1;
    }
}

static void dump_mapped(const char *name, DXGI_FORMAT f, const BYTE *p, int w, int h, int pitch, int alpha) {
    int k = fmt_of(f);
    if (k < 0) { printf("image %s: format %u\n", name, (unsigned)f); return; }
    image_out(name, p, w, h, pitch, k);
    if (alpha && (k == IMG_RGBX || k == IMG_BGRX) && f != DXGI_FORMAT_B8G8R8X8_UNORM && f != DXGI_FORMAT_B8G8R8X8_TYPELESS && f != DXGI_FORMAT_B8G8R8X8_UNORM_SRGB)
        image_alpha(name, p, w, h, pitch);
}

/* a 2D texture's first level, through a staging copy (resolved first if multisampled) */
static void dump_texture(const char *name, ID3D11Texture2D *t) {
    D3D11_TEXTURE2D_DESC d, sd;
    ID3D11Device *dev = NULL;
    ID3D11DeviceContext *ctx = NULL;
    ID3D11Texture2D *plain = NULL, *stage = NULL;
    D3D11_MAPPED_SUBRESOURCE m;
    if (opt_noimages) return;
    ID3D11Texture2D_GetDesc(t, &d);
    ID3D11Texture2D_GetDevice(t, &dev);
    ID3D11Device_GetImmediateContext(dev, &ctx);
    sd = d;
    sd.MipLevels = sd.ArraySize = 1;
    sd.SampleDesc.Count = 1;
    sd.SampleDesc.Quality = 0;
    sd.MiscFlags = 0;
    ID3D11Resource *src = (ID3D11Resource *)t;
    if (d.SampleDesc.Count > 1) {
        sd.Usage = D3D11_USAGE_DEFAULT;
        sd.BindFlags = 0;
        sd.CPUAccessFlags = 0;
        if (SUCCEEDED(ID3D11Device_CreateTexture2D(dev, &sd, NULL, &plain))) {
            ID3D11DeviceContext_ResolveSubresource(ctx, (ID3D11Resource *)plain, 0, src, 0, d.Format);
            src = (ID3D11Resource *)plain;
        }
    }
    sd.Usage = D3D11_USAGE_STAGING;
    sd.BindFlags = 0;
    sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    HRESULT hr = ID3D11Device_CreateTexture2D(dev, &sd, NULL, &stage);
    if (FAILED(hr)) printf("image %s: CreateTexture2D %#lx\n", name, hr);
    else {
        ID3D11DeviceContext_CopySubresourceRegion(ctx, (ID3D11Resource *)stage, 0, 0, 0, 0, src, 0, NULL);
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)stage, 0, D3D11_MAP_READ, 0, &m);
        if (FAILED(hr)) printf("image %s: Map %#lx\n", name, hr);
        else {
            dump_mapped(name, d.Format, m.pData, d.Width, d.Height, m.RowPitch, 0);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)stage, 0);
        }
        ID3D11Texture2D_Release(stage);
    }
    if (plain) ID3D11Texture2D_Release(plain);
    ID3D11DeviceContext_Release(ctx);
    ID3D11Device_Release(dev);
}

/* the part of a resource last copied into it (sub ~0: the whole resource by CopyResource; w 0:
 * all of the subresource), one per resource: the latest copy */
static struct copied { void *res; UINT sub, x, y, w, h; } copies[64];
static int ncopies, copies_next;
static void copied(void *res, UINT sub, UINT x, UINT y, UINT w, UINT h) {
    int i;
    for (i = 0; i < ncopies; i++) if (copies[i].res == res) break;
    if (i == ncopies) {
        if (ncopies < 64) ncopies++;
        else i = copies_next++ % 64;
    }
    copies[i] = (struct copied){ res, sub, x, y, w, h };
}

void d3d11_before(const char *iface, const char *method, W *a, int n, UINT no) {
    char name[32];
    if (strncmp(iface, "IDXGISwapChain", 14) || (strcmp(method, "Present") && strcmp(method, "Present1"))) return;
    if (frame_wanted()) {
        ID3D11Texture2D *t = NULL;
        sprintf(name, "f%d", frame);
        HRESULT hr = IDXGISwapChain_GetBuffer((IDXGISwapChain *)a[0], 0, &IID_ID3D11Texture2D, (void **)&t);
        if (FAILED(hr)) printf("image %s: GetBuffer %#lx\n", name, hr);
        else {
            dump_texture(name, t);
            ID3D11Texture2D_Release(t);
        }
    }
    frame++;
}

void d3d11_after(const char *iface, const char *method, W *a, int n, UINT no, HRESULT hr) {
    char img[32];
    if (strncmp(iface, "ID3D11DeviceContext", 19)) return;
    if (!strcmp(method, "CopyResource") && n >= 3) {
        copied((void *)a[1], ~0u, 0, 0, 0, 0);
    } else if (!strcmp(method, "CopySubresourceRegion") && n >= 9) {
        const D3D11_BOX *b = (const D3D11_BOX *)a[8];
        copied((void *)a[1], (UINT)a[2], (UINT)a[3], (UINT)a[4], b ? b->right - b->left : 0, b ? b->bottom - b->top : 0);
    } else if (!strcmp(method, "Map") && n >= 6 && SUCCEEDED(hr) && ((UINT)a[3] == D3D11_MAP_READ || (UINT)a[3] == D3D11_MAP_READ_WRITE) && read_wanted()) {
        ID3D11Resource *r = (ID3D11Resource *)a[1];
        UINT sub = (UINT)a[2];
        D3D11_MAPPED_SUBRESOURCE *m = (D3D11_MAPPED_SUBRESOURCE *)a[5];
        D3D11_RESOURCE_DIMENSION dim;
        if (opt_noimages || !m || !m->pData) return;
        ID3D11Resource_GetType(r, &dim);
        sprintf(img, "m%u", no);
        if (dim == D3D11_RESOURCE_DIMENSION_BUFFER) {
            D3D11_BUFFER_DESC d;
            ID3D11Buffer_GetDesc((ID3D11Buffer *)r, &d);
            UINT k = d.ByteWidth < 256 ? d.ByteWidth : 256;
            printf("data %s", img);
            for (UINT i = 0; i < k; i++) printf("%s%02x", i % 4 ? "" : " ", ((BYTE *)m->pData)[i]);
            printf("\n");
        } else if (dim == D3D11_RESOURCE_DIMENSION_TEXTURE2D) {
            D3D11_TEXTURE2D_DESC d;
            ID3D11Texture2D_GetDesc((ID3D11Texture2D *)r, &d);
            UINT mip = sub % (d.MipLevels ? d.MipLevels : 1);
            UINT w = d.Width >> mip, h = d.Height >> mip, x = 0, y = 0;
            if (!w) w = 1;
            if (!h) h = 1;
            for (int i = 0; i < ncopies; i++)
                if (copies[i].res == r && copies[i].sub == sub && copies[i].w) {
                    x = copies[i].x; y = copies[i].y;
                    if (x < w && y < h) {
                        w = copies[i].w < w - x ? copies[i].w : w - x;
                        h = copies[i].h < h - y ? copies[i].h : h - y;
                    } else x = y = 0;
                    break;
                }
            int bpp = fmt_of(d.Format) == IMG_565 || fmt_of(d.Format) == IMG_X555 ? 2 : 4;
            /* (the whole picture with its alpha; one pixel read back is a value, alone) */
            dump_mapped(img, d.Format, (BYTE *)m->pData + (size_t)y * m->RowPitch + x * bpp, w, h, m->RowPitch, w > 1 || h > 1);
        } else {
            printf("image %s: dimension %u\n", img, (unsigned)dim);
        }
    } else if (draws_wanted() && !strncmp(method, "Draw", 4)) {
        ID3D11RenderTargetView *v = NULL;
        ID3D11DeviceContext_OMGetRenderTargets((ID3D11DeviceContext *)a[0], 1, &v, NULL);
        if (v) {
            ID3D11Resource *r = NULL;
            D3D11_RESOURCE_DIMENSION dim;
            ID3D11RenderTargetView_GetResource(v, &r);
            ID3D11Resource_GetType(r, &dim);
            sprintf(img, "d%u", no);
            if (dim == D3D11_RESOURCE_DIMENSION_TEXTURE2D) dump_texture(img, (ID3D11Texture2D *)r);
            ID3D11Resource_Release(r);
            ID3D11RenderTargetView_Release(v);
        }
    }
}
