/* D3DX 9 effects from source (d3dx9_43): D3DXCreateEffect on .fx text (sampler_state, a texture
 * parameter, annotations, a technique with render states and compile vs_2_0/ps_2_0), then
 * parameters by name and semantic, annotations, and the device calls BeginPass makes (through a
 * stand-in device that prints them). A compile error comes back in the errors buffer.
 * libs: -ld3dx9_43 */
#define COBJMACROS
#include <windows.h>
#include <d3d9.h>
#include <d3dx9.h>
#include <stdio.h>

/* a stand-in IDirect3DDevice9 (and its shaders and state block): only the slots effects use */
static ULONG WINAPI addref(void *d) { return 2; }
static ULONG WINAPI release(void *d) { return 1; }
static void *ovt[8] = { 0, addref, release };
static struct { void **vt; int id; } objs[8];
static int nobj;
static void *obj(int id) { objs[nobj].vt = ovt; objs[nobj].id = id; return &objs[nobj++]; }
static int id_of(void *o) { return o ? ((int *)o)[1] : 0; }
static HRESULT WINAPI apply(void *b) { printf("  stateblock apply\n"); return S_OK; }
static HRESULT WINAPI create_sb(void *d, D3DSTATEBLOCKTYPE t, void **out) { printf("  CreateStateBlock %u\n", t); *out = obj(99); ovt[5] = apply; return S_OK; }
static HRESULT WINAPI create_vs(void *d, const DWORD *code, void **out) { printf("  CreateVertexShader %08lx\n", code[0]); *out = obj(nobj + 1); return S_OK; }
static HRESULT WINAPI create_ps(void *d, const DWORD *code, void **out) { printf("  CreatePixelShader %08lx\n", code[0]); *out = obj(nobj + 1); return S_OK; }
static HRESULT WINAPI set_vs(void *d, void *s) { printf("  SetVertexShader %d\n", id_of(s)); return S_OK; }
static HRESULT WINAPI set_ps(void *d, void *s) { printf("  SetPixelShader %d\n", id_of(s)); return S_OK; }
static HRESULT WINAPI set_rs(void *d, D3DRENDERSTATETYPE s, DWORD v) { printf("  SetRenderState %u %lu\n", s, v); return S_OK; }
static HRESULT WINAPI set_samp(void *d, DWORD i, D3DSAMPLERSTATETYPE s, DWORD v) { printf("  SetSamplerState %lu %u %lu\n", i, s, v); return S_OK; }
static HRESULT WINAPI set_tex(void *d, DWORD i, void *t) { printf("  SetTexture %lu %d\n", i, id_of(t)); return S_OK; }
static HRESULT WINAPI setf(void *d, UINT start, const float *v, UINT n, const char *k)
{
    UINT i;
    for (i = 0; i < n; i++)
        printf("  %sF c%u = %g %g %g %g\n", k, start + i, v[4 * i], v[4 * i + 1], v[4 * i + 2], v[4 * i + 3]);
    return S_OK;
}
static HRESULT WINAPI vsf(void *d, UINT s, const float *v, UINT n) { return setf(d, s, v, n, "vs"); }
static HRESULT WINAPI psf(void *d, UINT s, const float *v, UINT n) { return setf(d, s, v, n, "ps"); }
static void *vt[120] = { 0, addref, release };
static struct { void **vt; } dev = { vt };

static const char src[] =
    "float4x4 WorldViewProj : WORLDVIEWPROJECTION;\n"
    "float Alpha <string UIName = \"alpha\"; float UIMin = 0.25;> = 0.5;\n"
    "float4 Tint = { 1, 0.5, 0.25, 1 };\n"
    "texture Tex;\n"
    "sampler2D Samp = sampler_state { Texture = <Tex>; MinFilter = LINEAR; MagFilter = Point; AddressU = Clamp; };\n"
    "struct VO { float4 pos : POSITION; float2 uv : TEXCOORD0; };\n"
    "VO vs(float4 p : POSITION, float2 uv : TEXCOORD0) { VO o; o.pos = mul(p, WorldViewProj); o.uv = uv; return o; }\n"
    "float4 ps(float2 uv : TEXCOORD0, uniform float k) : COLOR\n"
    "{ float4 c = tex2D(Samp, uv) * Tint; c.a *= Alpha * k; return c; }\n"
    "technique Main <string Desc = \"main\";> {\n"
    "  pass P0 {\n"
    "    AlphaBlendEnable = TRUE; SrcBlend = SrcAlpha; DestBlend = InvSrcAlpha; ZEnable = false;\n"
    "    VertexShader = compile vs_2_0 vs();\n"
    "    PixelShader = compile ps_2_0 ps(2.0);\n"
    "  }\n"
    "}\n";

