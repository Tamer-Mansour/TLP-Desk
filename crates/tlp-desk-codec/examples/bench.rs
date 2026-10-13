//! Capture -> encode -> decode benchmark.
//!
//! ```text
//! cargo run --release -p tlp-desk-codec --example bench -- [--seconds 10] [--fps 60]
//!     [--monitor 0] [--backend auto|wgc|dxgi] [--static] [--threads 0]
//!     [--complexity low|medium|high] [--kbps N] [--crop WxH] [--downscale auto|never|half]
//!     [--screen] [--scale WxH] [--pipelined] [--animate] [--conv]
//! ```
//!
//! * `--scale 1920x1200` opens the capturer with `CaptureOptions::max_size`, so frames are
//!   downscaled on the GPU before readback; the encoder is created for the scaled size.
//! * `--crop 1920x1080` encodes only the top-left 1920x1080 of each (possibly scaled) frame.
//! * `--pipelined` captures on one thread and encodes on another, handing frames over through a
//!   one-slot mailbox where the newest frame wins (what the host should do); decoding runs on a
//!   third thread like a viewer would. Without it, everything runs serially on one thread.
//! * `--animate` opens a small always-on-top window on the captured monitor that repaints every
//!   timer tick (~64 Hz), so the OS keeps delivering new desktop frames and the capture cost is
//!   actually exercised. Without it, an idle desktop yields few captured frames.
//! * `--conv` (serial mode) additionally times the BGRA->I420 conversion on its own (it also runs
//!   inside encode, so this inflates CPU and tick time).
//!
//! The encoder is paced at `--fps`. Each tick encodes the newest captured frame; when the desktop
//! did not change the previous frame is reused. Unless `--static` is given a moving box is drawn
//! into the frame every tick so the encoder always sees motion. With `--static` only real desktop
//! changes are encoded.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tlp_desk_capture::{
    list_monitors, open_capturer_opts, BgraFrame, CaptureBackend, CaptureOptions, ScreenCapturer,
};
use tlp_desk_codec::convert::{bgra_to_i420, bgra_to_i420_half, even_up, I420Buffer};
use tlp_desk_codec::{
    Downscale, EncodedFrame, EncoderConfig, H264Complexity, H264Decoder, H264Encoder, H264Tuning,
    VideoDecoder, VideoEncoder,
};

struct Args {
    seconds: u64,
    fps: u32,
    monitor: u32,
    backend: Option<CaptureBackend>,
    motion: bool,
    tuning: H264Tuning,
    kbps: Option<u32>,
    crop: Option<(u32, u32)>,
    scale: Option<(u32, u32)>,
    pipelined: bool,
    animate: bool,
    conv: bool,
}

fn parse_size(v: &str, flag: &str) -> (u32, u32) {
    let (w, h) = v.split_once('x').unwrap_or_else(|| panic!("{flag} WxH"));
    (
        w.parse().unwrap_or_else(|_| panic!("{flag} width")),
        h.parse().unwrap_or_else(|_| panic!("{flag} height")),
    )
}

