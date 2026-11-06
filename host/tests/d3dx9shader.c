/* D3DX 9 shader compiler check (d3dx9_43): D3DXCompileShader to ps_2_0 / vs_2_0 with a
 * constant table, then ID3DXConstantTable: descriptions, handles by name and element, and the
 * registers its Set* calls write (through a stand-in device that prints them). Constants have
 * explicit registers, so the output is what Windows' compiler and d3dx9 give.
 * libs: -ld3dx9_43 */
#include <windows.h>
#include <d3d9.h>
#include <d3dx9.h>
#include <stdio.h>

/* a stand-in IDirect3DDevice9: only the Set*ShaderConstant* slots */
static HRESULT WINAPI setf(void *d, UINT start, const float *v, UINT n, const char *k)
{
    UINT i;
    for (i = 0; i < n; i++)
        printf("  %sF c%u = %g %g %g %g\n", k, start + i, v[4 * i], v[4 * i + 1], v[4 * i + 2], v[4 * i + 3]);
    return S_OK;
}
static HRESULT WINAPI vsf(void *d, UINT s, const float *v, UINT n) { return setf(d, s, v, n, "vs"); }
static HRESULT WINAPI psf(void *d, UINT s, const float *v, UINT n) { return setf(d, s, v, n, "ps"); }
static HRESULT WINAPI psi(void *d, UINT s, const int *v, UINT n) { printf("  psI %u x%u\n", s, n); return S_OK; }
static HRESULT WINAPI psb(void *d, UINT s, const BOOL *v, UINT n) { printf("  psB %u x%u\n", s, n); return S_OK; }
static void *vt[120];
static struct { void **vt; } dev = { vt };

static const char src[] =
    "struct L { float3 dir; float4 col; };\n"
    "L lights[2] : register(c10);\n"
    "float4x4 wvp : register(c0);\n"
    "row_major float4x4 wvp_r : register(c4);\n"
    "float4 tint : register(c8);\n"
    "float scale : register(c9);\n"
    "sampler2D tex : register(s1);\n"
    "float4 ps(float2 uv : TEXCOORD0) : COLOR {\n"
    "  return tex2D(tex, uv) * tint * scale + mul(float4(uv, 0, 1), wvp) + mul(float4(uv, 1, 1), wvp_r)\n"
    "    + lights[1].col + lights[0].dir.x + lights[1].dir.y; }\n"
    "float4 vs(float4 p : POSITION) : POSITION { return mul(p, wvp); }\n";

static void desc(ID3DXConstantTable *t, D3DXHANDLE h, const char *what)
{
    D3DXCONSTANT_DESC d;
    UINT n = 1;
    HRESULT hr = ID3DXConstantTable_GetConstantDesc(t, h, &d, &n);
    if (FAILED(hr)) { printf("%s: desc failed\n", what); return; }
    printf("%s: %s set %u reg %u count %u class %u type %u %ux%u elements %u members %u bytes %u\n", what, d.Name, d.RegisterSet,
           d.RegisterIndex, d.RegisterCount, d.Class, d.Type, d.Rows, d.Columns, d.Elements, d.StructMembers, d.Bytes);
}

