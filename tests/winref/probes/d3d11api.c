/* Direct3D 11 API answers (feature level 11_0; WARP on the Windows runner): what the device says
 * about each DXGI format (CheckFormatSupport, CheckFormatSupport2's flags, MSAA quality levels),
 * which texture and buffer descriptions it accepts (each format with each bind flag, mip counts,
 * sizes, usages and CPU access, misc flags), views of them, queries (sizes, GetData's answers
 * before and after End, size checks), and the reference counts the device and resources show.
 * libs: -ld3d11 -ldxgi
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <d3d11.h>
#include <dxgi1_2.h>

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;

static ULONG refs(void *o) {
    IUnknown_AddRef((IUnknown *)o);
    return IUnknown_Release((IUnknown *)o);
}

/* one texture: S_OK, E_INVALIDARG or another code */
static HRESULT tex2d(DXGI_FORMAT f, UINT w, UINT h, UINT mips, UINT layers, UINT bind, D3D11_USAGE usage, UINT cpu, UINT misc, UINT samples, ID3D11Texture2D **out) {
    D3D11_TEXTURE2D_DESC d = {w, h, mips, layers, f, {samples, 0}, usage, bind, cpu, misc};
    ID3D11Texture2D *t = NULL;
    HRESULT hr = ID3D11Device_CreateTexture2D(dev, &d, NULL, &t);
    if (out) *out = t;
    else if (t) ID3D11Texture2D_Release(t);
    return hr;
}

static const char *code(HRESULT hr) {
    static char b[16];
    if (hr == S_OK) return "ok";
    if (hr == S_FALSE) return "false";
    if (hr == E_INVALIDARG) return "inval";
    if (hr == E_FAIL) return "fail";
    if (hr == E_NOTIMPL) return "notimpl";
    if (hr == E_NOINTERFACE) return "noif";
    if (hr == DXGI_ERROR_INVALID_CALL) return "invcall";
    if (hr == DXGI_ERROR_UNSUPPORTED) return "unsup";
    snprintf(b, sizeof b, "%#lx", (unsigned long)hr);
    return b;
}

static void formats(void) {
    /* bind flags one at a time: VB IB CB SRV SO RT DS UAV */
    static const UINT binds[] = {0x1, 0x2, 0x4, 0x8, 0x10, 0x20, 0x40, 0x80};
    for (UINT f = 1; f <= 132; f++) {
        UINT sup = 0, q2 = 0, q4 = 0, q8 = 0;
        HRESULT hr = ID3D11Device_CheckFormatSupport(dev, f, &sup);
        D3D11_FEATURE_DATA_FORMAT_SUPPORT2 s2 = {f, 0};
        HRESULT hr2 = ID3D11Device_CheckFeatureSupport(dev, D3D11_FEATURE_FORMAT_SUPPORT2, &s2, sizeof s2);
        ID3D11Device_CheckMultisampleQualityLevels(dev, f, 2, &q2);
        ID3D11Device_CheckMultisampleQualityLevels(dev, f, 4, &q4);
        ID3D11Device_CheckMultisampleQualityLevels(dev, f, 8, &q8);
        char line[256];
        int n = snprintf(line, sizeof line, "fmt %u %s %#x %s %#x ms %u %u %u tex", f, code(hr), sup, code(hr2), s2.OutFormatSupport2, q2, q4, q8);
        n += snprintf(line + n, sizeof line - n, " %s", code(tex2d(f, 16, 16, 1, 1, 0, D3D11_USAGE_DEFAULT, 0, 0, 1, NULL)));
        for (int b = 0; b < 8; b++) n += snprintf(line + n, sizeof line - n, " %s", code(tex2d(f, 16, 16, 1, 1, binds[b], D3D11_USAGE_DEFAULT, 0, 0, 1, NULL)));
        /* mip chain, staging, dynamic, cube, generate-mips, 1D, 3D */
        n += snprintf(line + n, sizeof line - n, " | %s", code(tex2d(f, 16, 16, 0, 1, 8, D3D11_USAGE_DEFAULT, 0, 0, 1, NULL)));
        n += snprintf(line + n, sizeof line - n, " %s", code(tex2d(f, 16, 16, 1, 1, 0, D3D11_USAGE_STAGING, D3D11_CPU_ACCESS_READ, 0, 1, NULL)));
        n += snprintf(line + n, sizeof line - n, " %s", code(tex2d(f, 16, 16, 1, 1, 8, D3D11_USAGE_DYNAMIC, D3D11_CPU_ACCESS_WRITE, 0, 1, NULL)));
        n += snprintf(line + n, sizeof line - n, " %s", code(tex2d(f, 16, 16, 1, 6, 8, D3D11_USAGE_DEFAULT, 0, D3D11_RESOURCE_MISC_TEXTURECUBE, 1, NULL)));
        n += snprintf(line + n, sizeof line - n, " %s", code(tex2d(f, 16, 16, 0, 1, 0x28, D3D11_USAGE_DEFAULT, 0, D3D11_RESOURCE_MISC_GENERATE_MIPS, 1, NULL)));
        n += snprintf(line + n, sizeof line - n, " %s", code(tex2d(f, 16, 16, 1, 1, 0x20, D3D11_USAGE_DEFAULT, 0, 0, 4, NULL)));
        {
            D3D11_TEXTURE1D_DESC d = {16, 1, 1, f, D3D11_USAGE_DEFAULT, 8, 0, 0};
            ID3D11Texture1D *t = NULL;
            n += snprintf(line + n, sizeof line - n, " %s", code(ID3D11Device_CreateTexture1D(dev, &d, NULL, &t)));
            if (t) ID3D11Texture1D_Release(t);
        }
        {
            D3D11_TEXTURE3D_DESC d = {8, 8, 8, 1, f, D3D11_USAGE_DEFAULT, 8, 0, 0};
            ID3D11Texture3D *t = NULL;
            n += snprintf(line + n, sizeof line - n, " %s", code(ID3D11Device_CreateTexture3D(dev, &d, NULL, &t)));
            if (t) ID3D11Texture3D_Release(t);
        }
        /* a typed buffer view of the format */
        {
            D3D11_BUFFER_DESC bd = {256, D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, 0, 0, 0};
            ID3D11Buffer *b = NULL;
            ID3D11ShaderResourceView *v = NULL;
            ID3D11Device_CreateBuffer(dev, &bd, NULL, &b);
            D3D11_SHADER_RESOURCE_VIEW_DESC vd = {f, D3D11_SRV_DIMENSION_BUFFER};
            vd.Buffer.FirstElement = 0;
            vd.Buffer.NumElements = 4;
            n += snprintf(line + n, sizeof line - n, " bsrv %s", b ? code(ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)b, &vd, &v)) : "-");
            if (v) ID3D11ShaderResourceView_Release(v);
            if (b) ID3D11Buffer_Release(b);
        }
        puts(line);
    }
}