fn parse_args() -> Args {
    let mut a = Args {
        seconds: 10,
        fps: 60,
        monitor: 0,
        backend: None,
        motion: true,
        tuning: H264Tuning::default(),
        kbps: None,
        crop: None,
        scale: None,
        pipelined: false,
        animate: false,
        conv: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--seconds" => a.seconds = val().parse().expect("--seconds"),
            "--fps" => a.fps = val().parse().expect("--fps"),
            "--monitor" => a.monitor = val().parse().expect("--monitor"),
            "--kbps" => a.kbps = Some(val().parse().expect("--kbps")),
            "--threads" => a.tuning.threads = val().parse().expect("--threads"),
            "--complexity" => {
                a.tuning.complexity = match val().as_str() {
                    "low" => H264Complexity::Low,
                    "medium" => H264Complexity::Medium,
                    "high" => H264Complexity::High,
                    other => panic!("unknown complexity {other}"),
                }
            }
            "--backend" => {
                a.backend = match val().as_str() {
                    "auto" => None,
                    "wgc" => Some(CaptureBackend::WindowsGraphicsCapture),
                    "dxgi" => Some(CaptureBackend::DxgiDuplication),
                    other => panic!("unknown backend {other}"),
                }
            }
            "--static" => a.motion = false,
            "--screen" => a.tuning.screen_content = true,
            "--crop" => a.crop = Some(parse_size(&val(), "--crop")),
            "--scale" => a.scale = Some(parse_size(&val(), "--scale")),
            "--pipelined" => a.pipelined = true,
            "--animate" => a.animate = true,
            "--conv" => a.conv = true,
            "--downscale" => {
                a.tuning.downscale = match val().as_str() {
                    "auto" => Downscale::Auto,
                    "never" => Downscale::Never,
                    "half" => Downscale::Half,
                    other => panic!("unknown downscale {other}"),
                }
            }
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

#[derive(Default)]
struct Stage(Vec<f64>);

impl Stage {
    fn add(&mut self, d: Duration) {
        self.0.push(d.as_secs_f64() * 1000.0);
    }
    fn summary(&mut self) -> String {
        if self.0.is_empty() {
            return "n/a".into();
        }
        self.0.sort_by(|a, b| a.total_cmp(b));
        let avg = self.0.iter().sum::<f64>() / self.0.len() as f64;
        let p95 = self.0[((self.0.len() as f64 * 0.95) as usize).min(self.0.len() - 1)];
        let max = self.0[self.0.len() - 1];
        format!(
            "avg {avg:6.2} ms   p95 {p95:6.2} ms   max {max:6.2} ms   (n={})",
            self.0.len()
        )
    }
}

/// Draws a 160x120 box that moves every tick and changes colour.
fn draw_motion(frame: &mut BgraFrame, tick: u32) {
    let (bw, bh) = (160u32, 120u32);
    if frame.width <= bw || frame.height <= bh {
        return;
    }
    let x0 = (tick * 13) % (frame.width - bw);
    let y0 = (tick * 7) % (frame.height - bh);
    let color = [
        (tick * 5) as u8,
        255 - (tick * 3) as u8,
        (tick * 11) as u8,
        255,
    ];
    for y in y0..y0 + bh {
        let row = frame.row_mut(y);
        for px in row[x0 as usize * 4..(x0 + bw) as usize * 4].chunks_exact_mut(4) {
            px.copy_from_slice(&color);
        }
    }
}

#[cfg(windows)]
fn process_cpu_time() -> Duration {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut c, mut e, mut k, mut u) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: GetCurrentProcess returns a pseudo-handle valid for this call; all four
    // out-pointers reference live FILETIMEs.
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    if ok.is_err() {
        return Duration::ZERO;
    }
    let ticks = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    Duration::from_nanos((ticks(k) + ticks(u)) * 100)
}

#[cfg(not(windows))]
fn process_cpu_time() -> Duration {
    Duration::ZERO
}

/// A window that repaints itself in a new colour on every timer tick, so the desktop keeps
/// changing while the benchmark runs.
#[cfg(windows)]
mod animator {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::thread::JoinHandle;

    use windows::core::w;
    use windows::Win32::Foundation::COLORREF;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, PAINTSTRUCT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        PostQuitMessage, RegisterClassW, SetTimer, MSG, WM_DESTROY, WM_PAINT, WM_TIMER, WNDCLASSW,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    };

    static STOP: AtomicBool = AtomicBool::new(false);
    static TICK: AtomicU32 = AtomicU32::new(0);

    extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        // SAFETY: standard window procedure; every call receives the live `hwnd` it was invoked
        // for, and PAINTSTRUCT/brush handles are used strictly within BeginPaint/EndPaint.
        unsafe {
            match msg {
                WM_TIMER => {
                    if STOP.load(Ordering::Relaxed) {
                        let _ = DestroyWindow(hwnd);
                    } else {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    LRESULT(0)
                }
                WM_PAINT => {
                    let t = TICK.fetch_add(1, Ordering::Relaxed);
                    let mut ps = PAINTSTRUCT::default();
                    let hdc = BeginPaint(hwnd, &mut ps);
                    let color = (t * 7 % 256) | ((t * 3 % 256) << 8) | ((255 - t % 256) << 16);
                    let brush = CreateSolidBrush(COLORREF(color));
                    FillRect(hdc, &ps.rcPaint, brush);
                    let _ = DeleteObject(brush.into());
                    let _ = EndPaint(hwnd, &ps);
                    LRESULT(0)
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
    }

    /// Opens a 640x400 animated window at (`x`, `y`) in virtual-desktop physical pixels.
    pub fn start(x: i32, y: i32) -> JoinHandle<()> {
        STOP.store(false, Ordering::Relaxed);
        std::thread::spawn(move || {
            // SAFETY: plain Win32 window creation and message loop on this thread; the class
            // name and window procedure are 'static; out-pointers reference live locals.
            unsafe {
                let instance = GetModuleHandleW(None).unwrap_or_default();
                let class = WNDCLASSW {
                    lpfnWndProc: Some(wndproc),
                    hInstance: instance.into(),
                    lpszClassName: w!("tlp_desk_bench_animator"),
                    ..Default::default()
                };
                RegisterClassW(&class);
                let Ok(hwnd) = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    w!("tlp_desk_bench_animator"),
                    w!("tlp-desk bench"),
                    WS_POPUP | WS_VISIBLE,
                    x,
                    y,
                    640,
                    400,
                    None,
                    None,
                    Some(instance.into()),
                    None,
                ) else {
                    eprintln!("animator: CreateWindowExW failed");
                    return;
                };
                SetTimer(Some(hwnd), 1, 10, None);
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    DispatchMessageW(&msg);
                }
            }
        })
    }

    pub fn stop(handle: JoinHandle<()>) {
        STOP.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }
}

#[cfg(windows)]
fn make_dpi_aware() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    // SAFETY: process-wide setting with a constant argument; failure (already set) is harmless.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Size the encoder converts to: even-rounded source, or half of it when downscaling.
fn conv(frame: &BgraFrame, encoded: (u32, u32), yuv: &mut I420Buffer) {
    if encoded == (even_up(frame.width), even_up(frame.height)) {
        bgra_to_i420(frame, yuv);
    } else {
        bgra_to_i420_half(frame, yuv);
    }
}

struct Report {
    secs: f64,
    ticks: u32,
    captured: u32,
    encoded: u32,
    decoded: u32,
    keyframes: u32,
    bytes: u64,
    s_cap: Stage,
    s_conv: Stage,
    s_enc: Stage,
    s_dec: Stage,
    s_total: Stage,
}

fn main() {
    let args = parse_args();
    #[cfg(windows)]
    make_dpi_aware();
    let mon = list_monitors()
        .expect("list monitors")
        .into_iter()
        .nth(args.monitor as usize)
        .expect("no such monitor");
    #[cfg(windows)]
    let animator = args
        .animate
        .then(|| animator::start(mon.x + 64, mon.y + 64));

    let opts = CaptureOptions {
        backend: args.backend,
        max_size: args.scale,
    };
    let mut cap = open_capturer_opts(args.monitor, opts).expect("open capturer");
    let (fw, fh) = cap.frame_size();
    let (w, h) = match args.crop {
        Some((cw, ch)) => (cw.min(fw), ch.min(fh)),
        None => (fw, fh),
    };
    let mut config = EncoderConfig::for_screen(w, h, args.fps);
    if let Some(kbps) = args.kbps {
        config.bitrate_kbps = kbps;
    }
    let mut enc = H264Encoder::with_tuning(config, args.tuning).expect("encoder");
    let mut dec = H264Decoder::new().expect("decoder");
    let (ew, eh) = enc.encoded_size();
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());

    println!(
        "monitor {} '{}' {}x{} primary={} | backend {:?} | capture frames {}x{} (scale {:?})
source {}x{} -> encoded {}x{} | target {} fps, {} kbps | {:?} | {} logical CPUs | motion overlay: {} | {} | animate: {}",
        mon.index, mon.name, mon.width, mon.height, mon.primary, cap.backend(), fw, fh, args.scale,
        w, h, ew, eh, config.fps, config.bitrate_kbps, args.tuning, cpus, args.motion,
        if args.pipelined { "PIPELINED" } else { "serial" }, args.animate
    );

    let crop = move |mut f: BgraFrame| {
        // Cropping to the top-left is free: keep the stride, shrink the visible size.
        f.width = w;
        f.height = h;
        f
    };
    // Wait for the first frame (always delivered after start).
    let last = loop {
        if let Some(f) = cap.next_frame(Duration::from_secs(1)).expect("first frame") {
            break crop(f);
        }
    };
    assert_eq!((last.width, last.height), (w, h));
    // Warm up the encoder (first frame initialises it and is an IDR).
    let first = enc.encode(&last).expect("encode").expect("first packet");
    dec.decode(&first.data).expect("decode first");

    let cpu_start = process_cpu_time();
    let mut r = if args.pipelined {
        run_pipelined(&args, cap, enc, dec, last, crop)
    } else {
        run_serial(&args, cap, enc, dec, last, crop)
    };
    let cpu = (process_cpu_time() - cpu_start).as_secs_f64();
    #[cfg(windows)]
    if let Some(a) = animator {
        animator::stop(a);
    }

    let secs = r.secs;
    println!("ran {secs:.1} s, {} encode ticks", r.ticks);
    println!(
        "fps: encoded {:.1}, decoded {:.1}, new desktop frames from capture {:.1}",
        r.encoded as f64 / secs,
        r.decoded as f64 / secs,
        r.captured as f64 / secs
    );
    println!("capture (frame ready -> CPU BGRA): {}", r.s_cap.summary());
    if args.conv && !args.pipelined {
        println!("bgra->i420 (part of encode):       {}", r.s_conv.summary());
    }
    println!("encode (incl. conversion):         {}", r.s_enc.summary());
    println!("decode (incl. i420->bgra):         {}", r.s_dec.summary());
    println!("total per tick:                    {}", r.s_total.summary());
    println!(
        "bitrate: {:.2} Mbps avg, {:.1} KB/frame avg, {} keyframes, {} encoded",
        r.bytes as f64 * 8.0 / secs / 1e6,
        r.bytes as f64 / r.encoded.max(1) as f64 / 1024.0,
        r.keyframes,
        r.encoded
    );
    println!(
        "process CPU: {:.1}% of one core, {:.1}% of the whole machine (capture+encode+decode)",
        cpu / secs * 100.0,
        cpu / secs * 100.0 / cpus as f64
    );
}