int main(void)
{
    ID3DXEffect *fx = NULL;
    ID3DXBuffer *err = NULL;
    D3DXPARAMETER_DESC pd;
    D3DXTECHNIQUE_DESC td;
    D3DXHANDLE h, t;
    D3DXMATRIX m;
    LPCSTR s;
    float f;
    UINT passes, i;
    HRESULT hr;
    vt[57] = set_rs; vt[59] = create_sb; vt[65] = set_tex; vt[69] = set_samp; vt[91] = create_vs; vt[92] = set_vs;
    vt[94] = vsf; vt[106] = create_ps; vt[107] = set_ps; vt[109] = psf;

    printf("create\n");
    hr = D3DXCreateEffect((IDirect3DDevice9 *)&dev, src, sizeof(src) - 1, NULL, NULL, 0, NULL, &fx, &err);
    printf("D3DXCreateEffect %08lx\n", hr);
    if (FAILED(hr)) {
        if (err) printf("%s\n", (char *)ID3DXBuffer_GetBufferPointer(err));
        return 1;
    }
    h = fx->lpVtbl->GetParameterBySemantic(fx, NULL, "WORLDVIEWPROJECTION");
    fx->lpVtbl->GetParameterDesc(fx, h, &pd);
    printf("by semantic: %s class %u type %u %ux%u\n", pd.Name, pd.Class, pd.Type, pd.Rows, pd.Columns);
    h = fx->lpVtbl->GetParameterByName(fx, NULL, "Alpha");
    fx->lpVtbl->GetParameterDesc(fx, h, &pd);
    fx->lpVtbl->GetFloat(fx, h, &f);
    printf("Alpha = %g, %u annotations\n", f, pd.Annotations);
    fx->lpVtbl->GetString(fx, fx->lpVtbl->GetAnnotationByName(fx, h, "UIName"), &s);
    fx->lpVtbl->GetFloat(fx, fx->lpVtbl->GetAnnotationByName(fx, h, "UIMin"), &f);
    printf("UIName %s UIMin %g\n", s, f);
    fx->lpVtbl->SetFloat(fx, h, 0.75f);
    h = fx->lpVtbl->GetParameterByName(fx, NULL, "Samp");
    fx->lpVtbl->GetParameterDesc(fx, h, &pd);
    printf("Samp: class %u type %u\n", pd.Class, pd.Type);

    t = fx->lpVtbl->GetTechniqueByName(fx, "Main");
    fx->lpVtbl->GetTechniqueDesc(fx, t, &td);
    fx->lpVtbl->GetString(fx, fx->lpVtbl->GetAnnotationByName(fx, t, "Desc"), &s);
    printf("technique %s: %u passes, Desc %s, valid %08lx\n", td.Name, td.Passes, s, fx->lpVtbl->ValidateTechnique(fx, t));
    fx->lpVtbl->SetTechnique(fx, t);

    D3DXMatrixIdentity(&m);
    m._41 = 3;
    fx->lpVtbl->SetMatrix(fx, fx->lpVtbl->GetParameterByName(fx, NULL, "WorldViewProj"), &m);
    printf("begin\n");
    fx->lpVtbl->Begin(fx, &passes, 0);
    for (i = 0; i < passes; i++) {
        printf("pass %u\n", i);
        fx->lpVtbl->BeginPass(fx, i);
        fx->lpVtbl->EndPass(fx);
    }
    printf("end\n");
    fx->lpVtbl->End(fx);
    fx->lpVtbl->Release(fx);

    hr = D3DXCreateEffect((IDirect3DDevice9 *)&dev, "technique T { pass { ZEnable = Nope; } }", 41, NULL, NULL, 0, NULL, &fx, &err);
    printf("bad effect %08lx, errors %s\n", hr, err ? "given" : "none");
    return 0;
}
