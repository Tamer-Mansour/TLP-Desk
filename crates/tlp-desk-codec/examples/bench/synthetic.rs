//! `--synthetic WxH`: encode generated desktop-like frames (no screen capture) and report
//! encode time, encoder CPU, bitrate and PSNR after decoding with OpenH264.

use std::time::{Duration, Instant};

use tlp_desk_capture::BgraFrame;
use tlp_desk_codec::convert::{bgra_to_i420, I420Buffer};
use tlp_desk_codec::{EncoderConfig, H264Decoder, VideoDecoder};

use super::{draw_motion, make_encoder, pace, process_cpu_time, Args, Stage};

/// Gradient desktop with a 600x300 "document" whose dark text-like strokes scroll 2 px per
/// frame, plus (with `noise`) a 400x300 block of scrolling noise that no bitrate can afford.
fn render(frame: &mut BgraFrame, t: u32, noise: bool) {
    let (w, h) = (frame.width, frame.height);
    for y in 0..h {
        let row = frame.row_mut(y);
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let x = x as u32;
            let doc = (40..640).contains(&x) && (300..600).contains(&y);
            let v = if doc {
                let line = (y + 2 * t) % 18;
                let ink = line < 11 && (x / 3 + (y + 2 * t) / 18 * 7) % 5 < 3 && x % 9 != 0;
                if ink {
                    [30, 30, 30, 255]
                } else {
                    [252, 252, 252, 255]
                }
            } else if noise && x >= w.saturating_sub(440) && x < w.saturating_sub(40) && y < 300 {
                let mut v = x.wrapping_mul(0x9E37_79B9) ^ (y + 3 * t).wrapping_mul(0x85EB_CA6B);
                v ^= v >> 15;
                v = v.wrapping_mul(0x2C1B_3C6D);
                v ^= v >> 12;
                [v as u8, (v >> 8) as u8, (v >> 16) as u8, 255]
            } else {
                [
                    (x * 255 / w) as u8,
                    (y * 255 / h) as u8,
                    ((x + y) * 255 / (w + h)) as u8,
                    255,
                ]
            };
            px.copy_from_slice(&v);
        }
    }
    draw_motion(frame, t);
}

/// PSNR over B, G, R of the area both frames cover.
fn psnr(a: &BgraFrame, b: &BgraFrame) -> f64 {
    let (w, h) = (a.width.min(b.width), a.height.min(b.height));
    let mut sse = 0u64;
    for y in 0..h {
        let (ra, rb) = (&a.row(y)[..w as usize * 4], &b.row(y)[..w as usize * 4]);
        for (p, q) in ra.chunks_exact(4).zip(rb.chunks_exact(4)) {
            for c in 0..3 {
                let d = i64::from(p[c]) - i64::from(q[c]);
                sse += (d * d) as u64;
            }
        }
    }
    let mse = sse as f64 / (f64::from(w) * f64::from(h) * 3.0);
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

pub(super) fn run(args: &Args, (w, h): (u32, u32)) {
    let mut config = EncoderConfig::for_screen(w, h, args.fps);
    if let Some(kbps) = args.kbps {
        config.bitrate_kbps = kbps;
    }
    let (mut enc, (ew, eh)) = make_encoder(args, config);
    let mut dec = H264Decoder::new().expect("decoder");
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    println!(
        "synthetic {w}x{h} -> encoded {ew}x{eh} | {} fps, {} kbps | {} | noise: {} | {} logical CPUs",
        config.fps,
        config.bitrate_kbps,
        enc.backend_label(),
        args.noise,
        cpus
    );

    let mut frame = BgraFrame::new_tight(w, h);
    let mut s_enc = Stage::default();
    let mut s_dec = Stage::default();
    let mut s_conv = Stage::default();
    let mut yuv = I420Buffer::default();
    let (mut bytes, mut encoded, mut keyframes) = (0u64, 0u32, 0u32);
    let (mut psnr_sum, mut psnr_min, mut psnr_n) = (0f64, f64::MAX, 0u32);
    let mut enc_cpu = Duration::ZERO;
    let interval = Duration::from_secs_f64(1.0 / f64::from(args.fps.max(1)));

    // Warm-up frame (encoder start-up and the first IDR are not part of the steady state).
    render(&mut frame, 0, args.noise);
    let first = enc.encode(&frame).expect("encode").expect("first packet");
    dec.decode(&first.data)
        .expect("decode")
        .expect("first picture");

    let start = Instant::now();
    let mut next_tick = start;
    let mut t = 1u32;
    while pace(start, &mut next_tick, interval, args.seconds) {
        render(&mut frame, t, args.noise);
        frame.timestamp_us = u64::from(t) * 1_000_000 / u64::from(args.fps.max(1));
        let cpu0 = process_cpu_time();
        let te = Instant::now();
        let pkt = enc.encode(&frame).expect("encode");
        s_enc.add(te.elapsed());
        enc_cpu += process_cpu_time().saturating_sub(cpu0);
        t += 1;
        let Some(pkt) = pkt else { continue };
        encoded += 1;
        keyframes += u32::from(pkt.keyframe);
        bytes += pkt.data.len() as u64;
        let td = Instant::now();
        let out = dec.decode(&pkt.data).expect("decode");
        s_dec.add(td.elapsed());
        if let Some(out) = out {
            // PSNR on every 4th frame keeps the loop at the target rate on slow machines.
            if t % 4 == 0 {
                let q = psnr(&frame, &out);
                psnr_sum += q;
                psnr_min = psnr_min.min(q);
                psnr_n += 1;
                // The colour conversion alone, to separate it from the encoder proper.
                let tc = Instant::now();
                bgra_to_i420(&frame, &mut yuv);
                s_conv.add(tc.elapsed());
            }
            dec.recycle(out);
        }
    }
    let secs = start.elapsed().as_secs_f64();
    let cpu = enc_cpu.as_secs_f64();
    println!("ran {secs:.1} s, {} frames, {encoded} encoded", t - 1);
    println!("encode (incl. conversion):  {}", s_enc.summary());
    println!("bgra->i420 alone (sampled): {}", s_conv.summary());
    println!("decode (OpenH264):          {}", s_dec.summary());
    println!(
        "bitrate: {:.2} Mbps avg, {:.1} KB/frame, {keyframes} keyframes",
        bytes as f64 * 8.0 / secs / 1e6,
        bytes as f64 / f64::from(encoded.max(1)) / 1024.0
    );
    println!(
        "PSNR vs source: avg {:.2} dB, min {:.2} dB (n={psnr_n})",
        psnr_sum / f64::from(psnr_n.max(1)),
        if psnr_n == 0 { 0.0 } else { psnr_min }
    );
    println!(
        "encoder CPU (process CPU during encode calls): {:.1}% of one core, {:.2}% of the machine",
        cpu / secs * 100.0,
        cpu / secs * 100.0 / cpus as f64
    );
}
