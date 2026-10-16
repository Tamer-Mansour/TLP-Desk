//! Colour fidelity through the real encode -> decode path: BGRA -> BT.709 limited-range I420 ->
//! H.264 -> I420 -> BGRA must not wash out, darken or tint flat colours.

use tlp_desk_capture::BgraFrame;
use tlp_desk_codec::{new_decoder, EncoderConfig, H264Encoder, VideoEncoder};

fn flat(w: u32, h: u32, bgr: [u8; 3]) -> BgraFrame {
    let mut f = BgraFrame::new_tight(w, h);
    for p in f.data.chunks_exact_mut(4) {
        p.copy_from_slice(&[bgr[0], bgr[1], bgr[2], 255]);
    }
    f
}

#[test]
fn flat_colours_survive_the_codec_without_a_matrix_or_range_error() {
    let colours: [([u8; 3], &str); 9] = [
        ([255, 255, 255], "white"),
        ([0, 0, 0], "black"),
        ([128, 128, 128], "grey"),
        ([0, 0, 255], "red"),
        ([0, 255, 0], "green"),
        ([255, 0, 0], "blue"),
        ([0, 200, 255], "amber"),
        ([40, 40, 40], "dark grey (dark theme)"),
        ([245, 245, 245], "light grey (light theme)"),
    ];
    for (bgr, name) in colours {
        let cfg = EncoderConfig {
            width: 128,
            height: 64,
            fps: 30,
            bitrate_kbps: 4_000,
        };
        let mut enc = H264Encoder::new(cfg).unwrap();
        let mut dec = new_decoder().unwrap();
        let src = flat(128, 64, bgr);
        let out = enc.encode(&src).unwrap().unwrap();
        let got = dec.decode(&out.data).unwrap().unwrap();
        let (cx, cy) = (64usize, 32usize);
        let px = &got.row(cy as u32)[cx * 4..cx * 4 + 3];
        for c in 0..3 {
            let err = i32::from(px[c]).abs_diff(i32::from(bgr[c]));
            assert!(
                err <= 6,
                "{name}: channel {c} source {} decoded {} (err {err})",
                bgr[c],
                px[c]
            );
        }
    }
}
