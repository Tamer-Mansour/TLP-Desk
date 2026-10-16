//! `--quality`: objective picture quality of the encoders on text-heavy desktop scenes.
//!
//! Real fonts are rendered off screen with GDI (ClearType, like a real desktop; nothing is
//! shown on the user's screen), four scenes are animated from the rendered canvases, every
//! scene is encoded at several bits-per-pixel budgets and the decoded pictures are compared
//! with the source:
//!
//! * `PSNR-Y`: luma PSNR over the whole picture.
//! * `PSNR-edge`: luma PSNR over the pixels on or next to strokes and edges (3x3 range > 48 in
//!   the source), which is where text lives and where blur and blocking show first.
//! * `SSIM`: mean structural similarity of luma over 8x8 windows (stride 4).
//! * `QP`: average slice QP read back from the bitstream (`QpProbe`).
//!
//! ```text
//! cargo run --release -p tlp-desk-codec --example bench -- --quality [--synthetic 1920x1080]
//!     [--encoder sw|hw] [--adapter intel|nvidia] [--bpp 0.03,0.06,0.1,0.2] [--frames 90]
//!     [--scene ui|code|scroll|photo] [--qp 0:51]
//! ```
//!
//! `--qp MIN:MAX` overrides the quantiser limits (`0:51` = the old, unbounded behaviour).

use std::ffi::c_void;
use std::time::Instant;

use tlp_desk_capture::BgraFrame;
use tlp_desk_codec::{EncoderConfig, H264Decoder, QpProbe, VideoDecoder};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush, DeleteDC, DeleteObject,
    FillRect, SelectObject, SetBkMode, SetTextColor, TextOutW, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, FF_DONTCARE,
    HDC, HGDIOBJ, OUT_DEFAULT_PRECIS, TRANSPARENT,
};

use super::{make_encoder, Args};

const FPS: u32 = 60;

/// A GDI canvas: a top-down 32-bit DIB we can read directly.
struct Canvas {
    dc: HDC,
    w: u32,
    h: u32,
    bits: *mut u8,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        // SAFETY: a memory DC and a DIB section of w*h*4 bytes whose pixel pointer GDI keeps
        // valid until the bitmap is deleted (in Drop); the pointer is only read through
        // `pixels()` while the canvas lives.
        unsafe {
            let dc = CreateCompatibleDC(None);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
                .expect("DIB section");
            SelectObject(dc, HGDIOBJ(bmp.0));
            SetBkMode(dc, TRANSPARENT);
            Self {
                dc,
                w,
                h,
                bits: bits.cast(),
            }
        }
    }

    fn fill(&self, x: i32, y: i32, w: i32, h: i32, rgb: (u8, u8, u8)) {
        // SAFETY: plain GDI fill on our DC; the brush is deleted right after.
        unsafe {
            let brush = CreateSolidBrush(COLORREF(
                u32::from(rgb.0) | (u32::from(rgb.1) << 8) | (u32::from(rgb.2) << 16),
            ));
            let r = RECT {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            };
            FillRect(self.dc, &r, brush);
            let _ = DeleteObject(brush.into());
        }
    }

    /// Draws `text` with `face` at pixel height `px`.
    fn text(&self, x: i32, y: i32, text: &str, face: &str, px: i32, bold: bool, rgb: (u8, u8, u8)) {
        let face: Vec<u16> = face.encode_utf16().chain(Some(0)).collect();
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: font created, selected, used and deleted within this call; `face` is
        // NUL-terminated and outlives CreateFontW.
        unsafe {
            let font = CreateFontW(
                -px,
                0,
                0,
                0,
                if bold { 700 } else { 400 },
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                FF_DONTCARE.0 as u32,
                PCWSTR(face.as_ptr()),
            );
            let old = SelectObject(self.dc, HGDIOBJ(font.0));
            SetTextColor(
                self.dc,
                COLORREF(u32::from(rgb.0) | (u32::from(rgb.1) << 8) | (u32::from(rgb.2) << 16)),
            );
            let _ = TextOutW(self.dc, x, y, &wide);
            SelectObject(self.dc, old);
            let _ = DeleteObject(font.into());
        }
    }

    /// Copies rows `y0..y0+h` into `frame` (alpha forced opaque).
    fn blit(&self, y0: u32, frame: &mut BgraFrame) {
        let rows = frame.height.min(self.h.saturating_sub(y0));
        // SAFETY: `bits` points at w*h*4 bytes (see `new`); rows y0..y0+rows are inside.
        let src = unsafe {
            std::slice::from_raw_parts(
                self.bits.add(y0 as usize * self.w as usize * 4),
                rows as usize * self.w as usize * 4,
            )
        };
        for (y, line) in src.chunks_exact(self.w as usize * 4).enumerate() {
            let row = frame.row_mut(y as u32);
            let n = row.len().min(line.len());
            row[..n].copy_from_slice(&line[..n]);
            for px in row[..n].chunks_exact_mut(4) {
                px[3] = 255;
            }
        }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: deletes the bitmap selected into our own DC and then the DC, once.
        unsafe {
            let bmp = SelectObject(self.dc, HGDIOBJ::default());
            let _ = DeleteObject(bmp);
            let _ = DeleteDC(self.dc);
        }
    }
}

