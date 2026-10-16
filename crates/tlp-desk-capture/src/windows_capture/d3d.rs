//! Direct3D 11 helpers: device creation, optional GPU downscale, GPU texture -> CPU BGRA
//! readback.

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;

use super::platform_err;
use super::scaler::GpuScaler;
use crate::{fit_within, BgraFrame, CaptureError};

/// Creates a hardware D3D11 device, on `adapter` if given (required for desktop duplication,
/// which must use the adapter that owns the output), otherwise on the default adapter.
pub(super) fn create_device(
    adapter: Option<&IDXGIAdapter>,
) -> Result<(ID3D11Device, ID3D11DeviceContext), CaptureError> {
    let mut device = None;
    let mut context = None;
    let driver = if adapter.is_some() {
        D3D_DRIVER_TYPE_UNKNOWN
    } else {
        D3D_DRIVER_TYPE_HARDWARE
    };
    // SAFETY: all out-pointers reference live locals; a null feature-level list selects the
    // defaults; `adapter`, when given, is a valid COM interface borrowed for the call.
    unsafe {
        D3D11CreateDevice(
            adapter,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|e| platform_err("D3D11CreateDevice", e))?;
    match (device, context) {
        (Some(d), Some(c)) => Ok((d, c)),
        _ => Err(CaptureError::Platform(
            "D3D11CreateDevice returned no device".into(),
        )),
    }
}

/// Description of a single-mip, non-array BGRA8 2D texture with default usage and no bindings.
pub(super) fn bgra_desc(width: u32, height: u32) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        ..Default::default()
    }
}

pub(super) fn create_texture(
    device: &ID3D11Device,
    desc: &D3D11_TEXTURE2D_DESC,
    what: &str,
) -> Result<ID3D11Texture2D, CaptureError> {
    let mut tex = None;
    // SAFETY: `desc` is a fully initialised texture description and `tex` is a live out-pointer;
    // no initial data is supplied.
    unsafe { device.CreateTexture2D(desc, None, Some(&mut tex)) }
        .map_err(|e| platform_err(what, e))?;
    tex.ok_or_else(|| CaptureError::Platform(format!("{what}: no texture")))
}

/// Turns captured GPU textures into CPU [`BgraFrame`]s: optionally downscales on the GPU
/// ([`GpuScaler`]), then copies into a reused staging texture, maps it and copies the rows into a
/// recycled buffer (one memcpy, no per-frame allocation in steady state).
pub(super) struct StagingReader {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    spare: Option<Vec<u8>>,
    scaler: Option<GpuScaler>,
    /// Size of the source (monitor) region this reader was set up for.
    src_size: (u32, u32),
}

impl StagingReader {
    /// `src_size` is the captured monitor size. With `max_size`, frames are shrunk on the GPU to
    /// [`fit_within`]`(src_size, max_size)`; if the GPU scaler cannot be created the reader falls
    /// back to native-size readback.
    pub(super) fn new(
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        src_size: (u32, u32),
        max_size: Option<(u32, u32)>,
    ) -> Self {
        let scaler = max_size
            .map(|max| fit_within(src_size, max))
            .filter(|&out| out != src_size)
            .and_then(|out| GpuScaler::new(&device, src_size, out).ok());
        Self {
            device,
            context,
            staging: None,
            spare: None,
            scaler,
            src_size,
        }
    }

    /// Size of the frames [`StagingReader::read`] produces.
    pub(super) fn output_size(&self) -> (u32, u32) {
        self.scaler
            .as_ref()
            .map_or(self.src_size, GpuScaler::output_size)
    }

    pub(super) fn recycle(&mut self, buf: Vec<u8>) {
        self.spare = Some(buf);
    }

    fn staging(&mut self, width: u32, height: u32) -> Result<ID3D11Texture2D, CaptureError> {
        if let Some((tex, w, h)) = &self.staging {
            if *w == width && *h == height {
                return Ok(tex.clone());
            }
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..bgra_desc(width, height)
        };
        let tex = create_texture(&self.device, &desc, "CreateTexture2D(staging)")?;
        self.staging = Some((tex.clone(), width, height));
        Ok(tex)
    }

