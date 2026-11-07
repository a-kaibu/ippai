struct SLANG_ParameterGroup_cb_std140_0
{
    @align(16) kDetectRatio_0 : f32,
    @align(4) kDetectThres_0 : f32,
    @align(8) kMinContrastRatio_0 : f32,
    @align(4) kRatioNorm_0 : f32,
    @align(16) kContrastBoost_0 : f32,
    @align(4) kEps_0 : f32,
    @align(8) kSharpStartY_0 : f32,
    @align(4) kSharpScaleY_0 : f32,
    @align(16) kSharpStrengthMin_0 : f32,
    @align(4) kSharpStrengthScale_0 : f32,
    @align(8) kSharpLimitMin_0 : f32,
    @align(4) kSharpLimitScale_0 : f32,
    @align(16) kScaleX_0 : f32,
    @align(4) kScaleY_0 : f32,
    @align(8) kDstNormX_0 : f32,
    @align(4) kDstNormY_0 : f32,
    @align(16) kSrcNormX_0 : f32,
    @align(4) kSrcNormY_0 : f32,
    @align(8) kInputViewportOriginX_0 : u32,
    @align(4) kInputViewportOriginY_0 : u32,
    @align(16) kInputViewportWidth_0 : u32,
    @align(4) kInputViewportHeight_0 : u32,
    @align(8) kOutputViewportOriginX_0 : u32,
    @align(4) kOutputViewportOriginY_0 : u32,
    @align(16) kOutputViewportWidth_0 : u32,
    @align(4) kOutputViewportHeight_0 : u32,
    @align(8) reserved0_0 : f32,
    @align(4) reserved1_0 : f32,
};

@binding(0) @group(0) var<uniform> cb_0 : SLANG_ParameterGroup_cb_std140_0;
@binding(16) @group(0) var in_texture_0 : texture_2d<f32>;

@binding(160) @group(0) var samplerLinearClamp_0 : sampler;

@binding(200) @group(0) var out_texture_0 : texture_storage_2d<rgba8unorm, write>;

var<workgroup> shPixelsY_0 : array<array<f32, i32(38)>, i32(38)>;

fn getY_0( rgba_0 : vec3<f32>) -> f32
{
    return 0.2125999927520752f * rgba_0.x + 0.71520000696182251f * rgba_0.y + 0.07220000028610229f * rgba_0.z;
}

fn CalcLTIFast_0( y_0 : array<f32, i32(5)>) -> f32
{
    var a_cont_0 : f32 = max(max(y_0[i32(0)], y_0[i32(1)]), y_0[i32(2)]) - min(min(y_0[i32(0)], y_0[i32(1)]), y_0[i32(2)]);
    var b_cont_0 : f32 = max(max(y_0[i32(2)], y_0[i32(3)]), y_0[i32(4)]) - min(min(y_0[i32(2)], y_0[i32(3)]), y_0[i32(4)]);
    return (1.0f - saturate((max(a_cont_0, b_cont_0) / (min(a_cont_0, b_cont_0) + cb_0.kEps_0) - cb_0.kMinContrastRatio_0) * cb_0.kRatioNorm_0)) * cb_0.kContrastBoost_0;
}

fn EvalUSM_0( pxl_0 : array<f32, i32(5)>,  sharpnessStrength_0 : f32,  sharpnessLimit_0 : f32) -> f32
{
    return min(sharpnessLimit_0, max(- sharpnessLimit_0, (-0.60009998083114624f * pxl_0[i32(1)] + 1.20019996166229248f * pxl_0[i32(2)] - 0.60009998083114624f * pxl_0[i32(3)]) * sharpnessStrength_0)) * CalcLTIFast_0(pxl_0);
}

