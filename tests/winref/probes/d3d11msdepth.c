/* Direct3D 11 multisampled depth textures of the depth formats and their typeless forms: which
 * CreateTexture2D takes at 1, 2, 4 and 8 samples with the bind flags DEPTH_STENCIL |
 * SHADER_RESOURCE, DEPTH_STENCIL, SHADER_RESOURCE and RENDER_TARGET, and the quality levels.
 * Unity 6 (キメセク!地雷ちゃん 体験版) makes a 2x R32G8X24_TYPELESS one bound for depth and
 * shader input; sake refused it with E_INVALIDARG.
 * libs: -ld3d11
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <d3d11.h>

int main(void) {
    ID3D11Device *dev;
    ID3D11DeviceContext *ctx;
    D3D_FEATURE_LEVEL want = D3D_FEATURE_LEVEL_11_0, got;
    HRESULT hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_WARP, NULL, 0, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) { printf("skip D3D11CreateDevice %#lx\n", hr); return 0; }
    /* the depth formats and their typeless and shader-readable relatives */
    static const DXGI_FORMAT fmts[] = { 19, 20, 21, 39, 40, 41, 44, 45, 46, 53, 55, 56 };
    static const UINT binds[] = { D3D11_BIND_DEPTH_STENCIL | D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_DEPTH_STENCIL,
                                  D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_RENDER_TARGET };
    static const UINT counts[] = { 1, 2, 4, 8 };
    for (int f = 0; f < (int)(sizeof fmts / sizeof *fmts); f++) {
        printf("fmt %d levels", fmts[f]);
        for (int c = 0; c < 4; c++) {
            UINT q = 0;
            hr = ID3D11Device_CheckMultisampleQualityLevels(dev, fmts[f], counts[c], &q);
            printf(" %u:%u", counts[c], SUCCEEDED(hr) ? q : 0);
        }
        printf("\n");
        for (int b = 0; b < 4; b++) {
            printf("  bind %#x:", binds[b]);
            for (int c = 0; c < 4; c++) {
                D3D11_TEXTURE2D_DESC d = { 540, 960, 1, 1, fmts[f], { counts[c], 0 }, D3D11_USAGE_DEFAULT, binds[b], 0, 0 };
                ID3D11Texture2D *t = NULL;
                hr = ID3D11Device_CreateTexture2D(dev, &d, NULL, &t);
                printf(" %u=%s", counts[c], hr == S_OK ? "ok" : hr == E_INVALIDARG ? "E_INVALIDARG" : "other");
                if (t) ID3D11Texture2D_Release(t);
            }
            printf("\n");
        }
    }
    ID3D11DeviceContext_Release(ctx);
    ID3D11Device_Release(dev);
    return 0;
}
