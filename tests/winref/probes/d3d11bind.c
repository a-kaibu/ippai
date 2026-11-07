/* Direct3D 11 bindings and state objects (formal/tla/Binds11.tla): a resource bound for output
 * (render target, depth, UAV, stream output) is unbound from every input slot that reads it,
 * an input bind of a resource bound for output is refused (the slot gets NULL), per
 * subresource; state objects of the same description are the same object; the references
 * the context's slots and Get* calls hold; ClearState. WARP or hardware on the Windows runner.
 * libs: -ld3d11
 * run: browser (sake's Direct3D 11 runs in the page: tests/winref/run-browser.mjs) */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <d3d11.h>

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;

static ULONG refs(void *o) {
    IUnknown_AddRef((IUnknown *)o);
    return IUnknown_Release((IUnknown *)o);
}

static ID3D11Texture2D *tex(UINT bind, UINT mips, DXGI_FORMAT f) {
    D3D11_TEXTURE2D_DESC d = { 16, 16, mips, 1, f, { 1, 0 }, D3D11_USAGE_DEFAULT, bind, 0, 0 };
    ID3D11Texture2D *t = NULL;
    if (FAILED(ID3D11Device_CreateTexture2D(dev, &d, NULL, &t))) printf("CreateTexture2D %#x failed\n", bind);
    return t;
}

static ID3D11ShaderResourceView *srv_mip(ID3D11Texture2D *t, DXGI_FORMAT f, UINT mip) {
    D3D11_SHADER_RESOURCE_VIEW_DESC d = { f, D3D11_SRV_DIMENSION_TEXTURE2D };
    d.Texture2D.MostDetailedMip = mip; d.Texture2D.MipLevels = 1;
    ID3D11ShaderResourceView *v = NULL;
    if (FAILED(ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)t, &d, &v))) printf("CreateShaderResourceView failed\n");
    return v;
}
static ID3D11RenderTargetView *rtv_mip(ID3D11Texture2D *t, UINT mip) {
    D3D11_RENDER_TARGET_VIEW_DESC d = { DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_RTV_DIMENSION_TEXTURE2D };
    d.Texture2D.MipSlice = mip;
    ID3D11RenderTargetView *v = NULL;
    if (FAILED(ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)t, &d, &v))) printf("CreateRenderTargetView failed\n");
    return v;
}

/* what the slots hold now: 1 the view, 0 NULL, x another */
static char ps_srv(UINT slot, void *want) {
    ID3D11ShaderResourceView *v = NULL;
    ID3D11DeviceContext_PSGetShaderResources(ctx, slot, 1, &v);
    if (v) ID3D11ShaderResourceView_Release(v);
    return v == want ? '1' : v ? 'x' : '0';
}
static char vs_srv(UINT slot, void *want) {
    ID3D11ShaderResourceView *v = NULL;
    ID3D11DeviceContext_VSGetShaderResources(ctx, slot, 1, &v);
    if (v) ID3D11ShaderResourceView_Release(v);
    return v == want ? '1' : v ? 'x' : '0';
}
static char cs_srv(UINT slot, void *want) {
    ID3D11ShaderResourceView *v = NULL;
    ID3D11DeviceContext_CSGetShaderResources(ctx, slot, 1, &v);
    if (v) ID3D11ShaderResourceView_Release(v);
    return v == want ? '1' : v ? 'x' : '0';
}
static char rt(UINT slot, void *want) {
    ID3D11RenderTargetView *v[2] = { 0 };
    ID3D11DepthStencilView *d = NULL;
    ID3D11DeviceContext_OMGetRenderTargets(ctx, 2, v, &d);
    for (int i = 0; i < 2; i++) if (v[i]) ID3D11RenderTargetView_Release(v[i]);
    if (d) ID3D11DepthStencilView_Release(d);
    return v[slot] == want ? '1' : v[slot] ? 'x' : '0';
}
static char ds(void *want) {
    ID3D11DepthStencilView *d = NULL;
    ID3D11DeviceContext_OMGetRenderTargets(ctx, 0, NULL, &d);
    if (d) ID3D11DepthStencilView_Release(d);
    return d == want ? '1' : d ? 'x' : '0';
}
static char vb(void *want) {
    ID3D11Buffer *b = NULL; UINT s, o;
    ID3D11DeviceContext_IAGetVertexBuffers(ctx, 0, 1, &b, &s, &o);
    if (b) ID3D11Buffer_Release(b);
    return b == want ? '1' : b ? 'x' : '0';
}
static char cb(void *want) {
    ID3D11Buffer *b = NULL;
    ID3D11DeviceContext_PSGetConstantBuffers(ctx, 0, 1, &b);
    if (b) ID3D11Buffer_Release(b);
    return b == want ? '1' : b ? 'x' : '0';
}
static char so(void *want) {
    ID3D11Buffer *b = NULL;
    ID3D11DeviceContext_SOGetTargets(ctx, 1, &b);
    if (b) ID3D11Buffer_Release(b);
    return b == want ? '1' : b ? 'x' : '0';
}
static char cs_uav(void *want) {
    ID3D11UnorderedAccessView *u = NULL;
    ID3D11DeviceContext_CSGetUnorderedAccessViews(ctx, 0, 1, &u);
    if (u) ID3D11UnorderedAccessView_Release(u);
    return u == want ? '1' : u ? 'x' : '0';
}