/* texture descriptions other than the format */
static void tex_descs(void) {
    static const struct { const char *name; UINT w, h, mips, layers, bind, usage, cpu, misc, samples; } t[] = {
        {"w0", 0, 16, 1, 1, 8, 0, 0, 0, 1},
        {"h0", 16, 0, 1, 1, 8, 0, 0, 0, 1},
        {"layers0", 16, 16, 1, 0, 8, 0, 0, 0, 1},
        {"samples0", 16, 16, 1, 1, 8, 0, 0, 0, 0},
        {"w16384", 16384, 1, 1, 1, 8, 0, 0, 0, 1},
        {"w16385", 16385, 1, 1, 1, 8, 0, 0, 0, 1},
        {"layers2048", 4, 4, 1, 2048, 8, 0, 0, 0, 1},
        {"layers2049", 4, 4, 1, 2049, 8, 0, 0, 0, 1},
        {"mips5of16", 16, 16, 5, 1, 8, 0, 0, 0, 1},
        {"mips6of16", 16, 16, 6, 1, 8, 0, 0, 0, 1},
        {"mips6of17", 17, 3, 6, 1, 8, 0, 0, 0, 1},
        {"immutable", 16, 16, 1, 1, 8, 1, 0, 0, 1},
        {"dynamic_nocpu", 16, 16, 1, 1, 8, 2, 0, 0, 1},
        {"dynamic_read", 16, 16, 1, 1, 8, 2, D3D11_CPU_ACCESS_READ, 0, 1},
        {"dynamic_mips2", 16, 16, 2, 1, 8, 2, D3D11_CPU_ACCESS_WRITE, 0, 1},
        {"dynamic_layers2", 16, 16, 1, 2, 8, 2, D3D11_CPU_ACCESS_WRITE, 0, 1},
        {"dynamic_rt", 16, 16, 1, 1, 0x28, 2, D3D11_CPU_ACCESS_WRITE, 0, 1},
        {"staging_bind", 16, 16, 1, 1, 8, 3, D3D11_CPU_ACCESS_READ, 0, 1},
        {"staging_nocpu", 16, 16, 1, 1, 0, 3, 0, 0, 1},
        {"staging_rw", 16, 16, 1, 1, 0, 3, D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE, 0, 1},
        {"default_cpu", 16, 16, 1, 1, 8, 0, D3D11_CPU_ACCESS_WRITE, 0, 1},
        {"cube5", 16, 16, 1, 5, 8, 0, 0, D3D11_RESOURCE_MISC_TEXTURECUBE, 1},
        {"cube12", 16, 16, 1, 12, 8, 0, 0, D3D11_RESOURCE_MISC_TEXTURECUBE, 1},
        {"cube9", 16, 16, 1, 9, 8, 0, 0, D3D11_RESOURCE_MISC_TEXTURECUBE, 1},
        {"cube_rect", 16, 8, 1, 6, 8, 0, 0, D3D11_RESOURCE_MISC_TEXTURECUBE, 1},
        {"genmips_nort", 16, 16, 0, 1, 8, 0, 0, D3D11_RESOURCE_MISC_GENERATE_MIPS, 1},
        {"genmips_nosrv", 16, 16, 0, 1, 0x20, 0, 0, D3D11_RESOURCE_MISC_GENERATE_MIPS, 1},
        {"shared", 16, 16, 1, 1, 0x28, 0, 0, D3D11_RESOURCE_MISC_SHARED, 1},
        {"shared_mutex", 16, 16, 1, 1, 0x28, 0, 0, D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX, 1},
        {"gdi", 16, 16, 1, 1, 0x28, 0, 0, D3D11_RESOURCE_MISC_GDI_COMPATIBLE, 1},
        {"raw", 16, 16, 1, 1, 8, 0, 0, D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS, 1},
        {"ms4_mips2", 16, 16, 2, 1, 0x20, 0, 0, 0, 4},
        {"ms3", 16, 16, 1, 1, 0x20, 0, 0, 0, 3},
        {"ms8", 16, 16, 1, 1, 0x20, 0, 0, 0, 8},
        {"ms16", 16, 16, 1, 1, 0x20, 0, 0, 0, 16},
        {"ms4_staging", 16, 16, 1, 1, 0, 3, D3D11_CPU_ACCESS_READ, 0, 4},
        {"bind_decoder", 16, 16, 1, 1, 0x200, 0, 0, 0, 1},
        {"bind_bad", 16, 16, 1, 1, 0x1000, 0, 0, 0, 1},
    };
    for (unsigned i = 0; i < sizeof t / sizeof *t; i++) {
        ID3D11Texture2D *x = NULL;
        HRESULT hr = tex2d(DXGI_FORMAT_R8G8B8A8_UNORM, t[i].w, t[i].h, t[i].mips, t[i].layers, t[i].bind, t[i].usage, t[i].cpu, t[i].misc, t[i].samples, &x);
        if (x) {
            D3D11_TEXTURE2D_DESC d;
            ID3D11Texture2D_GetDesc(x, &d);
            printf("tex %s %s mips %u misc %#x\n", t[i].name, code(hr), d.MipLevels, d.MiscFlags);
            ID3D11Texture2D_Release(x);
        } else {
            printf("tex %s %s\n", t[i].name, code(hr));
        }
    }
    /* initial data: a NULL pSysMem, for a texture with a mip chain */
    {
        D3D11_TEXTURE2D_DESC d = {4, 4, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0}, D3D11_USAGE_DEFAULT, 8, 0, 0};
        D3D11_SUBRESOURCE_DATA sd = {NULL, 16, 0};
        ID3D11Texture2D *x = NULL;
        printf("tex nulldata %s\n", code(ID3D11Device_CreateTexture2D(dev, &d, &sd, &x)));
        if (x) ID3D11Texture2D_Release(x);
        d.Usage = D3D11_USAGE_IMMUTABLE;
        printf("tex immutable_nodata %s\n", code(ID3D11Device_CreateTexture2D(dev, &d, NULL, &x)));
        printf("tex null_desc %s\n", code(ID3D11Device_CreateTexture2D(dev, NULL, NULL, &x)));
        d.Usage = D3D11_USAGE_DEFAULT;
        printf("tex null_out %s\n", code(ID3D11Device_CreateTexture2D(dev, &d, NULL, NULL)));
    }
}