/// Sleeps until the next tick; returns false when the run is over.
fn pace(start: Instant, next_tick: &mut Instant, interval: Duration, seconds: u64) -> bool {
    let now = Instant::now();
    if *next_tick > now {
        std::thread::sleep(*next_tick - now);
    }
    *next_tick += interval;
    if *next_tick < Instant::now() {
        *next_tick = Instant::now(); // fell behind: do not try to catch up
    }
    start.elapsed() < Duration::from_secs(seconds)
}

fn run_serial(
    args: &Args,
    mut cap: Box<dyn ScreenCapturer>,
    mut enc: H264Encoder,
    mut dec: H264Decoder,
    mut last: BgraFrame,
    crop: impl Fn(BgraFrame) -> BgraFrame,
) -> Report {
    let mut r = Report::new();
    let encoded_size = enc.encoded_size();
    let mut yuv = I420Buffer::default();
    let interval = Duration::from_secs_f64(1.0 / args.fps.max(1) as f64);
    let start = Instant::now();
    let mut next_tick = start;

    while pace(start, &mut next_tick, interval, args.seconds) {
        r.ticks += 1;
        let t0 = Instant::now();
        let mut fresh = false;
        if let Some(frame) = cap.next_frame(Duration::ZERO).expect("capture") {
            r.s_cap.add(t0.elapsed());
            r.captured += 1;
            fresh = true;
            cap.recycle(std::mem::replace(&mut last, crop(frame)));
        }
        let _ = cap.cursor().expect("cursor");
        if args.motion {
            draw_motion(&mut last, r.ticks);
        } else if !fresh {
            continue;
        }
        if args.conv {
            let tc = Instant::now();
            conv(&last, encoded_size, &mut yuv);
            r.s_conv.add(tc.elapsed());
        }
        let te = Instant::now();
        let pkt = enc.encode(&last).expect("encode");
        r.s_enc.add(te.elapsed());
        let Some(pkt) = pkt else { continue };
        r.count(&pkt);
        let td = Instant::now();
        if let Some(out) = dec.decode(&pkt.data).expect("decode") {
            r.s_dec.add(td.elapsed());
            r.decoded += 1;
            dec.recycle(out);
        }
        r.s_total.add(t0.elapsed());
    }
    r.secs = start.elapsed().as_secs_f64();
    r
}