fn GetDirUSM_0( p_0 : array<array<f32, i32(5)>, i32(5)>) -> vec4<f32>
{
    var scaleY_0 : f32 = 1.0f - saturate((p_0[i32(2)][i32(2)] - cb_0.kSharpStartY_0) * cb_0.kSharpScaleY_0);
    var sharpnessStrength_1 : f32 = scaleY_0 * cb_0.kSharpStrengthScale_0 + cb_0.kSharpStrengthMin_0;
    var sharpnessLimit_1 : f32 = (scaleY_0 * cb_0.kSharpLimitScale_0 + cb_0.kSharpLimitMin_0) * p_0[i32(2)][i32(2)];
    var rval_0 : vec4<f32>;
    var interp0Deg_0 : array<f32, i32(5)>;
    var i_0 : i32 = i32(0);
    for(;;)
    {
        if(i_0 < i32(5))
        {
        }
        else
        {
            break;
        }
        interp0Deg_0[i_0] = p_0[i_0][i32(2)];
        i_0 = i_0 + i32(1);
    }
    rval_0[i32(0)] = EvalUSM_0(interp0Deg_0, sharpnessStrength_1, sharpnessLimit_1);
    var interp90Deg_0 : array<f32, i32(5)>;
    i_0 = i32(0);
    for(;;)
    {
        if(i_0 < i32(5))
        {
        }
        else
        {
            break;
        }
        interp90Deg_0[i_0] = p_0[i32(2)][i_0];
        i_0 = i_0 + i32(1);
    }
    rval_0[i32(1)] = EvalUSM_0(interp90Deg_0, sharpnessStrength_1, sharpnessLimit_1);
    var interp45Deg_0 : array<f32, i32(5)>;
    interp45Deg_0[i32(0)] = p_0[i32(1)][i32(1)];
    interp45Deg_0[i32(1)] = mix(p_0[i32(2)][i32(1)], p_0[i32(1)][i32(2)], 0.5f);
    interp45Deg_0[i32(2)] = p_0[i32(2)][i32(2)];
    interp45Deg_0[i32(3)] = mix(p_0[i32(3)][i32(2)], p_0[i32(2)][i32(3)], 0.5f);
    interp45Deg_0[i32(4)] = p_0[i32(3)][i32(3)];
    rval_0[i32(2)] = EvalUSM_0(interp45Deg_0, sharpnessStrength_1, sharpnessLimit_1);
    var interp135Deg_0 : array<f32, i32(5)>;
    interp135Deg_0[i32(0)] = p_0[i32(3)][i32(1)];
    interp135Deg_0[i32(1)] = mix(p_0[i32(3)][i32(2)], p_0[i32(2)][i32(1)], 0.5f);
    interp135Deg_0[i32(2)] = p_0[i32(2)][i32(2)];
    interp135Deg_0[i32(3)] = mix(p_0[i32(2)][i32(3)], p_0[i32(1)][i32(2)], 0.5f);
    interp135Deg_0[i32(4)] = p_0[i32(1)][i32(3)];
    rval_0[i32(3)] = EvalUSM_0(interp135Deg_0, sharpnessStrength_1, sharpnessLimit_1);
    return rval_0;
}