static void buffers(void) {
    static const struct { const char *name; UINT size, usage, bind, cpu, misc, stride; } b[] = {
        {"size0", 0, 0, 1, 0, 0, 0},
        {"vb1", 1, 0, 1, 0, 0, 0},
        {"cb1", 1, 0, 4, 0, 0, 0},
        {"cb15", 15, 0, 4, 0, 0, 0},
        {"cb16", 16, 0, 4, 0, 0, 0},
        {"cb17", 17, 0, 4, 0, 0, 0},
        {"cb65536", 65536, 0, 4, 0, 0, 0},
        {"cb65552", 65552, 0, 4, 0, 0, 0},
        {"cb_vb", 16, 0, 5, 0, 0, 0},
        {"cb_srv", 16, 0, 0xc, 0, 0, 0},
        {"vb_ib_srv", 16, 0, 0xb, 0, 0, 0},
        {"vb_rt", 16, 0, 0x21, 0, 0, 0},
        {"vb_ds", 16, 0, 0x41, 0, 0, 0},
        {"so", 16, 0, 0x10, 0, 0, 0},
        {"uav", 16, 0, 0x80, 0, 0, 0},
        {"nobind", 16, 0, 0, 0, 0, 0},
        {"dynamic_vb", 16, 2, 1, D3D11_CPU_ACCESS_WRITE, 0, 0},
        {"dynamic_nocpu", 16, 2, 1, 0, 0, 0},
        {"dynamic_read", 16, 2, 1, D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE, 0, 0},
        {"dynamic_uav", 16, 2, 0x80, D3D11_CPU_ACCESS_WRITE, 0, 0},
        {"dynamic_so", 16, 2, 0x10, D3D11_CPU_ACCESS_WRITE, 0, 0},
        {"immutable", 16, 1, 1, 0, 0, 0},
        {"staging", 16, 3, 0, D3D11_CPU_ACCESS_READ, 0, 0},
        {"staging_bind", 16, 3, 1, D3D11_CPU_ACCESS_READ, 0, 0},
        {"staging_nocpu", 16, 3, 0, 0, 0, 0},
        {"default_cpu", 16, 0, 1, D3D11_CPU_ACCESS_WRITE, 0, 0},
        {"structured", 64, 0, 8, 0, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 16},
        {"structured_s0", 64, 0, 8, 0, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 0},
        {"structured_s3", 64, 0, 8, 0, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 3},
        {"structured_s6", 66, 0, 8, 0, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 6},
        {"structured_s2052", 2052, 0, 8, 0, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 2052},
        {"structured_s2056", 2056, 0, 8, 0, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 2056},
        {"structured_odd", 60, 0, 8, 0, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 16},
        {"structured_vb", 64, 0, 1, 0, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 16},
        {"structured_cb", 64, 0, 4, 0, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 16},
        {"raw", 64, 0, 8, 0, D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS, 0},
        {"raw_vb", 64, 0, 1, 0, D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS, 0},
        {"raw_structured", 64, 0, 8, 0, D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS | D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 16},
        {"indirect", 64, 0, 0, 0, D3D11_RESOURCE_MISC_DRAWINDIRECT_ARGS, 0},
        {"indirect_vb", 64, 0, 1, 0, D3D11_RESOURCE_MISC_DRAWINDIRECT_ARGS, 0},
        {"cube", 64, 0, 1, 0, D3D11_RESOURCE_MISC_TEXTURECUBE, 0},
        {"genmips", 64, 0, 0x28, 0, D3D11_RESOURCE_MISC_GENERATE_MIPS, 0},
        {"stride_unused", 64, 0, 1, 0, 0, 7},
    };
    for (unsigned i = 0; i < sizeof b / sizeof *b; i++) {
        D3D11_BUFFER_DESC d = {b[i].size, b[i].usage, b[i].bind, b[i].cpu, b[i].misc, b[i].stride};
        ID3D11Buffer *x = NULL;
        HRESULT hr = ID3D11Device_CreateBuffer(dev, &d, NULL, &x);
        if (x) {
            D3D11_BUFFER_DESC g;
            ID3D11Buffer_GetDesc(x, &g);
            printf("buf %s %s size %u stride %u misc %#x\n", b[i].name, code(hr), g.ByteWidth, g.StructureByteStride, g.MiscFlags);
            ID3D11Buffer_Release(x);
        } else {
            printf("buf %s %s\n", b[i].name, code(hr));
        }
    }
    D3D11_BUFFER_DESC d = {16, D3D11_USAGE_IMMUTABLE, 1, 0, 0, 0};
    ID3D11Buffer *x = NULL;
    printf("buf immutable_nodata %s\n", code(ID3D11Device_CreateBuffer(dev, &d, NULL, &x)));
    if (x) ID3D11Buffer_Release(x);
    D3D11_SUBRESOURCE_DATA sd = {NULL, 0, 0};
    d.Usage = D3D11_USAGE_DEFAULT;
    x = NULL;
    printf("buf null_sysmem %s\n", code(ID3D11Device_CreateBuffer(dev, &d, &sd, &x)));
    if (x) ID3D11Buffer_Release(x);
    printf("buf null_desc %s\n", code(ID3D11Device_CreateBuffer(dev, NULL, NULL, &x)));
    printf("buf null_out %s\n", code(ID3D11Device_CreateBuffer(dev, &d, NULL, NULL)));
}

