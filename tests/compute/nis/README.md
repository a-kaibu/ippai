# NVIDIA Image Scaling SDK v1.0.3 (MIT)

`NIS_Main.hlsl` and `NIS_Scaler.h` are the SDK's own files, unchanged
(https://github.com/NVIDIAGameWorks/NVIDIAImageScaling, `NIS/`; MIT license, in their
headers). Games that ship NIS compile them at run time with `D3DCompile` as `cs_5_0`
(天獄のヴィネア: AdvHD, `DX11Effect.arc`), so sake compiles them with hlslc.

`coef_scale.f32` and `coef_usm.f32` are the SDK's `coef_scale` and `coef_usm` tables
(`NIS/NIS_Config.h`, 64 phases x 8 taps, little-endian 32-bit floats): the textures a game
binds as `coef_scaler` / `coef_usm` (2 x 64 texels of `R32G32B32A32_FLOAT`).

Used by crates/hlslc's tests (every variant compiles and translates), tests/compute.mjs
(against Slang's translation on WebGPU) and the winref probe d3d11cs (against Windows).
