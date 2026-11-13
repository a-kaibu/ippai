float4 Tint = { 1, 0.5, 0.25, 1 };
float4 ps(float2 uv : TEXCOORD0) : COLOR { return Tint; }
technique Main {
  pass P0 { ZEnable = false; PixelShader = compile ps_2_0 ps(); }
  pass P1 { AlphaBlendEnable = TRUE; }
}
