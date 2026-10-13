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