/* the union fields a view dimension has (Windows leaves the others as they happen to be) */
static int union_words(int kind, UINT dim) {
    static const int srv[] = {0, 0, 2, 4, 2, 4, 0, 2, 2, 2, 4, 3}, rtv[] = {0, 2, 1, 3, 1, 3, 0, 2, 3}, dsv[] = {0, 1, 3, 1, 3, 0, 2}, uav[] = {0, 3, 1, 3, 1, 3, 0, 0, 3};
    switch (kind) {
    case 0: return dim < 12 ? srv[dim] : 0;
    case 1: return dim < 9 ? rtv[dim] : 0;
    case 2: return dim < 7 ? dsv[dim] : 0;
    default: return dim < 9 ? uav[dim] : 0;
    }
}

static void print_union(char *out, size_t size, const void *u, int n) {
    UINT w[4];
    memcpy(w, u, n * sizeof *w);
    size_t at = strlen(out);
    for (int i = 0; i < n; i++) at += snprintf(out + at, size - at, " %u", w[i]);
}

/* views of a texture: the view's format against the texture's (typeless families), mips and
 * array ranges, NULL descriptions */
static void views(void) {
    static const struct { const char *name; DXGI_FORMAT tf; UINT mips, layers, bind, kind; DXGI_FORMAT vf; UINT dim, a, b, c, d; } v[] = {
        /* kind 0 SRV (dim, most detailed mip, mips, first slice, slices), 1 RTV (dim, mip, first, n),
         * 2 DSV (dim, mip, first, n), 3 UAV (dim, mip, first, n) */
        {"srv_null", DXGI_FORMAT_R8G8B8A8_UNORM, 3, 1, 8, 0, 0, 0},
        {"srv_same", DXGI_FORMAT_R8G8B8A8_UNORM, 3, 1, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE2D, 0, 3},
        {"srv_fmt0", DXGI_FORMAT_R8G8B8A8_UNORM, 3, 1, 8, 0, 0, D3D11_SRV_DIMENSION_TEXTURE2D, 0, 3},
        {"srv_srgb", DXGI_FORMAT_R8G8B8A8_UNORM, 3, 1, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, D3D11_SRV_DIMENSION_TEXTURE2D, 0, 3},
        {"srv_typeless_unorm", DXGI_FORMAT_R8G8B8A8_TYPELESS, 3, 1, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE2D, 0, 3},
        {"srv_typeless_uint", DXGI_FORMAT_R8G8B8A8_TYPELESS, 3, 1, 8, 0, DXGI_FORMAT_R8G8B8A8_UINT, D3D11_SRV_DIMENSION_TEXTURE2D, 0, 3},
        {"srv_typeless_null", DXGI_FORMAT_R8G8B8A8_TYPELESS, 3, 1, 8, 0, 0, 0},
        {"srv_typeless_typeless", DXGI_FORMAT_R8G8B8A8_TYPELESS, 3, 1, 8, 0, DXGI_FORMAT_R8G8B8A8_TYPELESS, D3D11_SRV_DIMENSION_TEXTURE2D, 0, 3},
        {"srv_other_family", DXGI_FORMAT_R8G8B8A8_TYPELESS, 3, 1, 8, 0, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE2D, 0, 3},
        {"srv_bgra_typeless", DXGI_FORMAT_B8G8R8A8_TYPELESS, 3, 1, 8, 0, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB, D3D11_SRV_DIMENSION_TEXTURE2D, 0, 3},
        {"srv_r32_float_of_d32", DXGI_FORMAT_R32_TYPELESS, 1, 1, 0x48, 0, DXGI_FORMAT_R32_FLOAT, D3D11_SRV_DIMENSION_TEXTURE2D, 0, 1},
        {"srv_d32_of_d32", DXGI_FORMAT_R32_TYPELESS, 1, 1, 0x48, 0, DXGI_FORMAT_D32_FLOAT, D3D11_SRV_DIMENSION_TEXTURE2D, 0, 1},
        {"srv_r24x8", DXGI_FORMAT_R24G8_TYPELESS, 1, 1, 0x48, 0, DXGI_FORMAT_R24_UNORM_X8_TYPELESS, D3D11_SRV_DIMENSION_TEXTURE2D, 0, 1},
        {"srv_x24g8", DXGI_FORMAT_R24G8_TYPELESS, 1, 1, 0x48, 0, DXGI_FORMAT_X24_TYPELESS_G8_UINT, D3D11_SRV_DIMENSION_TEXTURE2D, 0, 1},
        {"srv_mips_all", DXGI_FORMAT_R8G8B8A8_UNORM, 3, 1, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE2D, 1, -1},
        {"srv_mips_over", DXGI_FORMAT_R8G8B8A8_UNORM, 3, 1, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE2D, 1, 3},
        {"srv_mips0", DXGI_FORMAT_R8G8B8A8_UNORM, 3, 1, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE2D, 0, 0},
        {"srv_mip3", DXGI_FORMAT_R8G8B8A8_UNORM, 3, 1, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE2D, 3, 1},
        {"srv_array_of_single", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 1, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE2DARRAY, 0, 1, 0, 1},
        {"srv_2d_of_array", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 4, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE2D, 0, 1},
        {"srv_array_range", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 4, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE2DARRAY, 0, 1, 1, 3},
        {"srv_array_over", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 4, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE2DARRAY, 0, 1, 2, 3},
        {"srv_array_all", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 4, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE2DARRAY, 0, 1, 0, -1},
        {"srv_array_none", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 4, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE2DARRAY, 0, 1, 0, 0},
        {"srv_cube_of_2d", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 6, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURECUBE, 0, 1},
        {"srv_3d_of_2d", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 1, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE3D, 0, 1},
        {"srv_1d_of_2d", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 1, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE1D, 0, 1},
        {"srv_ms_of_2d", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 1, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_SRV_DIMENSION_TEXTURE2DMS},
        {"srv_dim0", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 1, 8, 0, DXGI_FORMAT_R8G8B8A8_UNORM, 0},
        {"srv_nobind", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 1, 0x20, 0, 0, 0},
        {"rtv_null", DXGI_FORMAT_R8G8B8A8_UNORM, 3, 1, 0x20, 1, 0, 0},
        {"rtv_mip2", DXGI_FORMAT_R8G8B8A8_UNORM, 3, 1, 0x20, 1, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_RTV_DIMENSION_TEXTURE2D, 2},
        {"rtv_mip3", DXGI_FORMAT_R8G8B8A8_UNORM, 3, 1, 0x20, 1, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_RTV_DIMENSION_TEXTURE2D, 3},
        {"rtv_fmt0", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 1, 0x20, 1, 0, D3D11_RTV_DIMENSION_TEXTURE2D},
        {"rtv_typeless_null", DXGI_FORMAT_R8G8B8A8_TYPELESS, 1, 1, 0x20, 1, 0, 0},
        {"rtv_typeless_srgb", DXGI_FORMAT_R8G8B8A8_TYPELESS, 1, 1, 0x20, 1, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, D3D11_RTV_DIMENSION_TEXTURE2D},
        {"rtv_typeless_snorm", DXGI_FORMAT_R8G8B8A8_TYPELESS, 1, 1, 0x20, 1, DXGI_FORMAT_R8G8B8A8_SNORM, D3D11_RTV_DIMENSION_TEXTURE2D},
        {"rtv_unorm_as_srgb", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 1, 0x20, 1, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, D3D11_RTV_DIMENSION_TEXTURE2D},
        {"rtv_array_slice", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 4, 0x20, 1, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_RTV_DIMENSION_TEXTURE2DARRAY, 0, 3, 1},
        {"rtv_array_over", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 4, 0x20, 1, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_RTV_DIMENSION_TEXTURE2DARRAY, 0, 3, 2},
        {"rtv_array_all", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 4, 0x20, 1, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_RTV_DIMENSION_TEXTURE2DARRAY, 0, 0, -1},
        {"rtv_of_srv_only", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 1, 8, 1, 0, 0},
        {"rtv_d24", DXGI_FORMAT_R24G8_TYPELESS, 1, 1, 0x48, 1, DXGI_FORMAT_D24_UNORM_S8_UINT, D3D11_RTV_DIMENSION_TEXTURE2D},
        {"dsv_null", DXGI_FORMAT_D24_UNORM_S8_UINT, 1, 1, 0x40, 2, 0, 0},
        {"dsv_typeless_null", DXGI_FORMAT_R24G8_TYPELESS, 1, 1, 0x40, 2, 0, 0},
        {"dsv_typeless_d24", DXGI_FORMAT_R24G8_TYPELESS, 1, 1, 0x40, 2, DXGI_FORMAT_D24_UNORM_S8_UINT, D3D11_DSV_DIMENSION_TEXTURE2D},
        {"dsv_r32_typeless_d32", DXGI_FORMAT_R32_TYPELESS, 1, 1, 0x40, 2, DXGI_FORMAT_D32_FLOAT, D3D11_DSV_DIMENSION_TEXTURE2D},
        {"dsv_r32_float", DXGI_FORMAT_R32_TYPELESS, 1, 1, 0x40, 2, DXGI_FORMAT_R32_FLOAT, D3D11_DSV_DIMENSION_TEXTURE2D},
        {"dsv_fmt0", DXGI_FORMAT_D32_FLOAT, 1, 1, 0x40, 2, 0, D3D11_DSV_DIMENSION_TEXTURE2D},
        {"dsv_flags_ro", DXGI_FORMAT_D24_UNORM_S8_UINT, 1, 1, 0x40, 2, DXGI_FORMAT_D24_UNORM_S8_UINT, D3D11_DSV_DIMENSION_TEXTURE2D, 0, 0, 0, 3},
        {"dsv_flags_bad", DXGI_FORMAT_D24_UNORM_S8_UINT, 1, 1, 0x40, 2, DXGI_FORMAT_D24_UNORM_S8_UINT, D3D11_DSV_DIMENSION_TEXTURE2D, 0, 0, 0, 4},
        {"dsv_d32_stencil_ro", DXGI_FORMAT_D32_FLOAT, 1, 1, 0x40, 2, DXGI_FORMAT_D32_FLOAT, D3D11_DSV_DIMENSION_TEXTURE2D, 0, 0, 0, 2},
        {"dsv_of_rt", DXGI_FORMAT_R8G8B8A8_UNORM, 1, 1, 0x20, 2, 0, 0},
        {"uav_null", DXGI_FORMAT_R32_FLOAT, 1, 1, 0x80, 3, 0, 0},
        {"uav_typeless_r32_uint", DXGI_FORMAT_R32_TYPELESS, 1, 1, 0x80, 3, DXGI_FORMAT_R32_UINT, D3D11_UAV_DIMENSION_TEXTURE2D},
        {"uav_rgba8_typeless_uint", DXGI_FORMAT_R8G8B8A8_TYPELESS, 1, 1, 0x80, 3, DXGI_FORMAT_R8G8B8A8_UINT, D3D11_UAV_DIMENSION_TEXTURE2D},
        {"uav_rgba8_typeless_r32", DXGI_FORMAT_R8G8B8A8_TYPELESS, 1, 1, 0x80, 3, DXGI_FORMAT_R32_UINT, D3D11_UAV_DIMENSION_TEXTURE2D},
        {"uav_of_srv", DXGI_FORMAT_R32_FLOAT, 1, 1, 8, 3, 0, 0},
    };
    for (unsigned i = 0; i < sizeof v / sizeof *v; i++) {
        ID3D11Texture2D *t = NULL;
        HRESULT hr = tex2d(v[i].tf, 16, 16, v[i].mips, v[i].layers, v[i].bind, D3D11_USAGE_DEFAULT, 0, 0, 1, &t);
        if (!t) {
            printf("view %s texture %s\n", v[i].name, code(hr));
            continue;
        }
        ID3D11Resource *r = (ID3D11Resource *)t;
        BOOL nul = !v[i].vf && !v[i].dim;
        IUnknown *o = NULL;
        ULONG before = refs(t);
        char got[160] = "";
        switch (v[i].kind) {
        case 0: {
            D3D11_SHADER_RESOURCE_VIEW_DESC d = {v[i].vf, v[i].dim};
            d.Texture2DArray.MostDetailedMip = v[i].a;
            d.Texture2DArray.MipLevels = v[i].b;
            d.Texture2DArray.FirstArraySlice = v[i].c;
            d.Texture2DArray.ArraySize = v[i].d;
            hr = ID3D11Device_CreateShaderResourceView(dev, r, nul ? NULL : &d, (ID3D11ShaderResourceView **)&o);
            if (o) {
                ID3D11ShaderResourceView_GetDesc((ID3D11ShaderResourceView *)o, &d);
                snprintf(got, sizeof got, " fmt %u dim %u", d.Format, d.ViewDimension);
                print_union(got, sizeof got, &d.Texture2DArray, union_words(0, d.ViewDimension));
            }
            break;
        }
        case 1: {
            D3D11_RENDER_TARGET_VIEW_DESC d = {v[i].vf, v[i].dim};
            d.Texture2DArray.MipSlice = v[i].a;
            d.Texture2DArray.FirstArraySlice = v[i].b;
            d.Texture2DArray.ArraySize = v[i].c;
            hr = ID3D11Device_CreateRenderTargetView(dev, r, nul ? NULL : &d, (ID3D11RenderTargetView **)&o);
            if (o) {
                ID3D11RenderTargetView_GetDesc((ID3D11RenderTargetView *)o, &d);
                snprintf(got, sizeof got, " fmt %u dim %u", d.Format, d.ViewDimension);
                print_union(got, sizeof got, &d.Texture2DArray, union_words(1, d.ViewDimension));
            }
            break;
        }
        case 2: {
            D3D11_DEPTH_STENCIL_VIEW_DESC d = {v[i].vf, v[i].dim, v[i].d};
            d.Texture2DArray.MipSlice = v[i].a;
            d.Texture2DArray.FirstArraySlice = v[i].b;
            d.Texture2DArray.ArraySize = v[i].c;
            hr = ID3D11Device_CreateDepthStencilView(dev, r, nul ? NULL : &d, (ID3D11DepthStencilView **)&o);
            if (o) {
                ID3D11DepthStencilView_GetDesc((ID3D11DepthStencilView *)o, &d);
                snprintf(got, sizeof got, " fmt %u dim %u flags %u", d.Format, d.ViewDimension, d.Flags);
                print_union(got, sizeof got, &d.Texture2DArray, union_words(2, d.ViewDimension));
            }
            break;
        }
        default: {
            D3D11_UNORDERED_ACCESS_VIEW_DESC d = {v[i].vf, v[i].dim};
            d.Texture2DArray.MipSlice = v[i].a;
            d.Texture2DArray.FirstArraySlice = v[i].b;
            d.Texture2DArray.ArraySize = v[i].c;
            hr = ID3D11Device_CreateUnorderedAccessView(dev, r, nul ? NULL : &d, (ID3D11UnorderedAccessView **)&o);
            if (o) {
                ID3D11UnorderedAccessView_GetDesc((ID3D11UnorderedAccessView *)o, &d);
                snprintf(got, sizeof got, " fmt %u dim %u", d.Format, d.ViewDimension);
                print_union(got, sizeof got, &d.Texture2DArray, union_words(3, d.ViewDimension));
            }
        }
        }
        printf("view %s %s%s texrefs %lu->%lu\n", v[i].name, code(hr), got, before, refs(t));
        if (o) IUnknown_Release(o);
        ID3D11Texture2D_Release(t);
    }
}

