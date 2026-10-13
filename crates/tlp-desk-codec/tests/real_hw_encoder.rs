//! Hardware (Media Foundation) encoder against the OpenH264 decoder the viewer uses.
//!
//! Needs a GPU with an H.264 encoder MFT, so every test is `#[ignore]`. Nothing touches the
//! screen: frames are synthetic. Run on a Windows machine with:
//!
//! ```text
//! cargo test -p tlp-desk-codec --test real_hw_encoder -- --ignored --test-threads 1
//! ```
//!
//! The per-vendor tests skip (pass with a note) when that GPU is not present.
#![cfg(windows)]

/// NVIDIA Optimus laptops (Intel iGPU drives the panel, NVIDIA dGPU renders on demand) only
/// let a process activate the NVIDIA encoder MFT when the process runs on the dGPU; otherwise
/// `ActivateObject` fails with E_UNEXPECTED. NVIDIA's documented opt-in is an exported
/// `NvOptimusEnablement` DWORD = 1 in the executable; the `.drectve` linker directive exports
/// it from this test binary only.
#[allow(unsafe_code, non_upper_case_globals)]
mod optimus {
    #[no_mangle]
    #[used]
    pub static NvOptimusEnablement: u32 = 1;
    #[link_section = ".drectve"]
    #[used]
    static EXPORT_DIRECTIVE: [u8; 34] = *b" /EXPORT:NvOptimusEnablement,DATA ";
}

use tlp_desk_capture::BgraFrame;
use tlp_desk_codec::{
    list_adapters, list_hw_encoders, new_encoder_with, EncoderConfig, EncoderOptions,
    EncoderPreference, H264Decoder, HwAdapter, MfEncoder, VideoDecoder, VideoEncoder, VENDOR_INTEL,
    VENDOR_NVIDIA,
};

