/* HLSL compiler check (d3dcompiler_47): D3DCompile with defines and an ID3DInclude, a syntax
 * error, then D3DReflect of the results: constant buffer layout, bound resources, input
 * signature. Prints only what Windows' compiler gives the same (no sizes of blobs, no output
 * registers: fxc packs those).
 * libs: -ld3dcompiler_47 */
#define COBJMACROS
#include <windows.h>
#include <initguid.h>
#include <d3dcompiler.h>
#include <d3d11shader.h>
#include <stdio.h>
#include <string.h>

/* (mingw has no C macros for the reflection interfaces) */
#define M(o, m, ...) ((o)->lpVtbl->m((o), __VA_ARGS__))
#define M0(o, m) ((o)->lpVtbl->m(o))

static const char common[] = "float4 tint_of(float4 c) { return c * TINT; }\n";

static HRESULT WINAPI inc_open(ID3DInclude *This, D3D_INCLUDE_TYPE type, LPCSTR name, LPCVOID parent, LPCVOID *data, UINT *bytes)
{
    printf("include %s type %d\n", name, type);
    if (strcmp(name, "common.hlsli")) return E_FAIL;
    *data = common;
    *bytes = sizeof(common) - 1;
    return S_OK;
}
static HRESULT WINAPI inc_close(ID3DInclude *This, LPCVOID data) { return S_OK; }
static ID3DIncludeVtbl inc_vtbl = { inc_open, inc_close };
static ID3DInclude inc = { &inc_vtbl };

static const char src[] =
    "#include \"common.hlsli\"\n"
    "cbuffer Scene : register(b1) { row_major float4x4 wvp; float3 light; float power; int mode[2]; };\n"
    "cbuffer Unused : register(b3) { float4 nothing; };\n"
    "Texture2D tex : register(t2); SamplerState smp : register(s1);\n"
    "struct VI { float3 pos : POSITION; float3 n : NORMAL; float2 uv : TEXCOORD0; uint id : SV_VertexID; };\n"
    "struct VO { float4 pos : SV_Position; float3 n : NORMAL; float2 uv : TEXCOORD0; };\n"
    "VO vs(VI i) { VO o; o.pos = mul(float4(i.pos, 1), wvp); o.n = i.n; o.uv = i.uv + (float)i.id; return o; }\n"
    "float4 ps(VO i) : SV_Target {\n"
    "  float d = pow(saturate(dot(normalize(i.n), light)), power);\n"
    "  float4 c = tex.Sample(smp, i.uv) * d;\n"
    "  if (mode[1] == 2) c = c.bgra;\n"
    "  return tint_of(c); }\n";

static void reflect(ID3DBlob *code)
{
    ID3D11ShaderReflection *r;
    D3D11_SHADER_DESC d;
    UINT i, j;
    HRESULT hr = D3DReflect(ID3D10Blob_GetBufferPointer(code), ID3D10Blob_GetBufferSize(code), &IID_ID3D11ShaderReflection, (void **)&r);
    printf("D3DReflect -> %#lx\n", (unsigned long)hr);
    if (FAILED(hr)) return;
    M(r, GetDesc, &d);
    printf("version %#x cbuffers %u resources %u inputs %u\n", d.Version, d.ConstantBuffers, d.BoundResources, d.InputParameters);
    for (i = 0; i < d.ConstantBuffers; i++) {
        ID3D11ShaderReflectionConstantBuffer *cb = M(r, GetConstantBufferByIndex, i);
        D3D11_SHADER_BUFFER_DESC bd;
        M(cb, GetDesc, &bd);
        printf("cbuffer %s size %u vars %u\n", bd.Name, bd.Size, bd.Variables);
        for (j = 0; j < bd.Variables; j++) {
            ID3D11ShaderReflectionVariable *v = M(cb, GetVariableByIndex, j);
            D3D11_SHADER_VARIABLE_DESC vd;
            D3D11_SHADER_TYPE_DESC td;
            M(v, GetDesc, &vd);
            ID3D11ShaderReflectionType *ty = M0(v, GetType);
            M(ty, GetDesc, &td);
            printf("  %s offset %u size %u used %u class %u type %u %ux%u elements %u\n", vd.Name, vd.StartOffset, vd.Size,
                   (vd.uFlags & D3D_SVF_USED) != 0, td.Class, td.Type, td.Rows, td.Columns, td.Elements);
        }
    }
    for (i = 0; i < d.BoundResources; i++) {
        D3D11_SHADER_INPUT_BIND_DESC b;
        M(r, GetResourceBindingDesc, i, &b);
        printf("bind %s type %u point %u count %u\n", b.Name, b.Type, b.BindPoint, b.BindCount);
    }
    for (i = 0; i < d.InputParameters; i++) {
        D3D11_SIGNATURE_PARAMETER_DESC p;
        M(r, GetInputParameterDesc, i, &p);
        printf("input %s%u register %u sysval %u type %u mask %#x\n", p.SemanticName, p.SemanticIndex, p.Register,
               p.SystemValueType, p.ComponentType, p.Mask);
    }
    for (i = 0; i < d.OutputParameters; i++) {
        D3D11_SIGNATURE_PARAMETER_DESC p;
        M(r, GetOutputParameterDesc, i, &p);
        printf("output %s%u sysval %u type %u\n", p.SemanticName, p.SemanticIndex, p.SystemValueType, p.ComponentType);
    }
    {
        D3D11_SHADER_BUFFER_DESC bd;
        ID3D11ShaderReflectionConstantBuffer *cb = M(r, GetConstantBufferByName, "nope");
        hr = M(cb, GetDesc, &bd);
        printf("missing cbuffer -> %s\n", FAILED(hr) ? "failed" : "ok");
    }
    M0(r, Release);
}

static ID3DBlob *compile(const char *s, size_t n, const char *entry, const char *target)
{
    D3D_SHADER_MACRO defs[] = { { "TINT", "float4(1, 0.5, 0.25, 1)" }, { NULL, NULL } };
    ID3DBlob *code = NULL, *err = NULL;
    HRESULT hr = D3DCompile(s, n, "test.hlsl", defs, &inc, entry, target, 0, 0, &code, &err);
    printf("D3DCompile(%s, %s) -> %#lx code %d\n", entry, target, (unsigned long)hr, code != NULL);
    /* (warnings may come with a success) */
    if (FAILED(hr)) printf("errors %d\n", err != NULL);
    if (code) {
        const DWORD *w = ID3D10Blob_GetBufferPointer(code);
        printf("DXBC %d\n", w[0] == 0x43425844);
    }
    return code;
}

int main(void)
{
    ID3DBlob *vs = compile(src, sizeof(src) - 1, "vs", "vs_5_0");
    ID3DBlob *ps = compile(src, sizeof(src) - 1, "ps", "ps_4_0");
    if (vs) reflect(vs);
    if (ps) reflect(ps);
    compile("float4 ps() : SV_Target { return oops; }", 40, "ps", "ps_4_0");
    compile(src, sizeof(src) - 1, "missing", "ps_4_0");
    return 0;
}
