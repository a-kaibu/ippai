# Slang's WGSL of the NIS shader

The NVIDIA Image Scaling shader (`../nis`) translated by Slang 0.43.1 (`libslang-compiler.so`
of the slangpy 0.43.1 wheel) through `../slangwgsl.cpp`, one file per set of defines
(`nis_s<NIS_SCALER>_h<NIS_HDR_MODE>_t<NIS_THREAD_GROUP_SIZE>_p<NIS_USE_HALF_PRECISION>_v<NIS_VIEWPORT_SUPPORT>.wgsl`,
blocks of 32x24 for the scaler, 32x32 for sharpening). An independent HLSL compiler: what
hlslc and dxwgsl make of the same source must store the same bytes (`shaderconf --compute`,
`tests/compute.mjs`, in CI).

Two changes to what Slang wrote: Slang has no `min16float` (defined as `float`, the same as
NIS_USE_HALF_PRECISION gives on D3D11 hardware without half support), and its bindings are
moved to masu's numbering (constant buffers 0+, textures 16+, samplers 160+, typed UAVs 200+,
in group 0) with the UAV a write-only `rgba8unorm` storage texture:

    g++ -O1 -o slangwgsl tests/compute/slangwgsl.cpp -I<slang include> -L<dir of libslang-compiler.so> -lslang-compiler
    LD_LIBRARY_PATH=<that dir> ./slangwgsl tests/compute/nis NIS_Main.hlsl main NIS_SCALER=1 NIS_HDR_MODE=0 \
      NIS_BLOCK_WIDTH=32 NIS_BLOCK_HEIGHT=24 NIS_THREAD_GROUP_SIZE=128 NIS_VIEWPORT_SUPPORT=0 \
      NIS_USE_HALF_PRECISION=0 min16float=float min16float2=float2 min16float3=float3 min16float4=float4

then `@binding(n) @group(0)` of a storage texture +200 (and `read_write` -> `rgba8unorm, write`),
of a sampler +160, of another texture +16.
