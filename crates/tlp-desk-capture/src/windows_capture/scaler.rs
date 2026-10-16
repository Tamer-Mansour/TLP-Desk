//! GPU downscaler: shrinks the captured BGRA texture before CPU readback.
//!
//! The captured texture is copied into our own shader-readable texture (the capture APIs do not
//! guarantee shader bindings on theirs), optionally box-filtered with `GenerateMips` when the
//! reduction exceeds 2x, then drawn into a smaller render target with a full-screen triangle and a
//! bilinear sampler. When the remaining reduction is not exactly 2x (and not 1x) a Lanczos-2
//! pixel shader is used instead: bilinear/box filtering blurs 1-pixel text strokes at ratios
//! such as 1920x1200 -> 1600x1000, Lanczos-2 keeps them crisp (the same taps as the CPU
//! reference `resample_lanczos2`). For an exact 2x reduction the bilinear tap at each output pixel centre lands
//! on the corner shared by four source pixels, i.e. a 2x2 box filter, same as the CPU half path.
//!
//! The shaders are compiled at startup with `D3DCompile` (d3dcompiler_47.dll, part of Windows 10
//! and 11). Any failure while setting up makes the caller fall back to native-size readback.
//!
//! References: Microsoft Learn — "ID3D11DeviceContext::GenerateMips", "D3DCompile",
//! "Direct3D 11 rendering pipeline".

use windows::core::{s, PCSTR};
use windows::Win32::Graphics::Direct3D::Fxc::{
    D3DCompile, D3DCOMPILE_ENABLE_STRICTNESS, D3DCOMPILE_OPTIMIZATION_LEVEL3,
};
use windows::Win32::Graphics::Direct3D::{
    ID3DBlob, ID3DInclude, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D_SRV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Buffer, ID3D11Device, ID3D11DeviceContext, ID3D11PixelShader, ID3D11RenderTargetView,
    ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX,
    D3D11_BUFFER_DESC, D3D11_COMPARISON_NEVER, D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_FLOAT32_MAX,
    D3D11_RESOURCE_MISC_GENERATE_MIPS, D3D11_SAMPLER_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC,
    D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_SUBRESOURCE_DATA, D3D11_TEX2D_SRV,
    D3D11_TEXTURE2D_DESC, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_USAGE_IMMUTABLE, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

use super::d3d::{bgra_desc, create_texture};
use super::platform_err;
use crate::{mip_level_for, mip_size, needs_lanczos, CaptureError};

const SHADER: &str = r#"
Texture2D src_tex : register(t0);
SamplerState src_smp : register(s0);

struct VsOut {
    float4 pos : SV_Position;
    float2 uv : TEXCOORD0;
};

// Full-screen triangle from the vertex id; no vertex buffer or input layout.
VsOut vs_main(uint id : SV_VertexID) {
    VsOut o;
    float2 uv = float2((id << 1) & 2, id & 2);
    o.uv = uv;
    o.pos = float4(uv * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
    return o;
}

float4 ps_main(VsOut i) : SV_Target {
    return src_tex.Sample(src_smp, i.uv);
}

cbuffer Params : register(b0) {
    float2 ratio;   // source pixels per output pixel (>= 1)
    float2 pad;
};

float lanczos2(float x) {
    x = abs(x);
    if (x < 1e-5) return 1.0;
    if (x >= 2.0) return 0.0;
    float px = 3.14159265 * x;
    return 2.0 * sin(px) * sin(px * 0.5) / (px * px);
}

// Lanczos-2 with the kernel stretched by the reduction ratio. Exact `Load`s (no filtering), taps
// clamped to the image, weights normalised: same maths as the CPU reference.
float4 ps_lanczos(VsOut i) : SV_Target {
    uint w, h, levels;
    src_tex.GetDimensions(0, w, h, levels);
    float2 scale = max(ratio, float2(1.0, 1.0));
    float2 centre = i.pos.xy * ratio;          // pos.xy is already (x + 0.5, y + 0.5)
    float2 radius = 2.0 * scale;
    int2 first = (int2)floor(centre - radius);
    int2 last = (int2)ceil(centre + radius);
    int2 maxc = int2((int)w - 1, (int)h - 1);
    float3 acc = float3(0.0, 0.0, 0.0);
    float wsum = 0.0;
    [loop] for (int y = first.y; y <= last.y; y++) {
        float wy = lanczos2((y + 0.5 - centre.y) / scale.y);
        int cy = clamp(y, 0, maxc.y);
        [loop] for (int x = first.x; x <= last.x; x++) {
            float wgt = wy * lanczos2((x + 0.5 - centre.x) / scale.x);
            int cx = clamp(x, 0, maxc.x);
            acc += wgt * src_tex.Load(int3(cx, cy, 0)).rgb;
            wsum += wgt;
        }
    }
    return float4(saturate(acc / wsum), 1.0);
}
"#;

fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob, CaptureError> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source pointer/length describe `SHADER`, which outlives the call; entry and
    // target are NUL-terminated literals; both out-pointers reference live locals.
    let result = unsafe {
        D3DCompile(
            SHADER.as_ptr().cast(),
            SHADER.len(),
            s!("tlp_desk_scale.hlsl"),
            None,
            None::<&ID3DInclude>,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3 | D3DCOMPILE_ENABLE_STRICTNESS,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = result {
        let detail = errors.map(|b| {
            // SAFETY: the error blob holds `GetBufferSize` bytes of ASCII text at
            // `GetBufferPointer` for as long as the blob is alive (it is, for this block).
            let bytes = unsafe {
                std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())
            };
            String::from_utf8_lossy(bytes).into_owned()
        });
        return Err(CaptureError::Platform(format!(
            "D3DCompile: {e} {}",
            detail.unwrap_or_default()
        )));
    }
    code.ok_or_else(|| CaptureError::Platform("D3DCompile returned no code".into()))
}

fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    // SAFETY: a compiled-shader blob owns `GetBufferSize` bytes at `GetBufferPointer`, valid while
    // the blob (borrowed for the returned slice's lifetime) is alive.
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
    }
}

