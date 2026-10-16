//! Encode -> decode tests on synthetic frames.

use tlp_desk_capture::BgraFrame;

use crate::{
    CodecError, Downscale, EncoderConfig, H264Decoder, H264Encoder, H264Tuning, VideoDecoder,
    VideoEncoder,
};

/// Gradient background plus a box that moves with `t`, with a padded stride.
fn synthetic_frame(w: u32, h: u32, t: u32) -> BgraFrame {
    let stride = w * 4 + 64;
    let mut f = BgraFrame {
        width: w,
        height: h,
        stride,
        data: vec![0xAB; (stride * h) as usize],
        timestamp_us: t as u64 * 16_667,
    };
    let (bx, by) = (
        (t * 37) % w.saturating_sub(120).max(1),
        (t * 23) % h.saturating_sub(90).max(1),
    );
    for y in 0..h {
        let row = f.row_mut(y);
        for x in 0..w {
            let px = &mut row[x as usize * 4..x as usize * 4 + 4];
            let in_box = x >= bx && x < bx + 120 && y >= by && y < by + 90;
            if in_box {
                px.copy_from_slice(&[40, 200, 240, 255]);
            } else {
                px.copy_from_slice(&[
                    (x * 255 / w) as u8,
                    (y * 255 / h) as u8,
                    ((x + y) * 255 / (w + h)) as u8,
                    255,
                ]);
            }
        }
    }
    f
}

/// Gradient desktop with a 400x300 "window" of dense noise scrolling 3 px per frame (like
/// scrolling small text): expensive to code, so rate control has something to bite on.
fn noisy_frame(w: u32, h: u32, t: u32) -> BgraFrame {
    let mut f = synthetic_frame(w, h, 0);
    for y in 0..300.min(h) {
        let row = f.row_mut(y);
        for (x, px) in row.chunks_exact_mut(4).take(400).enumerate() {
            let mut v =
                (x as u32).wrapping_mul(0x9E37_79B9) ^ (y + 3 * t).wrapping_mul(0x85EB_CA6B);
            v ^= v >> 15;
            v = v.wrapping_mul(0x2C1B_3C6D);
            v ^= v >> 12;
            px.copy_from_slice(&[v as u8, (v >> 8) as u8, (v >> 16) as u8, 255]);
        }
    }
    f
}

/// PSNR over the BGR channels of the overlapping area.
fn psnr(a: &BgraFrame, b: &BgraFrame) -> f64 {
    let (w, h) = (a.width.min(b.width), a.height.min(b.height));
    let mut sse = 0f64;
    for y in 0..h {
        let (ra, rb) = (a.row(y), b.row(y));
        for i in 0..(w as usize * 4) {
            if i % 4 == 3 {
                continue;
            }
            let d = ra[i] as f64 - rb[i] as f64;
            sse += d * d;
        }
    }
    let mse = sse / (w as f64 * h as f64 * 3.0);
    if mse == 0.0 {
        return 99.0;
    }
    10.0 * (255.0 * 255.0 / mse).log10()
}

fn config(w: u32, h: u32) -> EncoderConfig {
    EncoderConfig {
        width: w,
        height: h,
        fps: 60,
        bitrate_kbps: 8_000,
    }
}

fn is_annex_b(data: &[u8]) -> bool {
    data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1])
}

