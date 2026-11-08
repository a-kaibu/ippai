/* D3D10ReflectShader (#59) over what D3D10CompileShader makes of a vertex and a pixel shader
 * (vs_4_0 / ps_4_0): the shader's description, its constant buffers, their variables and
 * types (struct members too), the bound resources, the input and output parameters. Each
 * description is written over a buffer of 0xcc bytes: what lies past the D3D10 struct must stay
 * (D3D11's are longer). Sizes of blobs and instruction counts are fxc's own: not printed.
 * libs: -ld3d10 */
#define COBJMACROS
#include "../probe.h"
#include <initguid.h>
#include <d3d10.h>
#include <d3d10shader.h>

#define M(o, m, ...) ((o)->lpVtbl->m((o), __VA_ARGS__))
#define M0(o, m) ((o)->lpVtbl->m(o))

static const char src[] =
    "struct Light { float3 dir; float power; float4 color[2]; };\n"
    "cbuffer Scene : register(b1) { row_major float4x4 wvp; Light light; int mode[2]; };\n"
    "cbuffer Unused : register(b3) { float4 nothing; };\n"
    "Texture2D tex : register(t2); SamplerState smp : register(s1);\n"
    "struct VI { float3 pos : POSITION; float3 n : NORMAL; float2 uv : TEXCOORD0; };\n"
    "struct VO { float4 pos : SV_Position; float3 n : NORMAL; float2 uv : TEXCOORD0; };\n"
    "VO vs(VI i) { VO o; o.pos = mul(float4(i.pos, 1), wvp); o.n = i.n * light.power; o.uv = i.uv; return o; }\n"
    "float4 ps(VO i) : SV_Target {\n"
    "  float d = saturate(dot(normalize(i.n), light.dir));\n"
    "  float4 c = tex.Sample(smp, i.uv) * d * light.color[1];\n"
    "  if (mode[1] == 2) c = c.bgra;\n"
    "  return c; }\n";

static unsigned char buf[512];
static void *fresh(void) {
    memset(buf, 0xcc, sizeof buf);
    return buf;
}
/* (bytes past `size` untouched) */
static int kept(size_t size) {
    for (size_t i = size; i < sizeof buf; i++)
        if (buf[i] != 0xcc) return 0;
    return 1;
}

static void type(ID3D10ShaderReflectionType *t, int depth) {
    D3D10_SHADER_TYPE_DESC *d = fresh();
    HRESULT hr = M(t, GetDesc, d);
    printf("%*stype %#lx: class %d type %d %ux%u elements %u members %u offset %u, past it kept %d\n", depth * 2, "", hr, d->Class, d->Type, d->Rows,
           d->Columns, d->Elements, d->Members, d->Offset, kept(sizeof *d));
    UINT n = d->Members;
    for (UINT i = 0; i < n && i < 8; i++) {
        printf("%*smember %u %s\n", depth * 2, "", i, M(t, GetMemberTypeName, i));
        type(M(t, GetMemberTypeByIndex, i), depth + 1);
    }
}