static void set_rt(ID3D11RenderTargetView *v, ID3D11DepthStencilView *d) { ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &v, d); }
static void set_ps(UINT slot, ID3D11ShaderResourceView *v) { ID3D11DeviceContext_PSSetShaderResources(ctx, slot, 1, &v); }
static void set_vs(UINT slot, ID3D11ShaderResourceView *v) { ID3D11DeviceContext_VSSetShaderResources(ctx, slot, 1, &v); }
static void set_cs(UINT slot, ID3D11ShaderResourceView *v) { ID3D11DeviceContext_CSSetShaderResources(ctx, slot, 1, &v); }

static void states(void) {
    D3D11_BLEND_DESC b = { 0 };
    b.RenderTarget[0].SrcBlend = D3D11_BLEND_ONE; b.RenderTarget[0].DestBlend = D3D11_BLEND_ZERO; b.RenderTarget[0].BlendOp = D3D11_BLEND_OP_ADD;
    b.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_ONE; b.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_ZERO; b.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
    b.RenderTarget[0].RenderTargetWriteMask = 0xf;
    ID3D11BlendState *b1 = NULL, *b2 = NULL, *b3 = NULL;
    ID3D11Device_CreateBlendState(dev, &b, &b1);
    ID3D11Device_CreateBlendState(dev, &b, &b2);
    b.RenderTarget[0].BlendEnable = TRUE;
    ID3D11Device_CreateBlendState(dev, &b, &b3);
    printf("blend: same desc same object %d, refs %lu; other desc other object %d, refs %lu\n", b1 == b2, refs(b1), b1 != b3, refs(b3));
    ID3D11DeviceContext_OMSetBlendState(ctx, b1, NULL, 0xffffffff);
    printf("blend: bound refs %lu\n", refs(b1));
    printf("blend: Release %lu %lu\n", ID3D11BlendState_Release(b1), ID3D11BlendState_Release(b2));
    ID3D11BlendState *g = NULL; float f[4]; UINT m;
    ID3D11DeviceContext_OMGetBlendState(ctx, &g, f, &m);
    printf("blend: still bound %d, refs after Get %lu\n", g == b1, g ? refs(g) : 0);
    if (g) ID3D11BlendState_Release(g);
    ID3D11DeviceContext_OMSetBlendState(ctx, NULL, NULL, 0xffffffff);
    ID3D11Device_CreateBlendState(dev, &b, &b2);
    printf("blend: made again while the other lives: same %d\n", b2 == b3);
    ID3D11BlendState_Release(b2);
    ID3D11BlendState_Release(b3);
    /* (a description differing only in a field the state ignores: a disabled blend's factors) */
    b.RenderTarget[0].BlendEnable = FALSE;
    ID3D11Device_CreateBlendState(dev, &b, &b1);
    b.RenderTarget[0].SrcBlend = D3D11_BLEND_SRC_ALPHA;
    ID3D11Device_CreateBlendState(dev, &b, &b2);
    printf("blend: differs only in a disabled factor: same %d\n", b1 == b2);
    ID3D11BlendState_Release(b1); ID3D11BlendState_Release(b2);

    D3D11_SAMPLER_DESC s = { D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE_ADDRESS_CLAMP, 0, 1, D3D11_COMPARISON_NEVER, { 0 }, 0, D3D11_FLOAT32_MAX };
    ID3D11SamplerState *s1 = NULL, *s2 = NULL;
    ID3D11Device_CreateSamplerState(dev, &s, &s1);
    ID3D11Device_CreateSamplerState(dev, &s, &s2);
    printf("sampler: same desc same object %d refs %lu\n", s1 == s2, refs(s1));
    ID3D11SamplerState_Release(s1); ID3D11SamplerState_Release(s2);
    D3D11_RASTERIZER_DESC r = { D3D11_FILL_SOLID, D3D11_CULL_BACK, FALSE, 0, 0, 0, TRUE, FALSE, FALSE, FALSE };
    ID3D11RasterizerState *r1 = NULL, *r2 = NULL;
    ID3D11Device_CreateRasterizerState(dev, &r, &r1);
    ID3D11Device_CreateRasterizerState(dev, &r, &r2);
    printf("rasterizer: same desc same object %d refs %lu\n", r1 == r2, refs(r1));
    ID3D11RasterizerState_Release(r1); ID3D11RasterizerState_Release(r2);
    D3D11_DEPTH_STENCIL_DESC d = { TRUE, D3D11_DEPTH_WRITE_MASK_ALL, D3D11_COMPARISON_LESS, FALSE, 0xff, 0xff,
                                   { D3D11_STENCIL_OP_KEEP, D3D11_STENCIL_OP_KEEP, D3D11_STENCIL_OP_KEEP, D3D11_COMPARISON_ALWAYS },
                                   { D3D11_STENCIL_OP_KEEP, D3D11_STENCIL_OP_KEEP, D3D11_STENCIL_OP_KEEP, D3D11_COMPARISON_ALWAYS } };
    ID3D11DepthStencilState *d1 = NULL, *d2 = NULL;
    ID3D11Device_CreateDepthStencilState(dev, &d, &d1);
    ID3D11Device_CreateDepthStencilState(dev, &d, &d2);
    printf("depthstencil: same desc same object %d refs %lu\n", d1 == d2, refs(d1));
    ID3D11DepthStencilState_Release(d1); ID3D11DepthStencilState_Release(d2);
}