pub(super) struct GpuScaler {
    input: (u32, u32),
    output: (u32, u32),
    /// Our shader-readable copy of the captured image (with mips when `generate_mips`).
    src: ID3D11Texture2D,
    /// All mips of `src`, for `GenerateMips`.
    src_all: Option<ID3D11ShaderResourceView>,
    /// The mip level the pixel shader samples.
    src_view: ID3D11ShaderResourceView,
    target: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    /// Constant buffer with the reduction ratio, present when `ps` is the Lanczos shader.
    params: Option<ID3D11Buffer>,
}

impl GpuScaler {
    pub(super) fn new(
        device: &ID3D11Device,
        input: (u32, u32),
        output: (u32, u32),
    ) -> Result<Self, CaptureError> {
        if input.0 == 0 || input.1 == 0 || output.0 == 0 || output.1 == 0 {
            return Err(CaptureError::Platform("zero-sized scaler".into()));
        }
        let level = mip_level_for(input, output);
        let generate_mips = level > 0;

        let mut src_desc = D3D11_TEXTURE2D_DESC {
            MipLevels: level + 1,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..bgra_desc(input.0, input.1)
        };
        if generate_mips {
            // GenerateMips needs the texture to be a render target too.
            src_desc.BindFlags |= D3D11_BIND_RENDER_TARGET.0 as u32;
            src_desc.MiscFlags = D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32;
        }
        let src = create_texture(device, &src_desc, "CreateTexture2D(scale source)")?;

        let srv = |most_detailed: u32, levels: u32| -> Result<_, CaptureError> {
            let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_SRV {
                        MostDetailedMip: most_detailed,
                        MipLevels: levels,
                    },
                },
            };
            let mut view = None;
            // SAFETY: `src` is a live BGRA texture with `level + 1` mips and shader binding; the
            // requested mip range lies inside it; `view` is a live out-pointer.
            unsafe { device.CreateShaderResourceView(&src, Some(&desc), Some(&mut view)) }
                .map_err(|e| platform_err("CreateShaderResourceView", e))?;
            view.ok_or_else(|| CaptureError::Platform("no shader resource view".into()))
        };
        let src_view = srv(level, 1)?;
        let src_all = if generate_mips {
            Some(srv(0, level + 1)?)
        } else {
            None
        };

        let target_desc = D3D11_TEXTURE2D_DESC {
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..bgra_desc(output.0, output.1)
        };
        let target = create_texture(device, &target_desc, "CreateTexture2D(scale target)")?;
        let mut rtv = None;
        // SAFETY: `target` is a live render-target texture; a null description views mip 0.
        unsafe { device.CreateRenderTargetView(&target, None, Some(&mut rtv)) }
            .map_err(|e| platform_err("CreateRenderTargetView", e))?;
        let rtv = rtv.ok_or_else(|| CaptureError::Platform("no render target view".into()))?;

        let vs_code = compile(s!("vs_main"), s!("vs_4_0"))?;
        let lanczos = needs_lanczos(mip_size(input, level), output);
        let ps_code = if lanczos {
            compile(s!("ps_lanczos"), s!("ps_4_0"))?
        } else {
            compile(s!("ps_main"), s!("ps_4_0"))?
        };
        let params = if lanczos {
            let m = mip_size(input, level);
            let data: [f32; 4] = [
                m.0 as f32 / output.0 as f32,
                m.1 as f32 / output.1 as f32,
                0.0,
                0.0,
            ];
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: 16,
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let init = D3D11_SUBRESOURCE_DATA {
                pSysMem: data.as_ptr().cast(),
                ..Default::default()
            };
            let mut buf = None;
            // SAFETY: `desc` describes a 16-byte immutable constant buffer and `init` points at
            // exactly 16 initialised bytes (`data`) that outlive the call; `buf` is a live
            // out-pointer.
            unsafe { device.CreateBuffer(&desc, Some(&init), Some(&mut buf)) }
                .map_err(|e| platform_err("CreateBuffer(scale params)", e))?;
            Some(buf.ok_or_else(|| CaptureError::Platform("no constant buffer".into()))?)
        } else {
            None
        };
        let (mut vs, mut ps) = (None, None);
        // SAFETY: the bytecode slices come from successful D3DCompile calls for the matching
        // shader stages and stay alive for the calls; no class linkage; live out-pointers.
        unsafe {
            device
                .CreateVertexShader(blob_bytes(&vs_code), None, Some(&mut vs))
                .map_err(|e| platform_err("CreateVertexShader", e))?;
            device
                .CreatePixelShader(blob_bytes(&ps_code), None, Some(&mut ps))
                .map_err(|e| platform_err("CreatePixelShader", e))?;
        }
        let (Some(vs), Some(ps)) = (vs, ps) else {
            return Err(CaptureError::Platform(
                "shader creation returned null".into(),
            ));
        };

        let sampler_desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MipLODBias: 0.0,
            MaxAnisotropy: 1,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            BorderColor: [0.0; 4],
            MinLOD: 0.0,
            MaxLOD: D3D11_FLOAT32_MAX,
        };
        let mut sampler = None;
        // SAFETY: `sampler_desc` is fully initialised; `sampler` is a live out-pointer.
        unsafe { device.CreateSamplerState(&sampler_desc, Some(&mut sampler)) }
            .map_err(|e| platform_err("CreateSamplerState", e))?;
        let sampler = sampler.ok_or_else(|| CaptureError::Platform("no sampler".into()))?;

        Ok(Self {
            input,
            output,
            src,
            src_all,
            src_view,
            target,
            rtv,
            vs,
            ps,
            sampler,
            params,
        })
    }

    pub(super) fn input_size(&self) -> (u32, u32) {
        self.input
    }

    pub(super) fn output_size(&self) -> (u32, u32) {
        self.output
    }

    /// Scales the top-left `input_size` region of `captured` (BGRA8, same device) and returns the
    /// `output_size` render target holding the result. The work is queued on `context`; the
    /// caller's staging copy + Map synchronises with it.
    pub(super) fn run(
        &self,
        context: &ID3D11DeviceContext,
        captured: &ID3D11Texture2D,
    ) -> Result<ID3D11Texture2D, CaptureError> {
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: self.input.0,
            bottom: self.input.1,
            back: 1,
        };
        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: self.output.0 as f32,
            Height: self.output.1 as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        // SAFETY: every resource, view, shader and state object was created on the device that
        // owns `context` (the capturer creates the scaler on its own device, and `captured` comes
        // from that device's capture pool / duplication). `captured` is BGRA8 and at least
        // `input` in size, matching mip 0 of `src`. The context is used from one thread at a time
        // (the owning capturer's `&mut self`). Views are unbound again before returning so the
        // next frame's copy into `src` does not hit a bound-resource hazard.
        unsafe {
            context.CopySubresourceRegion(&self.src, 0, 0, 0, 0, captured, 0, Some(&region));
            if let Some(all) = &self.src_all {
                context.GenerateMips(all);
            }
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(&self.vs, None);
            context.PSSetShader(&self.ps, None);
            context.PSSetShaderResources(0, Some(&[Some(self.src_view.clone())]));
            context.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            if let Some(params) = &self.params {
                context.PSSetConstantBuffers(0, Some(&[Some(params.clone())]));
            }
            context.RSSetState(None);
            context.OMSetBlendState(None, None, u32::MAX);
            context.RSSetViewports(Some(&[viewport]));
            context.OMSetRenderTargets(Some(&[Some(self.rtv.clone())]), None);
            context.Draw(3, 0);
            context.PSSetShaderResources(0, Some(&[None]));
            context.OMSetRenderTargets(None, None);
        }
        Ok(self.target.clone())
    }
}