static void queries(void) {
    for (UINT q = 0; q <= 16; q++) {
        D3D11_QUERY_DESC d = {q, 0};
        ID3D11Query *x = NULL;
        ID3D11Predicate *p = NULL;
        HRESULT hr = ID3D11Device_CreateQuery(dev, &d, &x);
        HRESULT hp = ID3D11Device_CreatePredicate(dev, &d, &p);
        if (p) ID3D11Predicate_Release(p);
        if (!x) {
            printf("query %u %s pred %s\n", q, code(hr), code(hp));
            continue;
        }
        ID3D11Asynchronous *a = (ID3D11Asynchronous *)x;
        UINT size = ID3D11Asynchronous_GetDataSize(a);
        BYTE buf[128];
        /* before anything, between Begin and End, after End (all of it, too little, too much) */
        HRESULT g0 = ID3D11DeviceContext_GetData(ctx, a, buf, size, 0);
        ID3D11DeviceContext_Begin(ctx, a);
        HRESULT g1 = ID3D11DeviceContext_GetData(ctx, a, NULL, 0, 0);
        ID3D11DeviceContext_End(ctx, a);
        HRESULT g2;
        for (int i = 0; i < 1000 && (g2 = ID3D11DeviceContext_GetData(ctx, a, NULL, 0, 0)) == S_FALSE; i++) Sleep(1);
        HRESULT g3 = ID3D11DeviceContext_GetData(ctx, a, buf, size, 0);
        HRESULT g4 = ID3D11DeviceContext_GetData(ctx, a, buf, size - 1, 0);
        HRESULT g5 = ID3D11DeviceContext_GetData(ctx, a, buf, size + 1, 0);
        HRESULT g6 = ID3D11DeviceContext_GetData(ctx, a, NULL, size, 0);
        HRESULT g7 = ID3D11DeviceContext_GetData(ctx, a, buf, 0, 0);
        /* Begin again after End: GetData until the next End */
        ID3D11DeviceContext_Begin(ctx, a);
        HRESULT g8 = ID3D11DeviceContext_GetData(ctx, a, NULL, 0, 0);
        ID3D11DeviceContext_End(ctx, a);
        printf("query %u %s pred %s size %u fresh %s begun %s ended %s data %s short %s long %s nodata %s nosize %s rebegun %s\n", q, code(hr), code(hp), size, code(g0), code(g1),
            code(g2), code(g3), code(g4), code(g5), code(g6), code(g7), code(g8));
        ID3D11Query_Release(x);
    }
    D3D11_QUERY_DESC d = {D3D11_QUERY_EVENT, 1};
    ID3D11Query *x = NULL;
    printf("query misc1 %s\n", code(ID3D11Device_CreateQuery(dev, &d, &x)));
    if (x) ID3D11Query_Release(x);
    printf("query null_desc %s\n", code(ID3D11Device_CreateQuery(dev, NULL, &x)));
    d.MiscFlags = 0;
    printf("query null_out %s\n", code(ID3D11Device_CreateQuery(dev, &d, NULL)));
}

