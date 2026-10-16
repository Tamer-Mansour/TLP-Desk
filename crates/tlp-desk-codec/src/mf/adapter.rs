//! DXGI adapter enumeration: which GPU drives which part of the desktop.
//!
//! On hybrid laptops (Intel iGPU + NVIDIA dGPU) each monitor is scanned out by one adapter;
//! encoding on that adapter avoids a cross-GPU copy, so the encoder asks for its MFTs by the
//! adapter's LUID.

use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
};

use crate::CodecError;

/// A hardware display adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuAdapter {
    /// Locally unique id of the adapter for this boot (`LUID` packed as high << 32 | low).
    pub luid: i64,
    /// PCI vendor id (0x8086 Intel, 0x10DE NVIDIA, 0x1002 AMD).
    pub vendor_id: u32,
    /// Driver-reported adapter name.
    pub name: String,
    /// Desktop rectangles (left, top, right, bottom) of the monitors this adapter drives.
    pub outputs: Vec<(i32, i32, i32, i32)>,
}

impl GpuAdapter {
    /// Whether one of this adapter's monitors contains the desktop point (`x`, `y`).
    pub fn drives_point(&self, x: i32, y: i32) -> bool {
        self.outputs
            .iter()
            .any(|&(l, t, r, b)| x >= l && x < r && y >= t && y < b)
    }
}

fn platform(what: &str, e: windows::core::Error) -> CodecError {
    CodecError::Backend(format!("{what} failed: {e}"))
}

fn adapter_info(adapter: &IDXGIAdapter1) -> Option<GpuAdapter> {
    // SAFETY: `adapter` is a live COM interface; GetDesc1 fills a struct it returns by value.
    let desc = unsafe { adapter.GetDesc1() }.ok()?;
    if desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
        return None;
    }
    let len = desc
        .Description
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(desc.Description.len());
    let name = String::from_utf16_lossy(desc.Description.get(..len).unwrap_or_default());
    let luid = (i64::from(desc.AdapterLuid.HighPart) << 32) | i64::from(desc.AdapterLuid.LowPart);
    let mut outputs = Vec::new();
    // Bounded: no machine has anywhere near 64 outputs on one adapter.
    for j in 0..64 {
        // SAFETY: EnumOutputs returns an owned interface or an error past the last output.
        let Ok(output) = (unsafe { adapter.EnumOutputs(j) }) else {
            break;
        };
        // SAFETY: `output` is a live COM interface; GetDesc returns a struct by value.
        let Ok(od) = (unsafe { output.GetDesc() }) else {
            continue;
        };
        if od.AttachedToDesktop.as_bool() {
            let r = od.DesktopCoordinates;
            outputs.push((r.left, r.top, r.right, r.bottom));
        }
    }
    Some(GpuAdapter {
        luid,
        vendor_id: desc.VendorId,
        name,
        outputs,
    })
}

/// Lists the hardware display adapters (software rasterisers are skipped).
///
/// # Errors
/// Fails when DXGI is unavailable.
pub fn list_adapters() -> Result<Vec<GpuAdapter>, CodecError> {
    // SAFETY: plain factory creation; the returned interface is owned.
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.map_err(|e| platform("CreateDXGIFactory1", e))?;
    let mut out = Vec::new();
    // Bounded: a machine has a handful of adapters.
    for i in 0..32 {
        // SAFETY: EnumAdapters1 returns an owned interface or DXGI_ERROR_NOT_FOUND at the end.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else {
            break;
        };
        if let Some(info) = adapter_info(&adapter) {
            out.push(info);
        }
    }
    Ok(out)
}