/// One-slot mailbox: the producer overwrites, the consumer takes the newest.
#[derive(Default)]
struct Mailbox {
    slot: Mutex<Option<BgraFrame>>,
    cv: Condvar,
}

fn run_pipelined(
    args: &Args,
    mut cap: Box<dyn ScreenCapturer>,
    mut enc: H264Encoder,
    mut dec: H264Decoder,
    mut last: BgraFrame,
    crop: impl Fn(BgraFrame) -> BgraFrame + Send + 'static,
) -> Report {
    let mailbox = Arc::new(Mailbox::default());
    let stop = Arc::new(AtomicBool::new(false));
    let (recycle_tx, recycle_rx) = mpsc::channel::<BgraFrame>();

    // Capture thread: poll for new frames (without waiting inside next_frame, so the measured
    // time is pure readback work), publish newest-wins, recycle what the encoder hands back.
    let cap_thread = {
        let (mailbox, stop) = (Arc::clone(&mailbox), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut s_cap = Stage::default();
            let mut captured = 0u32;
            while !stop.load(Ordering::Relaxed) {
                while let Ok(f) = recycle_rx.try_recv() {
                    cap.recycle(f);
                }
                let t0 = Instant::now();
                match cap.next_frame(Duration::ZERO).expect("capture") {
                    Some(f) => {
                        s_cap.add(t0.elapsed());
                        captured += 1;
                        let old = mailbox.slot.lock().unwrap().replace(crop(f));
                        mailbox.cv.notify_one();
                        if let Some(old) = old {
                            cap.recycle(old); // encoder never saw it: dropped, not queued
                        }
                    }
                    None => std::thread::sleep(Duration::from_millis(1)),
                }
                let _ = cap.cursor();
            }
            (s_cap, captured)
        })
    };

    // Decode thread (stands in for the viewer).
    let (pkt_tx, pkt_rx) = mpsc::channel::<EncodedFrame>();
    let dec_thread = std::thread::spawn(move || {
        let mut s_dec = Stage::default();
        let mut decoded = 0u32;
        for pkt in pkt_rx {
            let td = Instant::now();
            if let Some(out) = dec.decode(&pkt.data).expect("decode") {
                s_dec.add(td.elapsed());
                decoded += 1;
                dec.recycle(out);
            }
        }
        (s_dec, decoded)
    });

    // Encode loop on this thread.
    let mut r = Report::new();
    let interval = Duration::from_secs_f64(1.0 / args.fps.max(1) as f64);
    let start = Instant::now();
    let mut next_tick = start;
    while pace(start, &mut next_tick, interval, args.seconds) {
        r.ticks += 1;
        let t0 = Instant::now();
        let fresh = mailbox.slot.lock().unwrap().take();
        let is_fresh = fresh.is_some();
        if let Some(f) = fresh {
            let _ = recycle_tx.send(std::mem::replace(&mut last, f));
        }
        if args.motion {
            draw_motion(&mut last, r.ticks);
        } else if !is_fresh {
            continue;
        }
        let te = Instant::now();
        let pkt = enc.encode(&last).expect("encode");
        r.s_enc.add(te.elapsed());
        let Some(pkt) = pkt else { continue };
        r.count(&pkt);
        let _ = pkt_tx.send(pkt);
        r.s_total.add(t0.elapsed());
    }
    r.secs = start.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    drop(pkt_tx);
    let (s_cap, captured) = cap_thread.join().expect("capture thread");
    let (s_dec, decoded) = dec_thread.join().expect("decode thread");
    r.s_cap = s_cap;
    r.captured = captured;
    r.s_dec = s_dec;
    r.decoded = decoded;
    r
}

impl Report {
    fn new() -> Self {
        Self {
            secs: 0.0,
            ticks: 0,
            captured: 0,
            encoded: 0,
            decoded: 0,
            keyframes: 0,
            bytes: 0,
            s_cap: Stage::default(),
            s_conv: Stage::default(),
            s_enc: Stage::default(),
            s_dec: Stage::default(),
            s_total: Stage::default(),
        }
    }

    fn count(&mut self, pkt: &EncodedFrame) {
        self.encoded += 1;
        self.keyframes += pkt.keyframe as u32;
        self.bytes += pkt.data.len() as u64;
    }
}