    /// Reads the top-left `width` x `height` region of `src` (BGRA8, at least that large, created
    /// on this reader's device) into a CPU frame, downscaled when a GPU scaler is active.
    pub(super) fn read(
        &mut self,
        src: &ID3D11Texture2D,
        width: u32,
        height: u32,
        timestamp_us: u64,
    ) -> Result<BgraFrame, CaptureError> {
        let scaled = match &self.scaler {
            Some(scaler) if (width, height) == scaler.input_size() => {
                Some((scaler.run(&self.context, src)?, scaler.output_size()))
            }
            _ => None,
        };
        let (src, width, height) = match &scaled {
            Some((tex, (w, h))) => (tex, *w, *h),
            None => (src, width, height),
        };
        let staging = self.staging(width, height)?;
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: width,
            bottom: height,
            back: 1,
        };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: both textures belong to `self.device` (the capture source and the scaler output
        // are created on the same device), share the BGRA8 format, and the box lies inside both.
        // The immediate context is only used from `&mut self` methods of the owning capturer, so
        // there is no concurrent use.
        unsafe {
            self.context
                .CopySubresourceRegion(&staging, 0, 0, 0, 0, src, 0, Some(&region));
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(|e| platform_err("Map(staging)", e))?;

        let stride = mapped.RowPitch;
        let row_bytes = width as usize * 4;
        let len = (height as usize - 1) * stride as usize + row_bytes;
        let full = stride as usize * height as usize;
        let mut data = self.spare.take().unwrap_or_default();
        data.clear();
        data.reserve(full);
        // SAFETY: Map succeeded, so `pData` points to at least `RowPitch * (height-1) +
        // width*4` readable bytes of the mapped staging texture until Unmap below.
        let src_bytes = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, len) };
        data.extend_from_slice(src_bytes);
        // Pad the final row to a full stride so `data.len() == stride * height`.
        data.resize(full, 0);
        // SAFETY: matches the successful Map above; `src_bytes` is not used past this point.
        unsafe { self.context.Unmap(&staging, 0) };

        Ok(BgraFrame {
            width,
            height,
            stride,
            data,
            timestamp_us,
        })
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::Graphics::Direct3D11::{D3D11_SUBRESOURCE_DATA, D3D11_USAGE_DEFAULT};

    use super::*;
    use crate::resample_lanczos2;