/// Gradient desktop, a moving box, and a block of fine text-like stripes.
fn frame(w: u32, h: u32, t: u32) -> BgraFrame {
    let mut f = BgraFrame::new_tight(w, h);
    f.timestamp_us = u64::from(t) * 16_667;
    let (bx, by) = ((t * 17) % (w - 200), (t * 9) % (h - 150));
    for y in 0..h {
        for (x, px) in f.row_mut(y).chunks_exact_mut(4).enumerate() {
            let x = x as u32;
            let v = if x >= bx && x < bx + 200 && y >= by && y < by + 150 {
                [40, 200, 240, 255]
            } else if x < 400 && y < 200 {
                let ink = (x / 2 + y / 3 + t) % 7 == 0;
                if ink {
                    [20, 20, 20, 255]
                } else {
                    [250, 250, 250, 255]
                }
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
    f
}

/// [`frame`] plus a 400x300 block of dense noise scrolling 3 px per frame: expensive to code,
/// so rate control has something to bite on.
fn noisy_frame(w: u32, h: u32, t: u32) -> BgraFrame {
    let mut f = frame(w, h, t);
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

fn psnr(a: &BgraFrame, b: &BgraFrame) -> f64 {
    let (w, h) = (a.width.min(b.width), a.height.min(b.height));
    let mut sse = 0f64;
    for y in 0..h {
        for (p, q) in a.row(y)[..w as usize * 4]
            .chunks_exact(4)
            .zip(b.row(y)[..w as usize * 4].chunks_exact(4))
        {
            for c in 0..3 {
                let d = p[c] as f64 - q[c] as f64;
                sse += d * d;
            }
        }
    }
    let mse = sse / (w as f64 * h as f64 * 3.0);
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

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

fn cfg(w: u32, h: u32, kbps: u32) -> EncoderConfig {
    EncoderConfig {
        width: w,
        height: h,
        fps: 60,
        bitrate_kbps: kbps,
    }
}

/// Opens the encoder for `vendor`, or returns `None` (test skipped) when that GPU is absent.
fn open(vendor: u32, config: EncoderConfig) -> Option<MfEncoder> {
    let present = list_adapters()
        .unwrap()
        .iter()
        .any(|a| a.vendor_id == vendor);
    if !present {
        eprintln!("no adapter with vendor {vendor:#06x}: skipped");
        return None;
    }
    let enc = MfEncoder::new(config, HwAdapter::Vendor(vendor)).expect("hardware encoder");
    eprintln!("using {:?}", enc.info());
    Some(enc)
}

/// Every frame comes out, is Annex-B, decodes with OpenH264 at the right size and quality.
fn decodes_with_openh264(vendor: u32) {
    let (w, h) = (1920, 1080);
    let Some(mut enc) = open(vendor, cfg(w, h, 12_000)) else {
        return;
    };
    let mut dec = H264Decoder::new().unwrap();
    let mut worst = f64::MAX;
    let mut keyframes = 0;
    for t in 0..120 {
        let src = frame(w, h, t);
        let pkt = enc
            .encode(&src)
            .unwrap()
            .expect("one access unit per frame");
        assert!(pkt.data.starts_with(&[0, 0, 0, 1]) || pkt.data.starts_with(&[0, 0, 1]));
        assert_eq!(pkt.timestamp_us, src.timestamp_us);
        let types = nal_types(&pkt.data);
        if t == 0 {
            assert!(pkt.keyframe, "first frame must be an IDR");
            assert!(types.contains(&7) && types.contains(&8), "{types:?}");
        }
        assert_eq!(
            pkt.keyframe,
            types.contains(&5),
            "keyframe flag matches NALs"
        );
        keyframes += u32::from(pkt.keyframe);
        let out = dec.decode(&pkt.data).unwrap().expect("picture");
        assert_eq!((out.width, out.height), (w, h));
        let q = psnr(&src, &out);
        worst = worst.min(q);
        dec.recycle(out);
    }
    eprintln!("worst PSNR {worst:.1} dB, {keyframes} keyframes in 120 frames");
    assert!(worst > 30.0, "PSNR {worst:.1} dB too low");
    assert_eq!(keyframes, 1, "no periodic IDRs: keyframes only on request");
}

/// A requested keyframe is an IDR carrying SPS/PPS, so a viewer can join there.
fn keyframe_request_yields_idr(vendor: u32) {
    let (w, h) = (1280, 720);
    let Some(mut enc) = open(vendor, cfg(w, h, 6_000)) else {
        return;
    };
    for t in 0..10 {
        let pkt = enc.encode(&frame(w, h, t)).unwrap().unwrap();
        assert_eq!(pkt.keyframe, t == 0, "frame {t}");
    }
    enc.request_keyframe();
    let src = frame(w, h, 10);
    let pkt = enc.encode(&src).unwrap().unwrap();
    assert!(pkt.keyframe, "requested keyframe");
    let types = nal_types(&pkt.data);
    assert!(
        types.contains(&5) && types.contains(&7) && types.contains(&8),
        "{types:?}"
    );
    let mut late_viewer = H264Decoder::new().unwrap();
    let out = late_viewer.decode(&pkt.data).unwrap().expect("picture");
    assert!(psnr(&src, &out) > 30.0);
    let next = enc.encode(&frame(w, h, 11)).unwrap().unwrap();
    assert!(!next.keyframe);
    assert!(late_viewer.decode(&next.data).unwrap().is_some());
}

/// Lowering the bitrate mid-stream shrinks the output without a keyframe.
fn bitrate_change_works(vendor: u32) {
    let (w, h) = (1280, 720);
    let Some(mut enc) = open(vendor, cfg(w, h, 20_000)) else {
        return;
    };
    let mut high = 0usize;
    for t in 0..60 {
        let pkt = enc.encode(&noisy_frame(w, h, t)).unwrap().unwrap();
        if t >= 20 {
            high += pkt.data.len();
        }
    }
    enc.set_bitrate(1_000).unwrap();
    assert_eq!(enc.config().bitrate_kbps, 1_000);
    let mut low = 0usize;
    for t in 60..120 {
        let pkt = enc.encode(&noisy_frame(w, h, t)).unwrap().unwrap();
        assert!(!pkt.keyframe, "bitrate change must not force a keyframe");
        if t >= 80 {
            low += pkt.data.len();
        }
    }
    eprintln!("bytes over 40 frames: 20 Mbps {high}, 1 Mbps {low}");
    assert!(
        low * 2 < high,
        "lower bitrate did not shrink output: {high} -> {low}"
    );
}

/// Odd sizes are padded to even like the software encoder does.
fn odd_size_matches_software_policy(vendor: u32) {
    let (w, h) = (333, 199);
    let Some(mut enc) = open(vendor, cfg(w, h, 2_000)) else {
        return;
    };
    assert_eq!(enc.encoded_size(), (334, 200));
    let mut dec = H264Decoder::new().unwrap();
    for t in 0..5 {
        let src = frame(w, h, t);
        let pkt = enc.encode(&src).unwrap().unwrap();
        let out = dec.decode(&pkt.data).unwrap().expect("picture");
        assert_eq!((out.width, out.height), (334, 200));
        assert!(psnr(&src, &out) > 28.0);
    }
}

#[test]
#[ignore = "needs an Intel GPU with Quick Sync"]
fn intel_decodes_with_openh264() {
    decodes_with_openh264(VENDOR_INTEL);
}

#[test]
#[ignore = "needs an NVIDIA GPU"]
fn nvidia_decodes_with_openh264() {
    decodes_with_openh264(VENDOR_NVIDIA);
}

#[test]
#[ignore = "needs an Intel GPU with Quick Sync"]
fn intel_keyframe_request_yields_idr() {
    keyframe_request_yields_idr(VENDOR_INTEL);
}

#[test]
#[ignore = "needs an NVIDIA GPU"]
fn nvidia_keyframe_request_yields_idr() {
    keyframe_request_yields_idr(VENDOR_NVIDIA);
}

#[test]
#[ignore = "needs an Intel GPU with Quick Sync"]
fn intel_bitrate_change_works() {
    bitrate_change_works(VENDOR_INTEL);
}

#[test]
#[ignore = "needs an NVIDIA GPU"]
fn nvidia_bitrate_change_works() {
    bitrate_change_works(VENDOR_NVIDIA);
}

#[test]
#[ignore = "needs an Intel GPU with Quick Sync"]
fn intel_odd_size_matches_software_policy() {
    odd_size_matches_software_policy(VENDOR_INTEL);
}

#[test]
#[ignore = "needs an NVIDIA GPU"]
fn nvidia_odd_size_matches_software_policy() {
    odd_size_matches_software_policy(VENDOR_NVIDIA);
}

#[test]
#[ignore = "needs a GPU with an H.264 encoder MFT"]
fn hardware_preference_opens_a_hardware_encoder() {
    eprintln!("adapters: {:#?}", list_adapters().unwrap());
    let encoders = list_hw_encoders().unwrap();
    eprintln!("encoders: {encoders:#?}");
    assert!(!encoders.is_empty());
    let options = EncoderOptions {
        preference: EncoderPreference::Hardware,
        ..EncoderOptions::default()
    };
    let mut enc = new_encoder_with(cfg(640, 360, 2_000), &options).unwrap();
    assert!(
        enc.backend_label().starts_with("hardware"),
        "{}",
        enc.backend_label()
    );
    let pkt = enc.encode(&frame(640, 360, 0)).unwrap().unwrap();
    assert!(pkt.keyframe);
}
