/*
 * vnhost's use of vkd3d-shader (Wine's copy, libs/vkd3d): Direct3D shader bytecode to SPIR-V,
 * which crates/vnhost/src/d3d11/shader.rs turns into WGSL with naga.
 *
 * Bindings: descriptor set = the shader stage (0 vertex, 1 pixel); in a set, constant buffer
 * b<n> is binding n, shader resource t<n> 16 + n, sampler s<n> 160 + n, UAV u<n> 180 + n.
 * Vertex shader outputs are matched to the pixel shader's inputs by semantic (a varying map),
 * so each output's location is the pixel shader's input register.
 */
#include <stdlib.h>
#include <string.h>
#include <vkd3d_shader.h>

#define NUM_CB 15
#define NUM_SRV 128
#define NUM_SAMPLER 16
#define NUM_UAV 8

/* source: 0 DXBC (TPF), 1 d3dbc (SM1-3), 2 HLSL (`profile`, entry "main"); stage: 0 vertex, 1 pixel.
 * ps/ps_size: the pixel shader a vertex shader feeds (NULL: locations = output registers).
 * Returns 0 and the SPIR-V in *out (free with vn_shader_free), or <0 with the log in *log. */
int vn_shader_to_spirv( const void *code, size_t size, int source, int stage, const void *ps, size_t ps_size,
                        void **out, size_t *out_size, char **log )
{
    static const enum vkd3d_shader_source_type sources[] =
        { VKD3D_SHADER_SOURCE_DXBC_TPF, VKD3D_SHADER_SOURCE_D3D_BYTECODE, VKD3D_SHADER_SOURCE_HLSL };
    struct vkd3d_shader_resource_binding bindings[NUM_CB + NUM_SRV + NUM_SAMPLER + NUM_UAV], *b = bindings;
    struct vkd3d_shader_varying_map varyings[64];
    struct vkd3d_shader_varying_map_info varying_info = { VKD3D_SHADER_STRUCTURE_TYPE_VARYING_MAP_INFO };
    struct vkd3d_shader_interface_info iface = { VKD3D_SHADER_STRUCTURE_TYPE_INTERFACE_INFO };
    struct vkd3d_shader_spirv_target_info target = { VKD3D_SHADER_STRUCTURE_TYPE_SPIRV_TARGET_INFO };
    struct vkd3d_shader_compile_info info = { VKD3D_SHADER_STRUCTURE_TYPE_COMPILE_INFO };
    struct vkd3d_shader_scan_signature_info vs_sig = { VKD3D_SHADER_STRUCTURE_TYPE_SCAN_SIGNATURE_INFO };
    struct vkd3d_shader_scan_signature_info ps_sig = { VKD3D_SHADER_STRUCTURE_TYPE_SCAN_SIGNATURE_INFO };
    struct vkd3d_shader_code spirv = { 0 };
    unsigned int i, count = 0;
    int ret;

    *out = NULL;
    *out_size = 0;
    if (log) *log = NULL;
    for (i = 0; i < NUM_CB + NUM_SRV + NUM_SAMPLER + NUM_UAV; i++, b++)
    {
        memset( b, 0, sizeof(*b) );
        b->shader_visibility = VKD3D_SHADER_VISIBILITY_ALL;
        b->binding.set = stage;
        b->binding.count = 1;
        if (i < NUM_CB)
        {
            b->type = VKD3D_SHADER_DESCRIPTOR_TYPE_CBV;
            b->register_index = i;
            b->binding.binding = i;
            b->flags = VKD3D_SHADER_BINDING_FLAG_BUFFER;
        }
        else if (i < NUM_CB + NUM_SRV)
        {
            b->type = VKD3D_SHADER_DESCRIPTOR_TYPE_SRV;
            b->register_index = i - NUM_CB;
            b->binding.binding = 16 + b->register_index;
            b->flags = VKD3D_SHADER_BINDING_FLAG_IMAGE | VKD3D_SHADER_BINDING_FLAG_BUFFER;
        }
        else if (i < NUM_CB + NUM_SRV + NUM_SAMPLER)
        {
            b->type = VKD3D_SHADER_DESCRIPTOR_TYPE_SAMPLER;
            b->register_index = i - NUM_CB - NUM_SRV;
            b->binding.binding = 160 + b->register_index;
        }
        else
        {
            b->type = VKD3D_SHADER_DESCRIPTOR_TYPE_UAV;
            b->register_index = i - NUM_CB - NUM_SRV - NUM_SAMPLER;
            b->binding.binding = 180 + b->register_index;
            b->flags = VKD3D_SHADER_BINDING_FLAG_IMAGE | VKD3D_SHADER_BINDING_FLAG_BUFFER;
        }
    }
    iface.bindings = bindings;
    iface.binding_count = b - bindings;
    iface.next = &target;
    target.environment = VKD3D_SHADER_SPIRV_ENVIRONMENT_VULKAN_1_0;

    info.source.code = code;
    info.source.size = size;
    info.source_type = sources[source];
    info.target_type = VKD3D_SHADER_TARGET_SPIRV_BINARY;
    info.log_level = VKD3D_SHADER_LOG_WARNING;
    info.next = &iface;

    if (stage == 0 && ps && source == 0)
    {
        /* the pixel shader's input signature and ours: outputs go to its registers */
        struct vkd3d_shader_compile_info scan = info;
        scan.next = &vs_sig;
        if (!vkd3d_shader_scan( &scan, NULL ))
        {
            scan.source.code = ps;
            scan.source.size = ps_size;
            scan.next = &ps_sig;
            if (!vkd3d_shader_scan( &scan, NULL ) && vs_sig.output.element_count <= 64)
            {
                vkd3d_shader_build_varying_map( &vs_sig.output, &ps_sig.input, &count, varyings );
                varying_info.varying_map = varyings;
                varying_info.varying_count = count;
                target.next = &varying_info;
            }
        }
    }

    ret = vkd3d_shader_compile( &info, &spirv, log );
    if (stage == 0 && ps && source == 0)
    {
        vkd3d_shader_free_scan_signature_info( &vs_sig );
        vkd3d_shader_free_scan_signature_info( &ps_sig );
    }
    if (ret < 0) return ret;
    *out = (void *)spirv.code;
    *out_size = spirv.size;
    return 0;
}