const PROSE: [&str; 12] = [
    "Remote desktop sessions are judged by how readable the text stays while the screen moves.",
    "Every pixel of a character matters: a stroke that is one pixel wide and one shade too soft",
    "reads as blur, and a block boundary that cuts through a letter reads as damage. Sharpness",
    "is therefore bought with bits, and when the link cannot supply them the picture should get",
    "smaller, not dirtier. A 1600 by 1000 view with crisp text beats a 1920 by 1200 view that",
    "is smeared, because the eye reads edges first. Numbers 0123456789 and symbols {}[]()<>;:",
    "survive only if the quantiser stays low enough: ij il lI 1l O0 rn m cl d vv w. Quick brown",
    "fox, lazy dog; the quick brown fox jumps over the lazy dog 0123456789 !@#$%^&*() _+-=.",
    "Typical documents mix paragraphs, tables, menus and small gray captions on white paper.",
    "Table rows align columns of figures: 1,204.50   88.12   3,990.00   -17.25   0.04   7.",
    "Arabic text must stay readable too, with its dots and thin joins: the dots above and below",
    "the letters are only a pixel or two across and are the first thing a coarse encoder loses.",
];
const ARABIC: [&str; 4] = [
    "يجب أن يبقى النص العربي واضحاً حتى عند انخفاض الدقة، فالنقاط الصغيرة فوق الحروف وتحتها",
    "هي أول ما يضيع عندما يكون الضغط شديداً. الوضوح أهم من عدد البكسلات في الصورة النهائية.",
    "٠١٢٣٤٥٦٧٨٩ — جدول الأرقام والتواريخ ١٤٤٧/٠٣/١٥ والعملات ١٬٢٠٤٫٥٠ ر.س",
    "تقرير الأداء الشهري: عدد الجلسات ١٢٨، متوسط زمن الاستجابة ٢٣ مللي ثانية، معدل الإطارات ٦٠.",
];
const CODE: [&str; 20] = [
    "pub fn tick(&mut self, s: &Signals) -> AdaptDecision {",
    "    let dt = self.last_at.map_or(TICK, |t| s.at.saturating_sub(t));",
    "    let level = self.classify(s); // Clear | Mild | Severe",
    "    match level {",
    "        Congestion::Clear => self.target = (self.target + inc).min(max),",
    "        Congestion::Mild | Congestion::Severe => {",
    "            let factor = if level == Congestion::Severe { 600 } else { 850 };",
    "            self.target = (self.target * factor / 1000).clamp(min, max);",
    "        }",
    "    }",
    "    let bpp = f64::from(self.target) * 1000.0 / (w * h * fps) as f64;",
    "    if bpp < MIN_BPP && self.rung + 1 < RUNGS.len() { self.rung += 1; }",
    "    for (i, px) in frame.data.chunks_exact_mut(4).enumerate() {",
    "        let (x, y) = (i % w, i / w); px[0] = (x ^ y) as u8; px[3] = 0xff;",
    "    }",
    "    assert_eq!(decoded.width, 1920, \"size {:?} != {:?}\", a, b);",
    "    tracing::info!(kbps = d.bitrate_kbps, fps = d.fps, \"adaptive decision\");",
    "    let table: HashMap<String, Vec<u32>> = rows.iter().map(|r| r.parse()).collect();",
    "    #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct Rung { scale: u32 }",
    "}",
];

/// One animated scene: renders frame `t` into `frame`.
trait Scene {
    fn name(&self) -> &'static str;
    fn render(&mut self, t: u32, frame: &mut BgraFrame);
}

struct Ui {
    canvas: Canvas,
}