int main(void)
{
    ID3DXBuffer *code = NULL, *err = NULL;
    ID3DXConstantTable *t = NULL, *t2 = NULL;
    D3DXCONSTANTTABLE_DESC td;
    D3DXMATRIX m;
    D3DXVECTOR4 v = { 1, 2, 3, 4 };
    float col[4] = { 5, 6, 7, 8 };
    D3DXHANDLE h;
    HRESULT hr;
    int i;
    vt[94] = vsf; vt[109] = psf; vt[111] = psi; vt[113] = psb;
    for (i = 0; i < 16; i++) ((float *)&m)[i] = (float)(i + 1);

    hr = D3DXCompileShader(src, sizeof(src) - 1, NULL, NULL, "ps", "ps_2_0", 0, &code, &err, &t);
    printf("D3DXCompileShader(ps_2_0) -> %#lx code %d table %d\n", (unsigned long)hr, code != NULL, t != NULL);
    if (FAILED(hr)) return 1;
    printf("version %#lx size ok %d\n", (unsigned long)D3DXGetShaderVersion(ID3DXBuffer_GetBufferPointer(code)),
           D3DXGetShaderSize(ID3DXBuffer_GetBufferPointer(code)) == ID3DXBuffer_GetBufferSize(code));
    ID3DXConstantTable_GetDesc(t, &td);
    printf("constants %u version %#lx\n", td.Constants, (unsigned long)td.Version);
    desc(t, ID3DXConstantTable_GetConstantByName(t, NULL, "tint"), "tint");
    desc(t, ID3DXConstantTable_GetConstantByName(t, NULL, "wvp"), "wvp");
    desc(t, ID3DXConstantTable_GetConstantByName(t, NULL, "wvp_r"), "wvp_r");
    desc(t, ID3DXConstantTable_GetConstantByName(t, NULL, "lights"), "lights");
    h = ID3DXConstantTable_GetConstantElement(t, ID3DXConstantTable_GetConstantByName(t, NULL, "lights"), 1);
    desc(t, ID3DXConstantTable_GetConstantByName(t, h, "col"), "lights[1].col");
    desc(t, ID3DXConstantTable_GetConstantByName(t, NULL, "lights[1].dir"), "lights[1].dir");
    desc(t, ID3DXConstantTable_GetConstantByName(t, NULL, "tex"), "tex");
    printf("sampler index %u\n", ID3DXConstantTable_GetSamplerIndex(t, ID3DXConstantTable_GetConstantByName(t, NULL, "tex")));
    printf("missing %d\n", ID3DXConstantTable_GetConstantByName(t, NULL, "nope") == NULL);

    printf("SetVector tint\n");
    ID3DXConstantTable_SetVector(t, (IDirect3DDevice9 *)&dev, "tint", &v);
    printf("SetFloat scale\n");
    ID3DXConstantTable_SetFloat(t, (IDirect3DDevice9 *)&dev, "scale", 0.5f);
    printf("SetMatrix wvp\n");
    ID3DXConstantTable_SetMatrix(t, (IDirect3DDevice9 *)&dev, "wvp", &m);
    printf("SetMatrix wvp_r\n");
    ID3DXConstantTable_SetMatrix(t, (IDirect3DDevice9 *)&dev, "wvp_r", &m);
    printf("SetMatrixTranspose wvp\n");
    ID3DXConstantTable_SetMatrixTranspose(t, (IDirect3DDevice9 *)&dev, "wvp", &m);
    printf("SetFloatArray lights[1].col\n");
    ID3DXConstantTable_SetFloatArray(t, (IDirect3DDevice9 *)&dev, h ? ID3DXConstantTable_GetConstantByName(t, h, "col") : NULL, col, 4);

    hr = D3DXGetShaderConstantTable(ID3DXBuffer_GetBufferPointer(code), &t2);
    printf("D3DXGetShaderConstantTable -> %#lx\n", (unsigned long)hr);
    if (t2) {
        ID3DXConstantTable_GetDesc(t2, &td);
        printf("constants %u\n", td.Constants);
        ID3DXConstantTable_Release(t2);
    }
    ID3DXConstantTable_Release(t);
    ID3DXBuffer_Release(code);

    hr = D3DXCompileShader(src, sizeof(src) - 1, NULL, NULL, "vs", "vs_2_0", 0, &code, &err, &t);
    printf("D3DXCompileShader(vs_2_0) -> %#lx\n", (unsigned long)hr);
    if (SUCCEEDED(hr)) {
        ID3DXConstantTable_GetDesc(t, &td);
        printf("constants %u version %#lx\n", td.Constants, (unsigned long)td.Version);
        printf("SetMatrix wvp\n");
        ID3DXConstantTable_SetMatrix(t, (IDirect3DDevice9 *)&dev, "wvp", &m);
    }
    err = NULL;
    hr = D3DXCompileShader("float4 ps() : COLOR { return oops; }", 36, NULL, NULL, "ps", "ps_2_0", 0, &code, &err, NULL);
    printf("error -> %d errors %d\n", FAILED(hr), err != NULL);
    return 0;
}