/* HLSL to DXBC (for tests): `profile` like "vs_4_0" */
int vn_hlsl_compile( const char *src, const char *profile, void **out, size_t *out_size, char **log )
{
    struct vkd3d_shader_hlsl_source_info hlsl = { VKD3D_SHADER_STRUCTURE_TYPE_HLSL_SOURCE_INFO };
    struct vkd3d_shader_compile_info info = { VKD3D_SHADER_STRUCTURE_TYPE_COMPILE_INFO };
    struct vkd3d_shader_code dxbc = { 0 };
    int ret;

    hlsl.entry_point = "main";
    hlsl.profile = profile;
    info.next = &hlsl;
    info.source.code = src;
    info.source.size = strlen( src );
    info.source_type = VKD3D_SHADER_SOURCE_HLSL;
    info.target_type = VKD3D_SHADER_TARGET_DXBC_TPF;
    info.log_level = VKD3D_SHADER_LOG_WARNING;
    if ((ret = vkd3d_shader_compile( &info, &dxbc, log )) < 0) return ret;
    *out = (void *)dxbc.code;
    *out_size = dxbc.size;
    return 0;
}

void vn_shader_free( void *p )
{
    free( p );
}

/* a DXBC shader's input (which 0) or output (1) signature: up to `max` elements */
struct vn_sig_element
{
    char name[32];
    unsigned int index, reg, sysval, component_type, mask;
};

int vn_shader_signature( const void *code, size_t size, int which, struct vn_sig_element *out, int max )
{
    struct vkd3d_shader_scan_signature_info sig = { VKD3D_SHADER_STRUCTURE_TYPE_SCAN_SIGNATURE_INFO };
    struct vkd3d_shader_compile_info info = { VKD3D_SHADER_STRUCTURE_TYPE_COMPILE_INFO };
    const struct vkd3d_shader_signature *s;
    unsigned int i;
    int n = 0;

    info.source.code = code;
    info.source.size = size;
    info.source_type = VKD3D_SHADER_SOURCE_DXBC_TPF;
    info.target_type = VKD3D_SHADER_TARGET_SPIRV_BINARY;
    info.next = &sig;
    if (vkd3d_shader_scan( &info, NULL )) return -1;
    s = which ? &sig.output : &sig.input;
    for (i = 0; i < s->element_count && n < max; i++, n++)
    {
        const struct vkd3d_shader_signature_element *e = &s->elements[i];
        strncpy( out[n].name, e->semantic_name, sizeof(out[n].name) - 1 );
        out[n].name[sizeof(out[n].name) - 1] = 0;
        out[n].index = e->semantic_index;
        out[n].reg = e->register_index;
        out[n].sysval = e->sysval_semantic;
        out[n].component_type = e->component_type;
        out[n].mask = e->mask;
    }
    vkd3d_shader_free_scan_signature_info( &sig );
    return n;
}
