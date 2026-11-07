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

@binding(17) @group(0) var coef_scaler_0 : texture_2d<f32>;

@binding(18) @group(0) var coef_usm_0 : texture_2d<f32>;

@binding(200) @group(0) var out_texture_0 : texture_storage_2d<rgba8unorm, write>;

fn getY_0( rgba_0 : vec3<f32>) -> f32
{
    return 0.2125999927520752f * rgba_0.x + 0.71520000696182251f * rgba_0.y + 0.07220000028610229f * rgba_0.z;
}

var<workgroup> shPixelsY_0 : array<f32, i32(1140)>;

var<workgroup> shEdgeMap_0 : array<vec4<f32>, i32(884)>;

fn GetEdgeMap_0( p_0 : array<array<f32, i32(4)>, i32(4)>,  i_0 : i32,  j_0 : i32) -> vec4<f32>
{
    var _S1 : i32 = i32(1) + j_0;
    var _S2 : i32 = i32(2) + j_0;
    var _S3 : i32 = i32(2) + i_0;
    var g_0_0 : f32 = abs(p_0[i_0][j_0] + p_0[i_0][_S1] + p_0[i_0][_S2] - p_0[_S3][j_0] - p_0[_S3][_S1] - p_0[_S3][_S2]);
    var _S4 : i32 = i32(1) + i_0;
    var g_45_0 : f32 = abs(p_0[_S4][j_0] + p_0[i_0][j_0] + p_0[i_0][_S1] - p_0[_S3][_S1] - p_0[_S3][_S2] - p_0[_S4][_S2]);
    var g_90_0 : f32 = abs(p_0[i_0][j_0] + p_0[_S4][j_0] + p_0[_S3][j_0] - p_0[i_0][_S2] - p_0[_S4][_S2] - p_0[_S3][_S2]);
    var g_135_0 : f32 = abs(p_0[_S4][j_0] + p_0[_S3][j_0] + p_0[_S3][_S1] - p_0[i_0][_S1] - p_0[i_0][_S2] - p_0[_S4][_S2]);
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

var<workgroup> shCoefScaler_0 : array<array<f32, i32(6)>, i32(64)>;

var<workgroup> shCoefUSM_0 : array<array<f32, i32(6)>, i32(64)>;

fn LoadFilterBanksSh_0( i0_0 : i32)
{
    if(i0_0 < i32(128))
    {
        var phase_0 : i32 = (i0_0 >> (u32(1)));
        var vIdx_0 : i32 = (i0_0 & (i32(1)));
        var _S12 : vec3<i32> = vec3<i32>(vec3<u32>(vec2<u32>(vec2<i32>(vIdx_0, phase_0)), u32(0)));
        var _S13 : vec4<f32> = (textureLoad((coef_scaler_0), ((_S12)).xy, ((_S12)).z));
        var filterOffset_0 : i32 = vIdx_0 * i32(4);
        shCoefScaler_0[phase_0][filterOffset_0] = _S13.x;
        var _S14 : i32 = filterOffset_0 + i32(1);
        shCoefScaler_0[phase_0][_S14] = _S13.y;
        var _S15 : bool = vIdx_0 == i32(0);
        if(_S15)
        {
            shCoefScaler_0[phase_0][i32(2)] = _S13.z;
            shCoefScaler_0[phase_0][i32(3)] = _S13.w;
        }
        var _S16 : vec4<f32> = (textureLoad((coef_usm_0), ((_S12)).xy, ((_S12)).z));
        shCoefUSM_0[phase_0][filterOffset_0] = _S16.x;
        shCoefUSM_0[phase_0][_S14] = _S16.y;
        if(_S15)
        {
            shCoefUSM_0[phase_0][i32(2)] = _S16.z;
            shCoefUSM_0[phase_0][i32(3)] = _S16.w;
        }
    }
    return;
}

fn GetInterpEdgeMap_0( edge_0 : array<array<vec4<f32>, i32(2)>, i32(2)>,  phase_frac_x_0 : f32,  phase_frac_y_0 : f32) -> vec4<f32>
{
    var _S17 : vec4<f32> = vec4<f32>(phase_frac_x_0);
    return mix(mix(edge_0[i32(0)][i32(0)], edge_0[i32(0)][i32(1)], _S17), mix(edge_0[i32(1)][i32(0)], edge_0[i32(1)][i32(1)], _S17), vec4<f32>(phase_frac_y_0));
}

fn FilterNormal_0( p_1 : array<array<f32, i32(6)>, i32(6)>,  phase_x_frac_int_0 : i32,  phase_y_frac_int_0 : i32) -> f32
{
    var j_1 : i32 = i32(0);
    var h_acc_0 : f32 = 0.0f;
    for(;;)
    {
        if(j_1 < i32(6))
        {
        }
        else
        {
            break;
        }
        var i_1 : i32 = i32(0);
        var v_acc_0 : f32 = 0.0f;
        for(;;)
        {
            if(i_1 < i32(6))
            {
            }
            else
            {
                break;
            }
            var v_acc_1 : f32 = v_acc_0 + p_1[i_1][j_1] * shCoefScaler_0[phase_y_frac_int_0][i_1];
            i_1 = i_1 + i32(1);
            v_acc_0 = v_acc_1;
        }
        var h_acc_1 : f32 = h_acc_0 + v_acc_0 * shCoefScaler_0[phase_x_frac_int_0][j_1];
        j_1 = j_1 + i32(1);
        h_acc_0 = h_acc_1;
    }
    return h_acc_0;
}

fn CalcLTI_0( p0_0 : f32,  p1_0 : f32,  p2_0 : f32,  p3_0 : f32,  p4_0 : f32,  p5_0 : f32,  phase_index_0 : i32) -> f32
{
    var selector_0 : bool = phase_index_0 <= i32(32);
    var sel_0 : f32;
    if(selector_0)
    {
        sel_0 = p0_0;
    }
    else
    {
        sel_0 = p3_0;
    }
    var _S18 : f32 = min(min(p1_0, p2_0), sel_0);
    var _S19 : f32 = max(max(p1_0, p2_0), sel_0);
    if(selector_0)
    {
        sel_0 = p2_0;
    }
    else
    {
        sel_0 = p5_0;
    }
    var a_cont_0 : f32 = _S19 - _S18;
    var b_cont_0 : f32 = max(max(p3_0, p4_0), sel_0) - min(min(p3_0, p4_0), sel_0);
    return (1.0f - saturate((max(a_cont_0, b_cont_0) / (min(a_cont_0, b_cont_0) + cb_0.kEps_0) - cb_0.kMinContrastRatio_0) * cb_0.kRatioNorm_0)) * cb_0.kContrastBoost_0;
}

fn EvalPoly6_0( pxl_0 : array<f32, i32(6)>,  phase_int_0 : i32) -> f32
{
    var i_2 : i32 = i32(0);
    var y_0 : f32 = 0.0f;
    for(;;)
    {
        if(i_2 < i32(6))
        {
        }
        else
        {
            break;
        }
        var y_1 : f32 = y_0 + shCoefScaler_0[phase_int_0][i_2] * pxl_0[i_2];
        i_2 = i_2 + i32(1);
        y_0 = y_1;
    }
    i_2 = i32(0);
    var y_usm_0 : f32 = 0.0f;
    for(;;)
    {
        if(i_2 < i32(6))
        {
        }
        else
        {
            break;
        }
        var y_usm_1 : f32 = y_usm_0 + shCoefUSM_0[phase_int_0][i_2] * pxl_0[i_2];
        i_2 = i_2 + i32(1);
        y_usm_0 = y_usm_1;
    }
    var y_scale_0 : f32 = 1.0f - saturate((y_0 - cb_0.kSharpStartY_0) * cb_0.kSharpScaleY_0);
    var y_sharpness_limit_0 : f32 = (y_scale_0 * cb_0.kSharpLimitScale_0 + cb_0.kSharpLimitMin_0) * y_0;
    return y_0 + min(y_sharpness_limit_0, max(- y_sharpness_limit_0, y_usm_0 * (y_scale_0 * cb_0.kSharpStrengthScale_0 + cb_0.kSharpStrengthMin_0))) * CalcLTI_0(pxl_0[i32(0)], pxl_0[i32(1)], pxl_0[i32(2)], pxl_0[i32(3)], pxl_0[i32(4)], pxl_0[i32(5)], phase_int_0);
}

fn AddDirFilters_0( p_2 : array<array<f32, i32(6)>, i32(6)>,  phase_x_frac_0 : f32,  phase_y_frac_0 : f32,  phase_x_frac_int_1 : i32,  phase_y_frac_int_1 : i32,  w_0 : vec4<f32>) -> f32
{
    var _S20 : f32 = w_0.x;
    var i_3 : i32;
    var f_0 : f32;
    if(_S20 > 0.0f)
    {
        var interp0Deg_0 : array<f32, i32(6)>;
        i_3 = i32(0);
        for(;;)
        {
            if(i_3 < i32(6))
            {
            }
            else
            {
                break;
            }
            interp0Deg_0[i_3] = mix(p_2[i_3][i32(2)], p_2[i_3][i32(3)], phase_x_frac_0);
            i_3 = i_3 + i32(1);
        }
        f_0 = EvalPoly6_0(interp0Deg_0, phase_y_frac_int_1) * _S20;
    }
    else
    {
        f_0 = 0.0f;
    }
    var _S21 : f32 = w_0.y;
    if(_S21 > 0.0f)
    {
        var interp90Deg_0 : array<f32, i32(6)>;
        i_3 = i32(0);
        for(;;)
        {
            if(i_3 < i32(6))
            {
            }
            else
            {
                break;
            }
            interp90Deg_0[i_3] = mix(p_2[i32(2)][i_3], p_2[i32(3)][i_3], phase_y_frac_0);
            i_3 = i_3 + i32(1);
        }
        f_0 = f_0 + EvalPoly6_0(interp90Deg_0, phase_x_frac_int_1) * _S21;
    }
    else
    {
    }
    var _S22 : f32 = w_0.z;
    var a_0 : f32;
    var b_0 : f32;
    var c_0 : f32;
    var d_0 : f32;
    var pphase_p135_0 : f32;
    if(_S22 > 0.0f)
    {
        var pphase_b45_0 : f32 = 0.5f + 0.5f * (phase_x_frac_0 - phase_y_frac_0);
        var temp_interp45Deg_0 : array<f32, i32(7)>;
        temp_interp45Deg_0[i32(1)] = mix(p_2[i32(2)][i32(1)], p_2[i32(1)][i32(2)], pphase_b45_0);
        temp_interp45Deg_0[i32(3)] = mix(p_2[i32(3)][i32(2)], p_2[i32(2)][i32(3)], pphase_b45_0);
        temp_interp45Deg_0[i32(5)] = mix(p_2[i32(4)][i32(3)], p_2[i32(3)][i32(4)], pphase_b45_0);
        var pphase_b45_1 : f32 = pphase_b45_0 - 0.5f;
        var _S23 : bool = pphase_b45_1 >= 0.0f;
        if(_S23)
        {
            a_0 = p_2[i32(0)][i32(2)];
        }
        else
        {
            a_0 = p_2[i32(2)][i32(0)];
        }
        if(_S23)
        {
            b_0 = p_2[i32(1)][i32(3)];
        }
        else
        {
            b_0 = p_2[i32(3)][i32(1)];
        }
        if(_S23)
        {
            c_0 = p_2[i32(2)][i32(4)];
        }
        else
        {
            c_0 = p_2[i32(4)][i32(2)];
        }
        if(_S23)
        {
            d_0 = p_2[i32(3)][i32(5)];
        }
        else
        {
            d_0 = p_2[i32(5)][i32(3)];
        }
        var _S24 : f32 = abs(pphase_b45_1);
        temp_interp45Deg_0[i32(0)] = mix(p_2[i32(1)][i32(1)], a_0, _S24);
        temp_interp45Deg_0[i32(2)] = mix(p_2[i32(2)][i32(2)], b_0, _S24);
        temp_interp45Deg_0[i32(4)] = mix(p_2[i32(3)][i32(3)], c_0, _S24);
        temp_interp45Deg_0[i32(6)] = mix(p_2[i32(4)][i32(4)], d_0, _S24);
        var interp45Deg_0 : array<f32, i32(6)>;
        var pphase_p45_0 : f32 = phase_x_frac_0 + phase_y_frac_0;
        if(pphase_p45_0 >= 1.0f)
        {
            i_3 = i32(0);
            for(;;)
            {
                if(i_3 < i32(6))
                {
                }
                else
                {
                    break;
                }
                var _S25 : i32 = i_3 + i32(1);
                interp45Deg_0[i_3] = temp_interp45Deg_0[_S25];
                i_3 = _S25;
            }
            pphase_p135_0 = pphase_p45_0 - 1.0f;
        }
        else
        {
            i_3 = i32(0);
            for(;;)
            {
                if(i_3 < i32(6))
                {
                }
                else
                {
                    break;
                }
                interp45Deg_0[i_3] = temp_interp45Deg_0[i_3];
                i_3 = i_3 + i32(1);
            }
            pphase_p135_0 = pphase_p45_0;
        }
        f_0 = f_0 + EvalPoly6_0(interp45Deg_0, i32(pphase_p135_0 * 64.0f)) * _S22;
    }
    else
    {
    }
    var _S26 : f32 = w_0.w;
    if(_S26 > 0.0f)
    {
        var pphase_b135_0 : f32 = 0.5f * (phase_x_frac_0 + phase_y_frac_0);
        var temp_interp135Deg_0 : array<f32, i32(7)>;
        temp_interp135Deg_0[i32(1)] = mix(p_2[i32(3)][i32(1)], p_2[i32(4)][i32(2)], pphase_b135_0);
        temp_interp135Deg_0[i32(3)] = mix(p_2[i32(2)][i32(2)], p_2[i32(3)][i32(3)], pphase_b135_0);
        temp_interp135Deg_0[i32(5)] = mix(p_2[i32(1)][i32(3)], p_2[i32(2)][i32(4)], pphase_b135_0);
        var pphase_b135_1 : f32 = pphase_b135_0 - 0.5f;
        var _S27 : bool = pphase_b135_1 >= 0.0f;
        if(_S27)
        {
            a_0 = p_2[i32(5)][i32(2)];
        }
        else
        {
            a_0 = p_2[i32(3)][i32(0)];
        }
        if(_S27)
        {
            b_0 = p_2[i32(4)][i32(3)];
        }
        else
        {
            b_0 = p_2[i32(2)][i32(1)];
        }
        if(_S27)
        {
            c_0 = p_2[i32(3)][i32(4)];
        }
        else
        {
            c_0 = p_2[i32(1)][i32(2)];
        }
        if(_S27)
        {
            d_0 = p_2[i32(2)][i32(5)];
        }
        else
        {
            d_0 = p_2[i32(0)][i32(3)];
        }
        var _S28 : f32 = abs(pphase_b135_1);
        temp_interp135Deg_0[i32(0)] = mix(p_2[i32(4)][i32(1)], a_0, _S28);
        temp_interp135Deg_0[i32(2)] = mix(p_2[i32(3)][i32(2)], b_0, _S28);
        temp_interp135Deg_0[i32(4)] = mix(p_2[i32(2)][i32(3)], c_0, _S28);
        temp_interp135Deg_0[i32(6)] = mix(p_2[i32(1)][i32(4)], d_0, _S28);
        var interp135Deg_0 : array<f32, i32(6)>;
        var pphase_p135_1 : f32 = 1.0f + (phase_x_frac_0 - phase_y_frac_0);
        if(pphase_p135_1 >= 1.0f)
        {
            i_3 = i32(0);
            for(;;)
            {
                if(i_3 < i32(6))
                {
                }
                else
                {
                    break;
                }
                var _S29 : i32 = i_3 + i32(1);
                interp135Deg_0[i_3] = temp_interp135Deg_0[_S29];
                i_3 = _S29;
            }
            pphase_p135_0 = pphase_p135_1 - 1.0f;
        }
        else
        {
            i_3 = i32(0);
            for(;;)
            {
                if(i_3 < i32(6))
                {
                }
                else
                {
                    break;
                }
                interp135Deg_0[i_3] = temp_interp135Deg_0[i_3];
                i_3 = i_3 + i32(1);
            }
            pphase_p135_0 = pphase_p135_1;
        }
        f_0 = f_0 + EvalPoly6_0(interp135Deg_0, i32(pphase_p135_0 * 64.0f)) * _S26;
    }
    else
    {
    }
    return f_0;
}

fn NVScaler_0( blockIdx_0 : vec2<u32>,  threadIdx_0 : u32)
{
    var k_0 : i32;
    var j_2 : i32;
    var dstBlockX_0 : i32 = i32(u32(32) * blockIdx_0.x);
    var dstBlockY_0 : i32 = i32(u32(24) * blockIdx_0.y);
    var srcBlockStartX_0 : i32 = i32(floor((f32(dstBlockX_0) + 0.5f) * cb_0.kScaleX_0 - 0.5f));
    var srcBlockStartY_0 : i32 = i32(floor((f32(dstBlockY_0) + 0.5f) * cb_0.kScaleY_0 - 0.5f));
    var numTilePixelsX_0 : i32 = i32(ceil((f32(dstBlockX_0 + i32(32)) + 0.5f) * cb_0.kScaleX_0 - 0.5f)) - srcBlockStartX_0 + i32(6) - i32(1);
    var numTilePixelsY_0 : i32 = i32(ceil((f32(dstBlockY_0 + i32(24)) + 0.5f) * cb_0.kScaleY_0 - 0.5f)) - srcBlockStartY_0 + i32(6) - i32(1);
    var numTilePixelsX_1 : i32 = numTilePixelsX_0 + ((numTilePixelsX_0 & (i32(1))));
    var numTilePixelsY_1 : i32 = numTilePixelsY_0 + ((numTilePixelsY_0 & (i32(1))));
    var _S30 : i32 = numTilePixelsX_1 * numTilePixelsY_1;
    var numEdgeMapPixelsX_0 : i32 = numTilePixelsX_1 - i32(6) + i32(2);
    var _S31 : i32 = numEdgeMapPixelsX_0 * (numTilePixelsY_1 - i32(6) + i32(2));
    var _S32 : u32 = threadIdx_0 * u32(2);
    var i_4 : u32 = _S32;
    for(;;)
    {
        if(i_4 < ((u32(_S30) >> (u32(1)))))
        {
        }
        else
        {
            break;
        }
        var _S33 : u32 = u32(numTilePixelsX_1);
        var _S34 : u32 = i_4 / _S33;
        var py_0 : u32 = _S34 * u32(2);
        var px_0 : u32 = i_4 % _S33;
        var _S35 : f32 = (f32(u32(srcBlockStartX_0) + px_0) + -1.5f) * cb_0.kSrcNormX_0;
        var _S36 : f32 = (f32(u32(srcBlockStartY_0) + py_0) + -1.5f) * cb_0.kSrcNormY_0;
        var p_3 : array<array<f32, i32(2)>, i32(2)>;
        j_2 = i32(0);
        for(;;)
        {
            if(j_2 < i32(2))
            {
            }
            else
            {
                break;
            }
            k_0 = i32(0);
            for(;;)
            {
                if(k_0 < i32(2))
                {
                }
                else
                {
                    break;
                }
                p_3[j_2][k_0] = getY_0((textureSampleLevel((in_texture_0), (samplerLinearClamp_0), (vec2<f32>(_S35 + f32(k_0) * cb_0.kSrcNormX_0, _S36 + f32(j_2) * cb_0.kSrcNormY_0)), (0.0f))).xyz);
                k_0 = k_0 + i32(1);
            }
            j_2 = j_2 + i32(1);
        }
        var idx_0 : u32 = py_0 * u32(38) + px_0;
        shPixelsY_0[idx_0] = p_3[i32(0)][i32(0)];
        shPixelsY_0[idx_0 + u32(1)] = p_3[i32(0)][i32(1)];
        var _S37 : u32 = idx_0 + u32(38);
        shPixelsY_0[_S37] = p_3[i32(1)][i32(0)];
        shPixelsY_0[_S37 + u32(1)] = p_3[i32(1)][i32(1)];
        i_4 = i_4 + u32(512);
    }
    workgroupBarrier();
    i_4 = _S32;
    for(;;)
    {
        if(i_4 < ((u32(_S31) >> (u32(1)))))
        {
        }
        else
        {
            break;
        }
        var _S38 : u32 = u32(numEdgeMapPixelsX_0);
        var _S39 : u32 = i_4 / _S38;
        var py_1 : u32 = _S39 * u32(2);
        var px_1 : u32 = i_4 % _S38;
        var edgeMapIdx_0 : u32 = py_1 * u32(34) + px_1;
        var _S40 : u32 = (py_1 + u32(1)) * u32(38) + px_1 + u32(1);
        var p_4 : array<array<f32, i32(4)>, i32(4)>;
        j_2 = i32(0);
        for(;;)
        {
            if(j_2 < i32(4))
            {
            }
            else
            {
                break;
            }
            k_0 = i32(0);
            for(;;)
            {
                if(k_0 < i32(4))
                {
                }
                else
                {
                    break;
                }
                p_4[j_2][k_0] = shPixelsY_0[_S40 + u32(j_2 * i32(38)) + u32(k_0)];
                k_0 = k_0 + i32(1);
            }
            j_2 = j_2 + i32(1);
        }
        shEdgeMap_0[edgeMapIdx_0] = GetEdgeMap_0(p_4, i32(0), i32(0));
        shEdgeMap_0[edgeMapIdx_0 + u32(1)] = GetEdgeMap_0(p_4, i32(0), i32(1));
        var _S41 : u32 = edgeMapIdx_0 + u32(34);
        shEdgeMap_0[_S41] = GetEdgeMap_0(p_4, i32(1), i32(0));
        shEdgeMap_0[_S41 + u32(1)] = GetEdgeMap_0(p_4, i32(1), i32(1));
        i_4 = i_4 + u32(512);
    }
    LoadFilterBanksSh_0(i32(threadIdx_0));
    workgroupBarrier();
    var _S42 : i32 = i32(threadIdx_0 / u32(32));
    var _S43 : f32 = f32(dstBlockX_0 + i32(threadIdx_0 % u32(32)));
    var srcX_0 : f32 = (0.5f + _S43) * cb_0.kScaleX_0 - 0.5f;
    var _S44 : f32 = floor(srcX_0);
    var _S45 : i32 = i32(_S44 - f32(srcBlockStartX_0));
    var fx_0 : f32 = srcX_0 - _S44;
    var _S46 : i32 = i32(fx_0 * 64.0f);
    k_0 = i32(0);
    for(;;)
    {
        if(k_0 < i32(3))
        {
        }
        else
        {
            break;
        }
        var _S47 : f32 = f32(dstBlockY_0 + _S42 + k_0 * i32(8));
        var srcY_0 : f32 = (0.5f + _S47) * cb_0.kScaleY_0 - 0.5f;
        var _S48 : f32 = floor(srcY_0);
        var py_2 : i32 = i32(_S48 - f32(srcBlockStartY_0));
        var fy_0 : f32 = srcY_0 - _S48;
        var fy_int_0 : i32 = i32(fy_0 * 64.0f);
        var _S49 : i32 = py_2 * i32(34) + _S45;
        var edge_1 : array<array<vec4<f32>, i32(2)>, i32(2)>;
        var i_5 : i32 = i32(0);
        for(;;)
        {
            if(i_5 < i32(2))
            {
            }
            else
            {
                break;
            }
            j_2 = i32(0);
            for(;;)
            {
                if(j_2 < i32(2))
                {
                }
                else
                {
                    break;
                }
                edge_1[i_5][j_2] = shEdgeMap_0[_S49 + i_5 * i32(34) + j_2];
                j_2 = j_2 + i32(1);
            }
            i_5 = i_5 + i32(1);
        }
        var _S50 : vec4<f32> = GetInterpEdgeMap_0(edge_1, fx_0, fy_0);
        var _S51 : i32 = py_2 * i32(38) + _S45;
        var p_5 : array<array<f32, i32(6)>, i32(6)>;
        var i_6 : i32 = i32(0);
        for(;;)
        {
            if(i_6 < i32(6))
            {
            }
            else
            {
                break;
            }
            j_2 = i32(0);
            for(;;)
            {
                if(j_2 < i32(6))
                {
                }
                else
                {
                    break;
                }
                p_5[i_6][j_2] = shPixelsY_0[_S51 + i_6 * i32(38) + j_2];
                j_2 = j_2 + i32(1);
            }
            i_6 = i_6 + i32(1);
        }
        var opY_0 : f32 = FilterNormal_0(p_5, _S46, fy_int_0) * (1.0f - _S50.x - _S50.y - _S50.z - _S50.w) + AddDirFilters_0(p_5, fx_0, fy_0, _S46, fy_int_0, _S50);
        var dstCoord_0 : vec2<f32> = vec2<f32>(_S43, _S47);
        var _S52 : vec4<f32> = (textureSampleLevel((in_texture_0), (samplerLinearClamp_0), (vec2<f32>((srcX_0 + 0.5f) * cb_0.kSrcNormX_0, (srcY_0 + 0.5f) * cb_0.kSrcNormY_0)), (0.0f)));
        var op_0 : vec4<f32> = _S52;
        var corr_0 : f32 = opY_0 - getY_0(vec3<f32>(_S52.x, _S52.y, _S52.z));
        op_0[i32(0)] = op_0[i32(0)] + corr_0;
        op_0[i32(1)] = op_0[i32(1)] + corr_0;
        op_0[i32(2)] = op_0[i32(2)] + corr_0;
        textureStore((out_texture_0), (vec2<u32>(dstCoord_0)), (op_0));
        k_0 = k_0 + i32(1);
    }
    return;
}

@compute
@workgroup_size(256, 1, 1)
fn main(@builtin(workgroup_id) blockIdx_1 : vec3<u32>, @builtin(local_invocation_id) threadIdx_1 : vec3<u32>)
{
    NVScaler_0(blockIdx_1.xy, threadIdx_1.x);
    return;
}