fn GetEdgeMap_0( p_1 : array<array<f32, i32(5)>, i32(5)>,  i_1 : i32,  j_0 : i32) -> vec4<f32>
{
    var _S1 : i32 = i32(1) + j_0;
    var _S2 : i32 = i32(2) + j_0;
    var _S3 : i32 = i32(2) + i_1;
    var g_0_0 : f32 = abs(p_1[i_1][j_0] + p_1[i_1][_S1] + p_1[i_1][_S2] - p_1[_S3][j_0] - p_1[_S3][_S1] - p_1[_S3][_S2]);
    var _S4 : i32 = i32(1) + i_1;
    var g_45_0 : f32 = abs(p_1[_S4][j_0] + p_1[i_1][j_0] + p_1[i_1][_S1] - p_1[_S3][_S1] - p_1[_S3][_S2] - p_1[_S4][_S2]);
    var g_90_0 : f32 = abs(p_1[i_1][j_0] + p_1[_S4][j_0] + p_1[_S3][j_0] - p_1[i_1][_S2] - p_1[_S4][_S2] - p_1[_S3][_S2]);
    var g_135_0 : f32 = abs(p_1[_S4][j_0] + p_1[_S3][j_0] + p_1[_S3][_S1] - p_1[i_1][_S1] - p_1[i_1][_S2] - p_1[_S4][_S2]);
    var _S5 : f32 = max(g_0_0, g_90_0);
    var _S6 : f32 = min(g_0_0, g_90_0);
    var _S7 : f32 = max(g_45_0, g_135_0);
    var _S8 : f32 = min(g_45_0, g_135_0);
    var _S9 : f32 = _S5 + _S7;
    if(_S9 == 0.0f)
    {
        return vec4<f32>(0.0f, 0.0f, 0.0f, 0.0f);
    }
    var _S10 : f32 = min(_S5 / _S9, 1.0f);
    var e_45_135_0 : f32 = 1.0f - _S10;
    var c_0_90_0 : bool;
    if(_S5 > (_S6 * cb_0.kDetectRatio_0))
    {
        c_0_90_0 = _S5 > (cb_0.kDetectThres_0);
    }
    else
    {
        c_0_90_0 = false;
    }
    if(c_0_90_0)
    {
        c_0_90_0 = _S5 > _S8;
    }
    else
    {
        c_0_90_0 = false;
    }
    var c_45_135_0 : bool;
    if(_S7 > (_S8 * cb_0.kDetectRatio_0))
    {
        c_45_135_0 = _S7 > (cb_0.kDetectThres_0);
    }
    else
    {
        c_45_135_0 = false;
    }
    if(c_45_135_0)
    {
        c_45_135_0 = _S7 > _S6;
    }
    else
    {
        c_45_135_0 = false;
    }
    var c_g_0_90_0 : bool = _S5 == g_0_0;
    var c_g_45_135_0 : bool = _S7 == g_45_0;
    var _S11 : bool;
    if(c_0_90_0)
    {
        _S11 = c_45_135_0;
    }
    else
    {
        _S11 = false;
    }
    var f_e_0_90_0 : f32;
    if(_S11)
    {
        f_e_0_90_0 = _S10;
    }
    else
    {
        f_e_0_90_0 = 1.0f;
    }
    if(c_0_90_0)
    {
        _S11 = c_45_135_0;
    }
    else
    {
        _S11 = false;
    }
    var f_e_45_135_0 : f32;
    if(_S11)
    {
        f_e_45_135_0 = e_45_135_0;
    }
    else
    {
        f_e_45_135_0 = 1.0f;
    }
    if(c_0_90_0)
    {
        _S11 = c_g_0_90_0;
    }
    else
    {
        _S11 = false;
    }
    var weight_0_0 : f32;
    if(_S11)
    {
        weight_0_0 = f_e_0_90_0;
    }
    else
    {
        weight_0_0 = 0.0f;
    }
    if(c_0_90_0)
    {
        c_0_90_0 = !c_g_0_90_0;
    }
    else
    {
        c_0_90_0 = false;
    }
    var weight_90_0 : f32;
    if(c_0_90_0)
    {
        weight_90_0 = f_e_0_90_0;
    }
    else
    {
        weight_90_0 = 0.0f;
    }
    if(c_45_135_0)
    {
        c_0_90_0 = c_g_45_135_0;
    }
    else
    {
        c_0_90_0 = false;
    }
    var weight_45_0 : f32;
    if(c_0_90_0)
    {
        weight_45_0 = f_e_45_135_0;
    }
    else
    {
        weight_45_0 = 0.0f;
    }
    if(c_45_135_0)
    {
        c_0_90_0 = !c_g_45_135_0;
    }
    else
    {
        c_0_90_0 = false;
    }
    var weight_135_0 : f32;
    if(c_0_90_0)
    {
        weight_135_0 = f_e_45_135_0;
    }
    else
    {
        weight_135_0 = 0.0f;
    }
    return vec4<f32>(weight_0_0, weight_90_0, weight_45_0, weight_135_0);
}