impl Ui {
    fn new(w: u32, h: u32) -> Self {
        let c = Canvas::new(w, h);
        let (wi, hi) = (w as i32, h as i32);
        c.fill(0, 0, wi, hi, (240, 240, 243));
        c.fill(0, 0, wi, 40, (32, 74, 135));
        c.text(
            14,
            9,
            "TLP-Desk - Settings and session overview",
            "Segoe UI",
            18,
            true,
            (255, 255, 255),
        );
        c.fill(0, 40, 260, hi - 40, (228, 230, 236));
        for (i, item) in [
            "General",
            "Security",
            "Video quality",
            "Audio",
            "File transfer",
            "Address book",
            "Advanced",
        ]
        .iter()
        .enumerate()
        {
            let y = 60 + i as i32 * 38;
            if i == 2 {
                c.fill(8, y - 6, 244, 34, (200, 214, 240));
            }
            c.text(24, y, item, "Segoe UI", 17, i == 2, (20, 20, 28));
        }
        c.fill(280, 60, wi - 300, hi - 90, (255, 255, 255));
        c.text(300, 74, "Video quality", "Segoe UI", 26, true, (20, 20, 28));
        for (i, line) in PROSE.iter().enumerate() {
            c.text(
                300,
                130 + i as i32 * 24,
                line,
                "Segoe UI",
                16,
                false,
                (30, 30, 36),
            );
        }
        for (i, line) in ARABIC.iter().enumerate() {
            c.text(
                300,
                430 + i as i32 * 28,
                line,
                "Segoe UI",
                18,
                false,
                (30, 30, 36),
            );
        }
        for (i, label) in ["Apply", "Cancel", "Defaults"].iter().enumerate() {
            let x = 300 + i as i32 * 130;
            c.fill(x, hi - 120, 114, 36, (225, 228, 235));
            c.text(x + 18, hi - 112, label, "Segoe UI", 16, false, (20, 20, 28));
        }
        Self { canvas: c }
    }
}

impl Scene for Ui {
    fn name(&self) -> &'static str {
        "ui"
    }

    fn render(&mut self, t: u32, frame: &mut BgraFrame) {
        let c = &self.canvas;
        // A counter and a progress bar change every frame; everything else stays still.
        let (wi, hi) = (c.w as i32, c.h as i32);
        c.fill(wi - 420, 60, 100, 28, (255, 255, 255));
        c.text(
            wi - 420,
            62,
            &format!("{:05}", t),
            "Consolas",
            20,
            false,
            (20, 20, 28),
        );
        c.fill(300, hi - 170, 600, 14, (225, 228, 235));
        c.fill(300, hi - 170, (t as i32 * 5) % 600, 14, (32, 120, 200));
        c.blit(0, frame);
    }
}

struct Code {
    canvas: Canvas,
}

impl Code {
    fn new(w: u32, h: u32) -> Self {
        let lines = h / 20 + 140;
        let c = Canvas::new(w, lines * 20);
        c.fill(0, 0, w as i32, (lines * 20) as i32, (30, 30, 34));
        for i in 0..lines {
            let line = CODE[i as usize % CODE.len()];
            let y = i as i32 * 20;
            c.text(
                8,
                y + 1,
                &format!("{:4}", i + 1),
                "Consolas",
                15,
                false,
                (110, 110, 120),
            );
            let palette = [
                (220, 220, 220),
                (86, 156, 214),
                (206, 145, 120),
                (78, 201, 176),
                (181, 206, 168),
            ];
            // Colour per word so the syntax-highlight chroma edges are present.
            let mut x = 64;
            for (k, word) in line.split_inclusive(' ').enumerate() {
                c.text(
                    x,
                    y + 1,
                    word,
                    "Consolas",
                    15,
                    false,
                    palette[(k + i as usize) % palette.len()],
                );
                x += word.chars().count() as i32 * 8;
            }
        }
        Self { canvas: c }
    }
}

impl Scene for Code {
    fn name(&self) -> &'static str {
        "code"
    }

    fn render(&mut self, t: u32, frame: &mut BgraFrame) {
        // 3 px per frame (180 px/s): reading-speed smooth scrolling.
        let max = self.canvas.h.saturating_sub(frame.height);
        self.canvas.blit((t * 3) % max.max(1), frame);
    }
}

struct Scroll {
    canvas: Canvas,
}