int main(void) {
    D3D_FEATURE_LEVEL fl = D3D_FEATURE_LEVEL_11_0;
    HRESULT hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, &fl, 1, D3D11_SDK_VERSION, &dev, NULL, &ctx);
    if (FAILED(hr)) hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_WARP, NULL, 0, &fl, 1, D3D11_SDK_VERSION, &dev, NULL, &ctx);
    if (FAILED(hr)) { printf("skip D3D11CreateDevice %#lx\n", hr); return 0; }
    states();

    ID3D11Texture2D *a = tex(D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_UNORDERED_ACCESS, 2, DXGI_FORMAT_R8G8B8A8_UNORM);
    ID3D11Texture2D *other = tex(D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 1, DXGI_FORMAT_R8G8B8A8_UNORM);
    ID3D11ShaderResourceView *sa0 = srv_mip(a, DXGI_FORMAT_R8G8B8A8_UNORM, 0), *sa1 = srv_mip(a, DXGI_FORMAT_R8G8B8A8_UNORM, 1);
    ID3D11ShaderResourceView *so_ = srv_mip(other, DXGI_FORMAT_R8G8B8A8_UNORM, 0);
    ID3D11RenderTargetView *ra0 = rtv_mip(a, 0), *ra1 = rtv_mip(a, 1), *ro = rtv_mip(other, 0);

    /* output after input: the inputs reading what is written go */
    set_ps(0, sa0); set_ps(1, sa1); set_vs(0, sa0); set_cs(0, sa0); set_ps(2, so_);
    set_rt(ra0, NULL);
    printf("rt mip 0 after srv: ps0 %c ps1(mip 1) %c vs0 %c cs0 %c ps2(other) %c rt %c\n", ps_srv(0, sa0), ps_srv(1, sa1), vs_srv(0, sa0), cs_srv(0, sa0), ps_srv(2, so_), rt(0, ra0));
    /* input after output: refused */
    set_ps(0, sa0); set_ps(1, sa1);
    printf("srv after rt mip 0: ps0 %c ps1(mip 1) %c rt %c\n", ps_srv(0, sa0), ps_srv(1, sa1), rt(0, ra0));
    set_rt(ro, NULL);
    printf("rt moved away: ps0 %c (refused binds stay NULL)\n", ps_srv(0, sa0));
    set_ps(0, sa0);
    printf("srv again: ps0 %c\n", ps_srv(0, sa0));
    /* render target and a UAV of the same resource; compute UAV vs SRV */
    {
        ID3D11UnorderedAccessView *u = NULL;
        D3D11_UNORDERED_ACCESS_VIEW_DESC ud = { DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_UAV_DIMENSION_TEXTURE2D };
        ID3D11Device_CreateUnorderedAccessView(dev, (ID3D11Resource *)a, &ud, &u);
        set_cs(0, sa0);
        ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &u, NULL);
        printf("cs uav after cs srv: cs srv %c uav %c ps0 %c\n", cs_srv(0, sa0), cs_uav(u), ps_srv(0, sa0));
        set_cs(0, sa0);
        printf("cs srv after cs uav: cs srv %c uav %c\n", cs_srv(0, sa0), cs_uav(u));
        ID3D11UnorderedAccessView *nu = NULL;
        ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &nu, NULL);
        ID3D11UnorderedAccessView_Release(u);
    }
    /* depth: a depth texture's SRV and a DSV (read-only too) */
    {
        ID3D11Texture2D *z = tex(D3D11_BIND_DEPTH_STENCIL | D3D11_BIND_SHADER_RESOURCE, 1, DXGI_FORMAT_R24G8_TYPELESS);
        ID3D11ShaderResourceView *sz = srv_mip(z, DXGI_FORMAT_R24_UNORM_X8_TYPELESS, 0);
        D3D11_DEPTH_STENCIL_VIEW_DESC dd = { DXGI_FORMAT_D24_UNORM_S8_UINT, D3D11_DSV_DIMENSION_TEXTURE2D, 0 };
        ID3D11DepthStencilView *dz = NULL, *dzr = NULL;
        ID3D11Device_CreateDepthStencilView(dev, (ID3D11Resource *)z, &dd, &dz);
        dd.Flags = D3D11_DSV_READ_ONLY_DEPTH | D3D11_DSV_READ_ONLY_STENCIL;
        ID3D11Device_CreateDepthStencilView(dev, (ID3D11Resource *)z, &dd, &dzr);
        set_ps(3, sz);
        set_rt(ro, dz);
        printf("dsv after its srv: ps3 %c ds %c\n", ps_srv(3, sz), ds(dz));
        set_ps(3, sz);
        printf("srv after dsv: ps3 %c ds %c\n", ps_srv(3, sz), ds(dz));
        set_rt(ro, dzr);
        set_ps(3, sz);
        printf("srv with read-only dsv: ps3 %c ds %c\n", ps_srv(3, sz), ds(dzr));
        set_rt(ro, NULL); set_ps(3, NULL);
        ID3D11DepthStencilView_Release(dz); ID3D11DepthStencilView_Release(dzr);
        ID3D11ShaderResourceView_Release(sz); ID3D11Texture2D_Release(z);
    }
    /* buffers: a vertex buffer that becomes a stream output target, a constant buffer */
    {
        D3D11_BUFFER_DESC bd = { 256, D3D11_USAGE_DEFAULT, D3D11_BIND_VERTEX_BUFFER | D3D11_BIND_STREAM_OUTPUT, 0, 0, 0 };
        ID3D11Buffer *b = NULL;
        ID3D11Device_CreateBuffer(dev, &bd, NULL, &b);
        UINT stride = 16, off = 0;
        ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &b, &stride, &off);
        ID3D11DeviceContext_SOSetTargets(ctx, 1, &b, &off);
        printf("so after vb: vb %c so %c\n", vb(b), so(b));
        ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &b, &stride, &off);
        printf("vb after so: vb %c so %c\n", vb(b), so(b));
        ID3D11Buffer *n = NULL;
        ID3D11DeviceContext_SOSetTargets(ctx, 1, &n, &off);
        ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &n, &stride, &off);
        ID3D11Buffer_Release(b);
    }
    /* references slots hold; Get* adds one */
    {
        set_rt(ro, NULL);
        set_ps(0, NULL); set_ps(1, NULL); set_ps(2, NULL); set_vs(0, NULL); set_cs(0, NULL);
        ULONG r0 = refs(sa1);
        set_ps(5, sa1);
        ULONG r1 = refs(sa1);
        set_ps(6, sa1);
        ULONG r2 = refs(sa1);
        ID3D11ShaderResourceView *g = NULL;
        ID3D11DeviceContext_PSGetShaderResources(ctx, 5, 1, &g);
        printf("srv refs: bound +%ld, bound twice +%ld, after Get +%ld\n", (long)(r1 - r0), (long)(r2 - r0), (long)(refs(sa1) - r0));
        ID3D11ShaderResourceView_Release(g);
        ULONG t0 = refs(a);
        printf("texture refs with views bound %lu\n", t0);
        ID3D11DeviceContext_ClearState(ctx);
        printf("ClearState: ps5 %c rt %c srv refs %+ld\n", ps_srv(5, sa1), rt(0, ro), (long)(refs(sa1) - r0));
    }
    /* a view released by the application while bound stays usable through Get */
    {
        ID3D11ShaderResourceView *t = srv_mip(other, DXGI_FORMAT_R8G8B8A8_UNORM, 0);
        set_ps(7, t);
        printf("Release of a bound srv %lu\n", ID3D11ShaderResourceView_Release(t));
        printf("still bound %c\n", ps_srv(7, t));
        set_ps(7, NULL);
    }
    ID3D11ShaderResourceView_Release(sa0); ID3D11ShaderResourceView_Release(sa1); ID3D11ShaderResourceView_Release(so_);
    ID3D11RenderTargetView_Release(ra0); ID3D11RenderTargetView_Release(ra1); ID3D11RenderTargetView_Release(ro);
    printf("Release textures %lu %lu\n", ID3D11Texture2D_Release(a), ID3D11Texture2D_Release(other));
    ID3D11DeviceContext_Release(ctx);
    printf("Release device %lu\n", ID3D11Device_Release(dev));
    return 0;
}