    /// Dark 1-2 px strokes on a light page: the worst case for blurry filters.
    fn text_like(w: u32, h: u32) -> BgraFrame {
        let mut f = BgraFrame::new_tight(w, h);
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for y in 0..h {
            for x in 0..w {
                // Glyph cells of 6x10 px; a pseudo-random 5x7 bit pattern per cell.
                let (cx, cy) = (x / 6, y / 10);
                let mut s = seed ^ (u64::from(cx) * 0x1000_0000_01B3) ^ (u64::from(cy) << 32);
                s ^= s >> 29;
                s = s.wrapping_mul(0xBF58_476D_1CE4_E5B9);
                s ^= s >> 32;
                let (gx, gy) = (x % 6, y % 10);
                let on = gx < 5 && gy < 7 && s % 7 != 0 && (s >> (gy * 5 + gx) as u64) & 1 == 1;
                let v = if on { 20 } else { 245 };
                f.row_mut(y)[x as usize * 4..x as usize * 4 + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        seed = seed.wrapping_add(1);
        let _ = seed;
        f
    }

    /// Mean absolute neighbour difference: a simple sharpness measure.
    fn edge_energy(f: &BgraFrame) -> f64 {
        let mut total = 0u64;
        for y in 0..f.height - 1 {
            let (a, b) = (f.row(y), f.row(y + 1));
            for x in 0..f.width as usize - 1 {
                total += u64::from(a[x * 4].abs_diff(a[x * 4 + 4]));
                total += u64::from(a[x * 4].abs_diff(b[x * 4]));
            }
        }
        total as f64 / (f64::from(f.width) * f64::from(f.height))
    }

    fn psnr(a: &BgraFrame, b: &BgraFrame) -> f64 {
        let mut se = 0f64;
        for y in 0..a.height {
            for (p, q) in a.row(y).chunks_exact(4).zip(b.row(y).chunks_exact(4)) {
                for c in 0..3 {
                    let d = f64::from(p[c]) - f64::from(q[c]);
                    se += d * d;
                }
            }
        }
        let mse = se / (f64::from(a.width) * f64::from(a.height) * 3.0);
        if mse < 1e-9 {
            99.0
        } else {
            10.0 * (255.0f64 * 255.0 / mse).log10()
        }
    }

    /// Plain 2x2-area average to the target size (what the old bilinear path approximated).
    fn box_downscale(src: &BgraFrame, w: u32, h: u32) -> BgraFrame {
        let mut out = BgraFrame::new_tight(w, h);
        for oy in 0..h {
            let y0 = (u64::from(oy) * u64::from(src.height) / u64::from(h)) as u32;
            let y1 = ((u64::from(oy + 1) * u64::from(src.height)).div_ceil(u64::from(h)) as u32)
                .max(y0 + 1)
                .min(src.height);
            for ox in 0..w {
                let x0 = (u64::from(ox) * u64::from(src.width) / u64::from(w)) as u32;
                let x1 = ((u64::from(ox + 1) * u64::from(src.width)).div_ceil(u64::from(w)) as u32)
                    .max(x0 + 1)
                    .min(src.width);
                let mut sum = 0u64;
                for y in y0..y1 {
                    for x in x0..x1 {
                        sum += u64::from(src.row(y)[x as usize * 4]);
                    }
                }
                let v = (sum / u64::from((y1 - y0) * (x1 - x0))) as u8;
                out.row_mut(oy)[ox as usize * 4..ox as usize * 4 + 4]
                    .copy_from_slice(&[v, v, v, 255]);
            }
        }
        out
    }

    /// Uploads `src`, runs the real GPU scaler + readback, returns the scaled frame. `None` when
    /// no D3D11 device is available (headless CI): the test is then skipped.
    fn gpu_scale(src: &BgraFrame, out: (u32, u32)) -> Option<BgraFrame> {
        let (device, context) = create_device(None).ok()?;
        let desc = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_DEFAULT,
            ..bgra_desc(src.width, src.height)
        };
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: src.data.as_ptr().cast(),
            SysMemPitch: src.stride,
            SysMemSlicePitch: 0,
        };
        let mut tex = None;
        // SAFETY: `desc` is a valid BGRA8 description; `init` points at `stride * height`
        // initialised bytes of `src`, alive for the call; `tex` is a live out-pointer.
        unsafe { device.CreateTexture2D(&desc, Some(&init), Some(&mut tex)) }.ok()?;
        let tex = tex?;
        let mut reader = StagingReader::new(device, context, (src.width, src.height), Some(out));
        assert_eq!(reader.output_size(), out, "GPU scaler must be active");
        reader.read(&tex, src.width, src.height, 0).ok()
    }

    /// Measured numbers for the docs (run with `--nocapture`): for the non-2x ratios the GPU
    /// Lanczos-2 output matches the CPU reference and keeps more edge contrast than the old
    /// box/bilinear result.
    #[test]
    fn gpu_lanczos_matches_cpu_reference_and_is_sharper_than_box() {
        for (src_size, out) in [((1920, 1200), (1600, 1000)), ((2560, 1440), (1920, 1080))] {
            let src = text_like(src_size.0, src_size.1);
            let Some(gpu) = gpu_scale(&src, out) else {
                eprintln!("no D3D11 device; skipping");
                return;
            };
            assert_eq!((gpu.width, gpu.height), out);
            let cpu = resample_lanczos2(&src, out.0, out.1);
            let boxed = box_downscale(&src, out.0, out.1);
            let (e_gpu, e_box, e_src) = (edge_energy(&gpu), edge_energy(&boxed), edge_energy(&src));
            let p = psnr(&gpu, &cpu);
            eprintln!(
                "{src_size:?} -> {out:?}: GPU vs CPU Lanczos {p:.1} dB; edge energy src {e_src:.2}, box {e_box:.2}, lanczos {e_gpu:.2}"
            );
            assert!(
                p > 35.0,
                "GPU Lanczos differs from the CPU reference: {p:.1} dB"
            );
            assert!(
                e_gpu > e_box * 1.05,
                "no sharper than box: {e_gpu:.2} vs {e_box:.2}"
            );
        }
    }

    /// The exact-2x and 1:1 paths keep their old (box / identity) behaviour.
    #[test]
    fn gpu_exact_half_is_still_a_box_filter() {
        let src = text_like(1280, 800);
        let Some(gpu) = gpu_scale(&src, (640, 400)) else {
            return;
        };
        let boxed = box_downscale(&src, 640, 400);
        assert!(psnr(&gpu, &boxed) > 40.0);
    }
}