impl Scroll {
    fn new(w: u32, h: u32) -> Self {
        let rows = (h * 3) / 26 + 2;
        let c = Canvas::new(w, rows * 26);
        c.fill(0, 0, w as i32, (rows * 26) as i32, (255, 255, 255));
        for i in 0..rows {
            let line = if i % 9 == 8 {
                ARABIC[(i as usize / 9) % ARABIC.len()]
            } else {
                PROSE[i as usize % PROSE.len()]
            };
            let gray = if i % 7 == 0 {
                (110, 110, 120)
            } else {
                (28, 28, 32)
            };
            c.text(
                w as i32 / 8,
                i as i32 * 26 + 2,
                line,
                "Segoe UI",
                17,
                i % 11 == 0,
                gray,
            );
        }
        Self { canvas: c }
    }
}

impl Scene for Scroll {
    fn name(&self) -> &'static str {
        "scroll"
    }

    fn render(&mut self, t: u32, frame: &mut BgraFrame) {
        // 6 px per frame: a fast mouse-wheel / page scroll.
        let max = self.canvas.h.saturating_sub(frame.height);
        self.canvas.blit((t * 6) % max.max(1), frame);
    }
}

struct Photo;

impl Scene for Photo {
    fn name(&self) -> &'static str {
        "photo"
    }

    fn render(&mut self, t: u32, frame: &mut BgraFrame) {
        // Smooth colour fields with soft shapes and film grain, panning 2 px per frame.
        let (w, h) = (frame.width, frame.height);
        for y in 0..h {
            let row = frame.row_mut(y);
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let (u, v) = ((x as u32 + 2 * t) as f32, y as f32);
                let r = 128.0 + 70.0 * (u / 190.0).sin() + 40.0 * (v / 97.0).cos();
                let g = 120.0 + 60.0 * ((u + v) / 140.0).sin() + 35.0 * (u / 61.0).cos();
                let b = 130.0 + 70.0 * (v / 160.0).sin() - 30.0 * ((u - v) / 80.0).cos();
                let mut n =
                    (x as u32 + 2 * t).wrapping_mul(0x9E37_79B9) ^ (y).wrapping_mul(0x85EB_CA6B);
                n ^= n >> 15;
                n = n.wrapping_mul(0x2C1B_3C6D);
                let grain = ((n >> 24) as f32 - 128.0) * 0.05;
                px[0] = (b + grain).clamp(0.0, 255.0) as u8;
                px[1] = (g + grain).clamp(0.0, 255.0) as u8;
                px[2] = (r + grain).clamp(0.0, 255.0) as u8;
                px[3] = 255;
            }
        }
        let _ = (w, h);
    }
}

fn luma(p: &[u8]) -> f32 {
    0.0722 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.2126 * f32::from(p[2])
}

fn luma_plane(f: &BgraFrame, w: usize, h: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        let row = f.row(y as u32);
        out.extend(row.chunks_exact(4).take(w).map(luma));
    }
    out
}

/// Quality of one decoded picture against its source.
#[derive(Default, Clone, Copy)]
struct Quality {
    psnr: f64,
    edge_psnr: f64,
    ssim: f64,
}

fn psnr(sse: f64, n: f64) -> f64 {
    if sse <= 0.0 || n <= 0.0 {
        99.0
    } else {
        (10.0 * (255.0 * 255.0 * n / sse).log10()).min(99.0)
    }
}

fn compare(src: &BgraFrame, dec: &BgraFrame) -> Quality {
    let (w, h) = (
        src.width.min(dec.width) as usize,
        src.height.min(dec.height) as usize,
    );
    let a = luma_plane(src, w, h);
    let b = luma_plane(dec, w, h);
    let (mut sse, mut esse, mut en) = (0.0f64, 0.0f64, 0.0f64);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let d = f64::from(a[i] - b[i]);
            sse += d * d;
            if y > 0 && x > 0 && y + 1 < h && x + 1 < w {
                let (mut lo, mut hi) = (f32::MAX, f32::MIN);
                for dy in [y - 1, y, y + 1] {
                    for dx in [x - 1, x, x + 1] {
                        let v = a[dy * w + dx];
                        lo = lo.min(v);
                        hi = hi.max(v);
                    }
                }
                if hi - lo > 48.0 {
                    esse += d * d;
                    en += 1.0;
                }
            }
        }
    }
    // SSIM over 8x8 windows, stride 4 (L = 255).
    let (c1, c2) = (6.5025f64, 58.5225f64);
    let (mut ssum, mut sn) = (0.0f64, 0.0f64);
    let mut y = 0;
    while y + 8 <= h {
        let mut x = 0;
        while x + 8 <= w {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
            for dy in 0..8 {
                for dx in 0..8 {
                    let (p, q) = (
                        f64::from(a[(y + dy) * w + x + dx]),
                        f64::from(b[(y + dy) * w + x + dx]),
                    );
                    sa += p;
                    sb += q;
                    saa += p * p;
                    sbb += q * q;
                    sab += p * q;
                }
            }
            let (ma, mb) = (sa / 64.0, sb / 64.0);
            let (va, vb, cov) = (
                saa / 64.0 - ma * ma,
                sbb / 64.0 - mb * mb,
                sab / 64.0 - ma * mb,
            );
            ssum += ((2.0 * ma * mb + c1) * (2.0 * cov + c2))
                / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            sn += 1.0;
            x += 4;
        }
        y += 4;
    }
    Quality {
        psnr: psnr(sse, (w * h) as f64),
        edge_psnr: if en > 0.0 { psnr(esse, en) } else { 99.0 },
        ssim: if sn > 0.0 { ssum / sn } else { 1.0 },
    }
}