/// NAL unit types present in an Annex-B buffer.
fn nal_types(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            out.push(data[i + 3] & 0x1F);
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn encode_decode_1080p_matches_dimensions_and_psnr() {
    let (w, h) = (1920, 1080);
    let mut enc = H264Encoder::new(config(w, h)).unwrap();
    let mut dec = H264Decoder::new().unwrap();
    let mut decoded = 0;
    for t in 0..20 {
        let src = synthetic_frame(w, h, t);
        let Some(pkt) = enc.encode(&src).unwrap() else {
            continue;
        };
        assert!(is_annex_b(&pkt.data));
        assert_eq!(pkt.timestamp_us, src.timestamp_us);
        if t == 0 {
            assert!(pkt.keyframe, "first frame must be a keyframe");
            let types = nal_types(&pkt.data);
            assert!(
                types.contains(&7) && types.contains(&8) && types.contains(&5),
                "{types:?}"
            );
        }
        let out = dec
            .decode(&pkt.data)
            .unwrap()
            .expect("one picture per access unit");
        assert_eq!((out.width, out.height), (w, h));
        assert_eq!(out.stride, w * 4);
        let q = psnr(&src, &out);
        assert!(q > 32.0, "frame {t}: PSNR {q:.1} dB too low");
        dec.recycle(out);
        decoded += 1;
    }
    assert_eq!(decoded, 20);
}

#[test]
fn keyframe_request_yields_idr() {
    let (w, h) = (640, 360);
    let mut enc = H264Encoder::new(config(w, h)).unwrap();
    let mut kinds = Vec::new();
    for t in 0..10 {
        if t == 6 {
            enc.request_keyframe();
        }
        let pkt = enc.encode(&synthetic_frame(w, h, t)).unwrap().unwrap();
        kinds.push(pkt.keyframe);
        if t == 6 {
            assert!(
                nal_types(&pkt.data).contains(&5),
                "requested keyframe is not an IDR"
            );
        }
    }
    assert_eq!(
        kinds,
        vec![true, false, false, false, false, false, true, false, false, false]
    );
}

#[test]
fn decoder_can_join_at_requested_keyframe() {
    // A viewer that joins mid-stream must be able to start from a requested IDR.
    let (w, h) = (320, 240);
    let mut enc = H264Encoder::new(config(w, h)).unwrap();
    for t in 0..5 {
        enc.encode(&synthetic_frame(w, h, t)).unwrap();
    }
    enc.request_keyframe();
    let mut dec = H264Decoder::new().unwrap();
    for t in 5..10 {
        let src = synthetic_frame(w, h, t);
        let pkt = enc.encode(&src).unwrap().unwrap();
        let out = dec.decode(&pkt.data).unwrap().expect("picture");
        assert!(psnr(&src, &out) > 30.0);
    }
}

#[test]
fn odd_dimensions_are_handled() {
    let (w, h) = (333, 199);
    let mut enc = H264Encoder::new(config(w, h)).unwrap();
    let mut dec = H264Decoder::new().unwrap();
    for t in 0..5 {
        let src = synthetic_frame(w, h, t);
        let pkt = enc.encode(&src).unwrap().unwrap();
        let out = dec.decode(&pkt.data).unwrap().expect("picture");
        // Odd sizes are padded to even by duplicating the edge; the decoded picture is the
        // padded size and the visible area matches the source.
        assert_eq!((out.width, out.height), (334, 200));
        assert!(psnr(&src, &out) > 30.0);
    }
}

#[test]
fn size_mismatch_is_rejected() {
    let mut enc = H264Encoder::new(config(640, 360)).unwrap();
    let err = enc.encode(&synthetic_frame(320, 240, 0)).unwrap_err();
    assert!(matches!(
        err,
        CodecError::SizeMismatch {
            got_w: 320,
            got_h: 240,
            want_w: 640,
            want_h: 360
        }
    ));
}

#[test]
fn bitrate_change_takes_effect_without_keyframe() {
    let (w, h) = (1280, 720);
    let mut enc = H264Encoder::new(config(w, h)).unwrap();
    // Before the first frame, too.
    enc.set_bitrate(20_000).unwrap();
    let mut high = 0usize;
    for t in 0..30 {
        let pkt = enc.encode(&noisy_frame(w, h, t)).unwrap().unwrap();
        if t >= 10 {
            high += pkt.data.len();
        }
    }
    enc.set_bitrate(500).unwrap();
    assert_eq!(enc.config().bitrate_kbps, 500);
    let mut low = 0usize;
    for t in 30..60 {
        let pkt = enc.encode(&noisy_frame(w, h, t)).unwrap().unwrap();
        assert!(
            !pkt.keyframe,
            "bitrate change must not force a keyframe (frame {t})"
        );
        if t >= 40 {
            low += pkt.data.len();
        }
    }
    assert!(
        low * 2 < high,
        "lower bitrate did not shrink output: {high} -> {low}"
    );
}

#[test]
fn garbage_bitstream_is_an_error_or_no_picture() {
    let mut dec = H264Decoder::new().unwrap();
    let r = dec.decode(&[0, 0, 0, 1, 0x65, 0xFF, 0x00, 0x12, 0x34]);
    assert!(matches!(r, Err(CodecError::BadBitstream) | Ok(None)));
}

#[test]
fn screen_config_defaults() {
    let c = EncoderConfig::for_screen(1920, 1080, 60);
    assert_eq!((c.width, c.height, c.fps), (1920, 1080, 60));
    assert!(
        (10_000..=15_000).contains(&c.bitrate_kbps),
        "{}",
        c.bitrate_kbps
    );
    let c30 = EncoderConfig::for_screen(1920, 1080, 30);
    assert!(c30.bitrate_kbps < c.bitrate_kbps && c30.bitrate_kbps >= 5_000);
    // Oversized sources are budgeted for the half-size picture they are encoded at.
    let big = EncoderConfig::for_screen(3840, 2400, 60);
    assert_eq!(
        big.bitrate_kbps,
        EncoderConfig::for_screen(1920, 1200, 60).bitrate_kbps
    );
}

#[test]
fn prescaled_frames_are_encoded_at_their_own_size() {
    // A capturer scaling 3840x2400 to 1920x1200 on the GPU: the encoder must not halve again.
    let cfg = EncoderConfig::for_screen(1920, 1200, 60);
    let mut enc = H264Encoder::new(cfg).unwrap();
    assert_eq!(enc.encoded_size(), (1920, 1200));
    let mut dec = H264Decoder::new().unwrap();
    let src = synthetic_frame(1920, 1200, 1);
    let pkt = enc.encode(&src).unwrap().unwrap();
    let out = dec.decode(&pkt.data).unwrap().expect("picture");
    assert_eq!((out.width, out.height), (1920, 1200));
    assert!(psnr(&src, &out) > 30.0);
    // 16:10 scaled into a 16:9 box (1728x1080) also stays as is.
    let enc = H264Encoder::new(EncoderConfig::for_screen(1728, 1080, 60)).unwrap();
    assert_eq!(enc.encoded_size(), (1728, 1080));
}

#[test]
fn oversized_source_is_downscaled_automatically() {
    // 3840x2400 (16:10 4K) exceeds OpenH264's 3840x2160 limit: Auto encodes at half size.
    let (w, h) = (3840, 2400);
    let mut enc = H264Encoder::new(config(w, h)).unwrap();
    assert_eq!(enc.encoded_size(), (1920, 1200));
    let mut dec = H264Decoder::new().unwrap();
    for t in 0..3 {
        let pkt = enc.encode(&synthetic_frame(w, h, t)).unwrap().unwrap();
        let out = dec.decode(&pkt.data).unwrap().expect("picture");
        assert_eq!((out.width, out.height), (1920, 1200));
        // Compare with a half-size rendering of the same scene (box positions scale too, so
        // only check overall quality loosely).
        assert!(psnr(&synthetic_frame(1920, 1200, 0), &out) > 10.0);
    }
    let never = H264Tuning {
        downscale: Downscale::Never,
        ..H264Tuning::default()
    };
    assert!(H264Encoder::with_tuning(config(w, h), never).is_err());
}

#[test]
fn forced_half_downscale_keeps_quality() {
    let (w, h) = (1280, 720);
    let tuning = H264Tuning {
        downscale: Downscale::Half,
        ..H264Tuning::default()
    };
    let mut enc = H264Encoder::with_tuning(config(w, h), tuning).unwrap();
    assert_eq!(enc.encoded_size(), (640, 360));
    let mut dec = H264Decoder::new().unwrap();
    let src = synthetic_frame(w, h, 0);
    let pkt = enc.encode(&src).unwrap().unwrap();
    let out = dec.decode(&pkt.data).unwrap().expect("picture");
    assert_eq!((out.width, out.height), (640, 360));
    // Upsample the decoded frame (nearest) and compare with the source.
    let mut up = BgraFrame::new_tight(w, h);
    for y in 0..h {
        for x in 0..w {
            let s = &out.row(y / 2)[(x / 2) as usize * 4..(x / 2) as usize * 4 + 4];
            up.row_mut(y)[x as usize * 4..x as usize * 4 + 4].copy_from_slice(s);
        }
    }
    let q = psnr(&src, &up);
    assert!(q > 25.0, "half-res PSNR {q:.1}");
}

/// Average slice QP the probe reads from `frames` noisy frames encoded at `kbps`.
fn probed_qp(kbps: u32, tuning: H264Tuning) -> (f64, u8) {
    let mut enc = H264Encoder::with_tuning(
        EncoderConfig {
            width: 640,
            height: 480,
            fps: 30,
            bitrate_kbps: kbps,
        },
        tuning,
    )
    .unwrap();
    let mut probe = crate::QpProbe::new();
    let (mut sum, mut n, mut worst) = (0u32, 0u32, 0u8);
    for t in 0..30 {
        let Some(pkt) = enc.encode(&noisy_frame(640, 480, t)).unwrap() else {
            continue;
        };
        let qp = probe
            .feed(&pkt.data)
            .expect("QP readable from OpenH264 output");
        sum += u32::from(qp);
        n += 1;
        worst = worst.max(qp);
    }
    (f64::from(sum) / f64::from(n.max(1)), worst)
}

#[test]
fn quality_floor_caps_the_quantiser_and_the_probe_reads_it() {
    let (rich, _) = probed_qp(20_000, H264Tuning::default());
    let (starved, worst) = probed_qp(100, H264Tuning::default());
    assert!(rich < starved, "rich {rich} vs starved {starved}");
    assert!(worst <= crate::QUALITY_MAX_QP, "QP {worst} above the floor");
    // Without the floor the same starved stream goes far coarser.
    let loose = H264Tuning {
        qp: crate::QpBounds::UNBOUNDED,
        ..H264Tuning::default()
    };
    let (_, loose_worst) = probed_qp(100, loose);
    assert!(
        loose_worst > crate::QUALITY_MAX_QP,
        "unfloored QP only {loose_worst}"
    );
}