static void reflect(const char *entry, const char *profile) {
    ID3D10Blob *code = NULL, *err = NULL;
    HRESULT hr = D3D10CompileShader(src, sizeof src - 1, "probe.hlsl", NULL, NULL, entry, profile, 0, &code, &err);
    printf("%s: D3D10CompileShader %#lx\n", entry, hr);
    if (FAILED(hr)) {
        if (err) printf("%s\n", (const char *)ID3D10Blob_GetBufferPointer(err));
        return;
    }
    ID3D10ShaderReflection *r = NULL;
    hr = D3D10ReflectShader(ID3D10Blob_GetBufferPointer(code), ID3D10Blob_GetBufferSize(code), &r);
    printf("D3D10ReflectShader %#lx\n", hr);
    if (FAILED(hr)) return;
    D3D10_SHADER_DESC *d = fresh();
    hr = M(r, GetDesc, d);
    printf("desc %#lx: version %#x creator %d cbuffers %u resources %u inputs %u outputs %u, past it kept %d\n", hr, d->Version,
           d->Creator != NULL, d->ConstantBuffers, d->BoundResources, d->InputParameters, d->OutputParameters, kept(sizeof *d));
    D3D10_SHADER_DESC sd = *d;
    for (UINT i = 0; i < sd.ConstantBuffers; i++) {
        ID3D10ShaderReflectionConstantBuffer *cb = M(r, GetConstantBufferByIndex, i);
        D3D10_SHADER_BUFFER_DESC *bd = fresh();
        hr = M(cb, GetDesc, bd);
        printf("cbuffer %#lx %s: type %d vars %u size %u flags %u, past it kept %d\n", hr, bd->Name, bd->Type, bd->Variables, bd->Size, bd->uFlags,
               kept(sizeof *bd));
        printf("  by name same %d\n", M(r, GetConstantBufferByName, bd->Name) == cb);
        UINT nv = bd->Variables;
        for (UINT k = 0; k < nv; k++) {
            ID3D10ShaderReflectionVariable *v = M(cb, GetVariableByIndex, k);
            D3D10_SHADER_VARIABLE_DESC *vd = fresh();
            hr = M(v, GetDesc, vd);
            printf("  var %#lx %s: offset %u size %u flags %u default %d, past it kept %d\n", hr, vd->Name, vd->StartOffset, vd->Size, vd->uFlags,
                   vd->DefaultValue != NULL, kept(sizeof *vd));
            type(M0(v, GetType), 2);
        }
        /* (a name it does not have: an object whose GetDesc fails) */
        D3D10_SHADER_VARIABLE_DESC vd;
        printf("  missing variable: GetDesc failed %d\n", FAILED(M(M(cb, GetVariableByName, "none"), GetDesc, &vd)));
    }
    D3D10_SHADER_BUFFER_DESC bd;
    printf("cbuffer index %u: GetDesc failed %d\n", sd.ConstantBuffers, FAILED(M(M(r, GetConstantBufferByIndex, sd.ConstantBuffers), GetDesc, &bd)));
    for (UINT i = 0; i < sd.BoundResources; i++) {
        D3D10_SHADER_INPUT_BIND_DESC *b = fresh();
        hr = M(r, GetResourceBindingDesc, i, b);
        printf("resource %#lx %s: type %d point %u count %u flags %u return %d dimension %d samples %d, past it kept %d\n", hr, b->Name, b->Type,
               b->BindPoint, b->BindCount, b->uFlags, b->ReturnType, b->Dimension, (int)b->NumSamples, kept(sizeof *b));
    }
    for (int out = 0; out < 2; out++) {
        UINT n = out ? sd.OutputParameters : sd.InputParameters;
        for (UINT i = 0; i < n; i++) {
            D3D10_SIGNATURE_PARAMETER_DESC *p = fresh();
            hr = out ? M(r, GetOutputParameterDesc, i, p) : M(r, GetInputParameterDesc, i, p);
            printf("%s %#lx %s%u: register %u system value %d component %d mask %#x, past it kept %d\n", out ? "output" : "input", hr, p->SemanticName,
                   p->SemanticIndex, p->Register, p->SystemValueType, p->ComponentType, p->Mask, kept(sizeof *p));
        }
    }
    D3D10_SIGNATURE_PARAMETER_DESC p;
    printf("input %u: %#lx\n", sd.InputParameters, M(r, GetInputParameterDesc, sd.InputParameters, &p));
    printf("released %lu\n", M0(r, Release));
    ID3D10Blob_Release(code);
}

int main(void) {
    reflect("vs", "vs_4_0");
    reflect("ps", "ps_4_0");
    ID3D10ShaderReflection *r = (void *)1;
    static const unsigned char junk[64] = { 1, 2, 3 };
    HRESULT hr = D3D10ReflectShader(junk, sizeof junk, &r);
    printf("junk: failed %d\n", FAILED(hr));
    return 0;
}