/* the reference counts the device shows: children, the immediate context, views of resources */
static void refcounts(void) {
    ULONG d0 = refs(dev);
    ID3D11Buffer *b = NULL;
    D3D11_BUFFER_DESC bd = {16, D3D11_USAGE_DEFAULT, 8, 0, 0, 0};
    ID3D11Device_CreateBuffer(dev, &bd, NULL, &b);
    ULONG d1 = refs(dev);
    ID3D11Device *g = NULL;
    ID3D11Buffer_GetDevice(b, &g);
    ULONG d2 = refs(dev);
    ID3D11Device_Release(g);
    ID3D11DeviceContext *c = NULL;
    ID3D11Device_GetImmediateContext(dev, &c);
    ULONG d3 = refs(dev), c3 = refs(c);
    ID3D11DeviceContext_Release(c);
    ID3D11Buffer_Release(b);
    ULONG d4 = refs(dev);
    printf("refs device %lu buffer %lu getdevice %lu context %lu (context %lu) released %lu\n", d0, d1, d2, d3, c3, d4);
    /* a bound resource the application released: still there for the context */
    ID3D11Device_CreateBuffer(dev, &bd, NULL, &b);
    ID3D11ShaderResourceView *v = NULL;
    D3D11_SHADER_RESOURCE_VIEW_DESC vd = {DXGI_FORMAT_R32_FLOAT, D3D11_SRV_DIMENSION_BUFFER};
    vd.Buffer.NumElements = 4;
    ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)b, &vd, &v);
    ULONG br = refs(b), dv = refs(dev);
    ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &v);
    ULONG vr = refs(v), dvb = refs(dev);
    ID3D11ShaderResourceView_Release(v);
    ID3D11ShaderResourceView *back = NULL;
    ID3D11DeviceContext_PSGetShaderResources(ctx, 0, 1, &back);
    ULONG vr2 = back ? refs(back) : 0;
    ID3D11Resource *res = NULL;
    if (back) ID3D11ShaderResourceView_GetResource(back, &res);
    printf("refs view_of_buffer buffer %lu device %lu bound view %lu device %lu got_back %lu resource_is_buffer %d\n", br, dv, vr, dvb, vr2, res == (ID3D11Resource *)b);
    if (res) ID3D11Resource_Release(res);
    if (back) ID3D11ShaderResourceView_Release(back);
    ID3D11DeviceContext_ClearState(ctx);
    ID3D11Buffer_Release(b);
    printf("refs end device %lu\n", refs(dev));
}