pub(super) fn run(args: &Args, (w, h): (u32, u32)) {
    let frames = args.frames.max(40);
    let bpps: Vec<f64> = if args.bpps.is_empty() {
        vec![0.02, 0.04, 0.06, 0.08, 0.12, 0.2]
    } else {
        args.bpps.clone()
    };
    let mut scenes: Vec<Box<dyn Scene>> = vec![
        Box::new(Ui::new(w, h)),
        Box::new(Code::new(w, h)),
        Box::new(Scroll::new(w, h)),
        Box::new(Photo),
    ];
    if let Some(only) = &args.scene {
        scenes.retain(|s| s.name() == only);
    }
    println!(
        "# quality bench {w}x{h} @ {FPS} fps, {frames} frames/point (first 30 skipped), qp {}:{}",
        args.tuning.qp.min, args.tuning.qp.max
    );
    println!("scene    bpp    kbps-tgt kbps-out   QP   PSNR-Y  PSNR-edge  SSIM    enc-ms");
    for scene in &mut scenes {
        for &bpp in &bpps {
            let kbps = ((f64::from(w) * f64::from(h) * f64::from(FPS) * bpp) / 1000.0) as u32;
            let config = EncoderConfig {
                width: w,
                height: h,
                fps: FPS,
                bitrate_kbps: kbps.max(100),
            };
            let (mut enc, _) = make_encoder(args, config);
            let mut dec = H264Decoder::new().expect("decoder");
            let mut probe = QpProbe::new();
            let mut src = BgraFrame::new_tight(w, h);
            let (mut bytes, mut n, mut qsum, mut qn) = (0u64, 0u32, 0u32, 0u32);
            let (mut acc, mut an) = (Quality::default(), 0u32);
            let (mut worst_edge, mut enc_time) = (99.0f64, 0.0f64);
            for t in 0..frames {
                scene.render(t, &mut src);
                src.timestamp_us = u64::from(t) * 1_000_000 / u64::from(FPS);
                let t0 = Instant::now();
                let pkt = enc.encode(&src).expect("encode");
                enc_time += t0.elapsed().as_secs_f64();
                let Some(pkt) = pkt else { continue };
                let qp = probe.feed(&pkt.data);
                let out = dec.decode(&pkt.data).expect("decode");
                if t >= 30 {
                    bytes += pkt.data.len() as u64;
                    n += 1;
                    if let Some(q) = qp {
                        qsum += u32::from(q);
                        qn += 1;
                    }
                    if let Some(out) = &out {
                        let q = compare(&src, out);
                        acc.psnr += q.psnr;
                        acc.edge_psnr += q.edge_psnr;
                        acc.ssim += q.ssim;
                        worst_edge = worst_edge.min(q.edge_psnr);
                        an += 1;
                    }
                }
                if let Some(out) = out {
                    dec.recycle(out);
                }
            }
            let secs = f64::from(n.max(1)) / f64::from(FPS);
            let a = f64::from(an.max(1));
            let qp = if qn == 0 {
                "  - ".to_string()
            } else {
                format!("{:4.1}", f64::from(qsum) / f64::from(qn))
            };
            println!(
                "{:<8} {:<6} {:>8} {:>8} {} {:>8.2} {:>9.2} {:>7.4} {:>8.2}   (worst edge {:.1} dB, {})",
                scene.name(),
                bpp,
                kbps,
                (bytes as f64 * 8.0 / secs / 1000.0) as u64,
                qp,
                acc.psnr / a,
                acc.edge_psnr / a,
                acc.ssim / a,
                enc_time * 1000.0 / f64::from(frames),
                worst_edge,
                enc.backend_label(),
            );
        }
    }
}
