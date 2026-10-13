//! Windows.Graphics.Capture backend (Windows 10 1903+).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use windows::core::{IInspectable, Interface};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

use super::cursor::CursorTracker;
use super::d3d::{create_device, StagingReader};
use super::dxgi::find_output;
use super::monitors::Target;
use super::platform_err;
use crate::{BgraFrame, CaptureBackend, CaptureError, CursorState, MonitorInfo, ScreenCapturer};

/// Signalled from the free-threaded FrameArrived callback.
#[derive(Default)]
struct Signal {
    ready: Mutex<bool>,
    cv: Condvar,
}

pub(super) struct WgcCapturer {
    monitor: MonitorInfo,
    // Field order matters for drop: stop the session before the pool and device go away.
    session: GraphicsCaptureSession,
    pool: Direct3D11CaptureFramePool,
    _item: GraphicsCaptureItem,
    reader: StagingReader,
    cursor: CursorTracker,
    signal: Arc<Signal>,
    closed: Arc<AtomicBool>,
    frame_arrived_token: i64,
    started: Instant,
}

impl WgcCapturer {
    pub(super) fn new(target: Target, max_size: Option<(u32, u32)>) -> Result<Self, CaptureError> {
        if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
            return Err(CaptureError::Platform(
                "Windows.Graphics.Capture is not supported on this system".into(),
            ));
        }
        // Use the GPU that scans out this monitor: on hybrid-GPU machines the default adapter
        // may be a different one, and every frame would then cross adapters (measured 3.7 ms vs
        // 1.3 ms readback for 1080p). Fall back to the default adapter if lookup fails.
        let (device, context) = match find_output(&target).map(|(adapter, _)| adapter) {
            Ok(adapter) => create_device(Some(&adapter)).or_else(|_| create_device(None))?,
            Err(_) => create_device(None)?,
        };

        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| platform_err("IGraphicsCaptureItemInterop", e))?;
        // SAFETY: `target.hmonitor` was just returned by EnumDisplayMonitors.
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(target.hmonitor) }
            .map_err(|e| platform_err("CreateForMonitor", e))?;
        let size = item.Size().map_err(|e| platform_err("item.Size", e))?;
        let mut monitor = target.info;
        // The item size is authoritative (physical pixels).
        monitor.width = size.Width.max(0) as u32;
        monitor.height = size.Height.max(0) as u32;

        let dxgi_device: IDXGIDevice = device
            .cast()
            .map_err(|e| platform_err("ID3D11Device -> IDXGIDevice", e))?;
        // SAFETY: `dxgi_device` is a valid IDXGIDevice for the lifetime of the call.
        let winrt_device: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) }
                .and_then(|i| i.cast())
                .map_err(|e| platform_err("CreateDirect3D11DeviceFromDXGIDevice", e))?;

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )
        .map_err(|e| platform_err("CreateFreeThreaded", e))?;

        let signal = Arc::new(Signal::default());
        let cb_signal = Arc::clone(&signal);
        let frame_arrived_token = pool
            .FrameArrived(
                &TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |_, _| {
                    if let Ok(mut ready) = cb_signal.ready.lock() {
                        *ready = true;
                    }
                    cb_signal.cv.notify_one();
                    Ok(())
                }),
            )
            .map_err(|e| platform_err("FrameArrived", e))?;

        let closed = Arc::new(AtomicBool::new(false));
        let cb_closed = Arc::clone(&closed);
        let cb_signal = Arc::clone(&signal);
        item.Closed(
            &TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
                cb_closed.store(true, Ordering::SeqCst);
                if let Ok(mut ready) = cb_signal.ready.lock() {
                    *ready = true;
                }
                cb_signal.cv.notify_one();
                Ok(())
            }),
        )
        .map_err(|e| platform_err("item.Closed", e))?;

        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|e| platform_err("CreateCaptureSession", e))?;
        // The cursor is sent separately. Both setters need newer Windows builds; ignore failures
        // (an older build just draws the cursor / the yellow border).
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetIsBorderRequired(false);
        session
            .StartCapture()
            .map_err(|e| platform_err("StartCapture", e))?;

        let reader = StagingReader::new(device, context, (monitor.width, monitor.height), max_size);
        Ok(Self {
            cursor: CursorTracker::new(&monitor),
            monitor,
            session,
            pool,
            _item: item,
            reader,
            signal,
            closed,
            frame_arrived_token,
            started: Instant::now(),
        })
    }

    /// Returns the newest queued frame, closing any older ones (we never want stale frames).
    fn take_latest(&self) -> Result<Option<Direct3D11CaptureFrame>, CaptureError> {
        let mut latest: Option<Direct3D11CaptureFrame> = None;
        loop {
            match self.pool.TryGetNextFrame() {
                Ok(frame) => {
                    if let Some(old) = latest.replace(frame) {
                        let _ = old.Close();
                    }
                }
                // A null frame comes back as an "empty" error (code S_OK): queue is drained.
                Err(e) if e.code().is_ok() => return Ok(latest),
                Err(e) => return Err(platform_err("TryGetNextFrame", e)),
            }
        }
    }

    fn convert(&mut self, frame: Direct3D11CaptureFrame) -> Result<BgraFrame, CaptureError> {
        let result = (|| {
            let content = frame
                .ContentSize()
                .map_err(|e| platform_err("ContentSize", e))?;
            if content.Width as u32 != self.monitor.width
                || content.Height as u32 != self.monitor.height
            {
                // Resolution / orientation changed: the caller must re-open the capturer.
                return Err(CaptureError::SurfaceLost);
            }
            let surface = frame.Surface().map_err(|e| platform_err("Surface", e))?;
            let access: IDirect3DDxgiInterfaceAccess = surface
                .cast()
                .map_err(|e| platform_err("IDirect3DDxgiInterfaceAccess", e))?;
            // SAFETY: the surface wraps a D3D11 texture created on our device by the frame pool.
            let texture: ID3D11Texture2D = unsafe { access.GetInterface() }
                .map_err(|e| platform_err("GetInterface<ID3D11Texture2D>", e))?;
            let ts = self.started.elapsed().as_micros() as u64;
            self.reader
                .read(&texture, self.monitor.width, self.monitor.height, ts)
        })();
        // Return the buffer to the pool right away so capture can continue.
        let _ = frame.Close();
        result
    }
}

impl ScreenCapturer for WgcCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return Err(CaptureError::SurfaceLost);
            }
            if let Some(frame) = self.take_latest()? {
                return self.convert(frame).map(Some);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let guard = self
                .signal
                .ready
                .lock()
                .map_err(|_| CaptureError::Platform("signal mutex poisoned".into()))?;
            let (mut ready, _) = self
                .signal
                .cv
                .wait_timeout_while(guard, deadline - now, |r| !*r)
                .map_err(|_| CaptureError::Platform("signal mutex poisoned".into()))?;
            *ready = false;
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
        Some(CaptureBackend::WindowsGraphicsCapture)
    }

    fn frame_size(&self) -> (u32, u32) {
        self.reader.output_size()
    }
}

impl Drop for WgcCapturer {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.frame_arrived_token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