/* a 2D texture's IDXGISurface faces (AdvHD asks for IDXGISurface2 to make a Direct2D bitmap of
 * its picture): one level of one layer has them, GetResource gives the texture back */
static void surfaces(void) {
    struct { const char *name; UINT mips, layers; } t[] = { { "plain", 1, 1 }, { "mips", 2, 1 }, { "array", 1, 2 } };
    for (int i = 0; i < 3; i++) {
        ID3D11Texture2D *tex = NULL;
        HRESULT hr = tex2d(DXGI_FORMAT_B8G8R8A8_UNORM, 64, 32, t[i].mips, t[i].layers, D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET, D3D11_USAGE_DEFAULT, 0, 0, 1, &tex);
        if (FAILED(hr)) { printf("surface %s texture %s\n", t[i].name, code(hr)); continue; }
        IDXGISurface *s1 = NULL;
        IDXGISurface2 *s2 = NULL;
        HRESULT h1 = ID3D11Texture2D_QueryInterface(tex, &IID_IDXGISurface, (void **)&s1);
        HRESULT h2 = ID3D11Texture2D_QueryInterface(tex, &IID_IDXGISurface2, (void **)&s2);
        printf("surface %s IDXGISurface %s IDXGISurface2 %s same %d refs %lu\n", t[i].name, code(h1), code(h2), s1 && s2 && (void *)s1 == (void *)s2, refs(tex));
        if (s2) {
            ID3D11Texture2D *back = NULL;
            ID3D11Resource *res = NULL;
            UINT sub = 77, sub2 = 77;
            HRESULT g1 = IDXGISurface2_GetResource(s2, &IID_ID3D11Texture2D, (void **)&back, &sub);
            HRESULT g2 = IDXGISurface2_GetResource(s2, &IID_ID3D11Resource, (void **)&res, &sub2);
            DXGI_SURFACE_DESC d = { 0 };
            HRESULT gd = IDXGISurface2_GetDesc(s2, &d);
            printf("surface %s GetResource %s same %d sub %u resource %s same %d sub %u refs %lu desc %s %ux%u fmt %u\n", t[i].name, code(g1), back == tex, sub,
                   code(g2), res == (ID3D11Resource *)tex, sub2, refs(tex), code(gd), d.Width, d.Height, d.Format);
            if (back) ID3D11Texture2D_Release(back);
            if (res) ID3D11Resource_Release(res);
            IDXGISurface2_Release(s2);
        }
        if (s1) IDXGISurface_Release(s1);
        printf("surface %s released refs %lu\n", t[i].name, refs(tex));
        ID3D11Texture2D_Release(tex);
    }
}

int main(void) {
    D3D_FEATURE_LEVEL fl = D3D_FEATURE_LEVEL_11_0, got = 0;
    HRESULT hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, &fl, 1, D3D11_SDK_VERSION, NULL, NULL, NULL);
    printf("create check %s\n", code(hr));
    hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, &fl, 1, D3D11_SDK_VERSION, NULL, &got, NULL);
    printf("create level_only %s %#x\n", code(hr), got);
    ID3D11Device *x = (void *)1;
    hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_UNKNOWN, NULL, 0, &fl, 1, D3D11_SDK_VERSION, &x, NULL, NULL);
    printf("create unknown_no_adapter %s %d\n", code(hr), x == NULL);
    hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, &fl, 1, D3D11_SDK_VERSION, &dev, NULL, &ctx);
    if (FAILED(hr)) {
        printf("D3D11CreateDevice failed %#lx\n", (unsigned long)hr);
        return 1;
    }
    printf("create device refs %lu context refs %lu\n", refs(dev), refs(ctx));
    formats();
    tex_descs();
    buffers();
    views();
    queries();
    refcounts();
    surfaces();
    ID3D11DeviceContext_Release(ctx);
    printf("release device %lu\n", ID3D11Device_Release(dev));
    return 0;
}
