//! DXGI Desktop Duplication backend (Windows 8+), used when Windows.Graphics.Capture is
//! unavailable.

use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication,
    IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO,
};

use super::cursor::CursorTracker;
use super::d3d::{create_device, StagingReader};
use super::monitors::{PhysicalDpiScope, Target};
use super::platform_err;
use crate::{BgraFrame, CaptureBackend, CaptureError, CursorState, MonitorInfo, ScreenCapturer};

pub(super) struct DxgiCapturer {
    monitor: MonitorInfo,
    duplication: IDXGIOutputDuplication,
    reader: StagingReader,
    cursor: CursorTracker,
    started: Instant,
    width: u32,
    height: u32,
}

/// Finds the adapter + output that drive `target`'s HMONITOR.
pub(super) fn find_output(target: &Target) -> Result<(IDXGIAdapter, IDXGIOutput1), CaptureError> {
    // SAFETY: plain factory creation with no inputs.
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.map_err(|e| platform_err("CreateDXGIFactory1", e))?;
    for a in 0.. {
        // SAFETY: enumerating by index; DXGI_ERROR_NOT_FOUND terminates the loop.
        let adapter = match unsafe { factory.EnumAdapters1(a) } {
            Ok(adapter) => adapter,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(platform_err("EnumAdapters1", e)),
        };
        for o in 0.. {
            // SAFETY: as above, per adapter.
            let output = match unsafe { adapter.EnumOutputs(o) } {
                Ok(output) => output,
                Err(_) => break,
            };
            // SAFETY: `output` is a valid IDXGIOutput.
            let desc = unsafe { output.GetDesc() }.map_err(|e| platform_err("GetDesc", e))?;
            if desc.Monitor == target.hmonitor {
                let output1: IDXGIOutput1 = output
                    .cast()
                    .map_err(|e| platform_err("IDXGIOutput -> IDXGIOutput1", e))?;
                let adapter: IDXGIAdapter = adapter
                    .cast()
                    .map_err(|e| platform_err("IDXGIAdapter1 -> IDXGIAdapter", e))?;
                return Ok((adapter, output1));
            }
        }
    }
    Err(CaptureError::NoSuchMonitor(target.info.index))
}

impl DxgiCapturer {
    pub(super) fn new(target: Target, max_size: Option<(u32, u32)>) -> Result<Self, CaptureError> {
        let (adapter, output) = find_output(&target)?;
        let (device, context) = create_device(Some(&adapter))?;
        let duplication = {
            // Duplication of a high-DPI monitor needs a DPI-aware caller.
            let _dpi = PhysicalDpiScope::enter();
            // SAFETY: `device` was created on the adapter that owns `output`.
            unsafe { output.DuplicateOutput(&device) }
                .map_err(|e| platform_err("DuplicateOutput", e))?
        };
        // SAFETY: `duplication` is a valid interface; GetDesc fills a plain struct.
        let desc = unsafe { duplication.GetDesc() };
        let (width, height) = (desc.ModeDesc.Width, desc.ModeDesc.Height);
        let mut monitor = target.info;
        monitor.width = width;
        monitor.height = height;
        Ok(Self {
            cursor: CursorTracker::new(&monitor),
            monitor,
            duplication,
            reader: StagingReader::new(device, context, (width, height), max_size),
            started: Instant::now(),
            width,
            height,
        })
    }

    fn acquire(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<(DXGI_OUTDUPL_FRAME_INFO, IDXGIResource)>, CaptureError> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        // SAFETY: both out-pointers reference live locals.
        match unsafe {
            self.duplication
                .AcquireNextFrame(ms, &mut info, &mut resource)
        } {
            Ok(()) => match resource {
                Some(r) => Ok(Some((info, r))),
                None => {
                    self.release();
                    Ok(None)
                }
            },
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => Ok(None),
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => Err(CaptureError::SurfaceLost),
            Err(e) => Err(platform_err("AcquireNextFrame", e)),
        }
    }

    fn release(&self) {
        // SAFETY: only called after a successful AcquireNextFrame.
        let _ = unsafe { self.duplication.ReleaseFrame() };
    }
}

impl ScreenCapturer for DxgiCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some((info, resource)) = self.acquire(remaining)? else {
                return Ok(None);
            };
            // LastPresentTime == 0 means only the pointer moved: no new desktop image.
            if info.LastPresentTime == 0 {
                self.release();
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                continue;
            }
            let result = resource
                .cast::<ID3D11Texture2D>()
                .map_err(|e| platform_err("IDXGIResource -> ID3D11Texture2D", e))
                .and_then(|tex| {
                    let ts = self.started.elapsed().as_micros() as u64;
                    self.reader.read(&tex, self.width, self.height, ts)
                });
            drop(resource);
            self.release();
            return result.map(Some);
        }
    }

    fn cursor(&mut self) -> Result<CursorState, CaptureError> {
        self.cursor.poll()
    }

    fn monitor(&self) -> &MonitorInfo {
        &self.monitor
    }

    fn recycle(&mut self, frame: BgraFrame) {
        self.reader.recycle(frame.data);
    }

    fn backend(&self) -> Option<CaptureBackend> {
        Some(CaptureBackend::DxgiDuplication)
    }

    fn frame_size(&self) -> (u32, u32) {
        self.reader.output_size()
    }
}