fn NVSharpen_0( blockIdx_0 : vec2<u32>,  threadIdx_0 : u32)
{
    var dy_0 : i32;
    var dx_0 : i32;
    var _S12 : i32 = i32(u32(32) * blockIdx_0.x);
    var _S13 : i32 = i32(u32(32) * blockIdx_0.y);
    var _S14 : i32 = i32(threadIdx_0);
    var i_2 : i32 = _S14 * i32(2);
    for(;;)
    {
        if(i_2 < i32(722))
        {
        }
        else
        {
            break;
        }
        var _S15 : u32 = u32(i_2);
        var _S16 : u32 = _S15 % u32(38);
        var _S17 : u32 = _S15 / u32(38) * u32(2);
        dy_0 = i32(0);
        for(;;)
        {
            if(dy_0 < i32(2))
            {
            }
            else
            {
                break;
            }
            dx_0 = i32(0);
            for(;;)
            {
                if(dx_0 < i32(2))
                {
                }
                else
                {
                    break;
                }
                var _S18 : u32 = u32(dx_0);
                var _S19 : u32 = u32(dy_0);
                shPixelsY_0[_S17 + _S19][_S16 + _S18] = getY_0((textureSampleLevel((in_texture_0), (samplerLinearClamp_0), (vec2<f32>((f32(u32(_S12) + _S16 + _S18) + -1.5f) * cb_0.kSrcNormX_0, (f32(u32(_S13) + _S17 + _S19) + -1.5f) * cb_0.kSrcNormY_0)), (0.0f))).xyz);
                dx_0 = dx_0 + i32(1);
            }
            dy_0 = dy_0 + i32(1);
        }
        i_2 = i_2 + i32(512);
    }
    workgroupBarrier();
    dy_0 = _S14;
    for(;;)
    {
        if(dy_0 < i32(1024))
        {
        }
        else
        {
            break;
        }
        var _S20 : u32 = u32(dy_0);
        var _S21 : i32 = i32(_S20 % u32(32));
        var _S22 : i32 = i32(_S20 / u32(32));
        var p_2 : array<array<f32, i32(5)>, i32(5)>;
        i_2 = i32(0);
        for(;;)
        {
            if(i_2 < i32(5))
            {
            }
            else
            {
                break;
            }
            dx_0 = i32(0);
            for(;;)
            {
                if(dx_0 < i32(5))
                {
                }
                else
                {
                    break;
                }
                p_2[i_2][dx_0] = shPixelsY_0[_S22 + i_2][_S21 + dx_0];
                dx_0 = dx_0 + i32(1);
            }
            i_2 = i_2 + i32(1);
        }
        var dirUSM_0 : vec4<f32> = GetDirUSM_0(p_2);
        var w_0 : vec4<f32> = GetEdgeMap_0(p_2, i32(1), i32(1));
        var usmY_0 : f32 = dirUSM_0.x * w_0.x + dirUSM_0.y * w_0.y + dirUSM_0.z * w_0.z + dirUSM_0.w * w_0.w;
        var _S23 : f32 = f32(_S12 + _S21);
        var _S24 : f32 = f32(_S13 + _S22);
        var dstCoord_0 : vec2<f32> = vec2<f32>(_S23, _S24);
        var op_0 : vec4<f32> = (textureSampleLevel((in_texture_0), (samplerLinearClamp_0), (vec2<f32>((_S23 + 0.5f) * cb_0.kSrcNormX_0, (_S24 + 0.5f) * cb_0.kSrcNormY_0)), (0.0f)));
        op_0[i32(0)] = op_0[i32(0)] + usmY_0;
        op_0[i32(1)] = op_0[i32(1)] + usmY_0;
        op_0[i32(2)] = op_0[i32(2)] + usmY_0;
        textureStore((out_texture_0), (vec2<u32>(dstCoord_0)), (op_0));
        dy_0 = dy_0 + i32(256);
    }
    return;
}

@compute
@workgroup_size(256, 1, 1)
fn main(@builtin(workgroup_id) blockIdx_1 : vec3<u32>, @builtin(local_invocation_id) threadIdx_1 : vec3<u32>)
{
    NVSharpen_0(blockIdx_1.xy, threadIdx_1.x);
    return;
}

