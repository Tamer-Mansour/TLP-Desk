//! Perceived sharpness through the real encode -> decode pipeline: text scrolled at a LAN-like
//! bitrate is soft; one refine picture (`VideoEncoder::encode_refine`) makes it crisp.
//! Run with `--nocapture` to see the measured PSNR / SSIM numbers.

mod common;

use common::{psnr, ssim, text_scene};
use tlp_desk_codec::{new_decoder, EncoderConfig, H264Encoder, VideoDecoder, VideoEncoder};

fn decode_one(dec: &mut dyn VideoDecoder, data: &[u8]) -> tlp_desk_capture::BgraFrame {
    dec.decode(data).expect("decode").expect("picture")
}

#[test]
fn refine_picture_is_much_sharper_than_a_moving_frame() {
    let (w, h) = (1280, 720);
    let cfg = EncoderConfig {
        width: w,
        height: h,
        fps: 30,
        bitrate_kbps: 2_500,
    };
    let mut enc = H264Encoder::new(cfg).unwrap();
    let mut dec = new_decoder().unwrap();
    let mut last_src = None;
    let mut last_dec = None;
    for i in 0..24u32 {
        let src = text_scene(w, h, 2, i * 37);
        let mut f = src.clone();
        f.timestamp_us = u64::from(i) * 33_333;
        let out = enc.encode(&f).unwrap().expect("frame");
        last_dec = Some(decode_one(dec.as_mut(), &out.data));
        last_src = Some(src);
    }
    let (src, moving) = (last_src.unwrap(), last_dec.unwrap());
    let moving_psnr = psnr(&src, &moving);
    let moving_ssim = ssim(&src, &moving);

    let mut f = src.clone();
    f.timestamp_us = 24 * 33_333;
    let refined = enc.encode_refine(&f).unwrap().expect("refine frame");
    assert!(refined.keyframe, "a refresh is self-contained");
    let refined_dec = decode_one(dec.as_mut(), &refined.data);
    let refined_psnr = psnr(&src, &refined_dec);
    let refined_ssim = ssim(&src, &refined_dec);
    println!(
        "moving : {moving_psnr:.1} dB  SSIM {moving_ssim:.4}\nrefined: {refined_psnr:.1} dB  SSIM {refined_ssim:.4}  ({} KB)",
        refined.data.len() / 1024
    );
    assert!(
        refined_psnr > moving_psnr + 3.0,
        "refine {refined_psnr:.1} dB vs moving {moving_psnr:.1} dB"
    );
    assert!(refined_ssim > moving_ssim);

    // The next ordinary frame is a cheap P-frame again (rate control was restored).
    let mut f2 = src.clone();
    f2.timestamp_us = 25 * 33_333;
    let next = enc.encode(&f2).unwrap().expect("frame");
    assert!(!next.keyframe);
    assert!(next.data.len() < refined.data.len() / 4);
    let _ = decode_one(dec.as_mut(), &next.data);
}

/// A backend without quantiser control: only the trait's default `encode_refine` applies.
struct PlainEncoder(H264Encoder);

impl VideoEncoder for PlainEncoder {
    fn encode(
        &mut self,
        f: &tlp_desk_capture::BgraFrame,
    ) -> Result<Option<tlp_desk_codec::EncodedFrame>, tlp_desk_codec::CodecError> {
        self.0.encode(f)
    }
    fn request_keyframe(&mut self) {
        self.0.request_keyframe();
    }
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), tlp_desk_codec::CodecError> {
        self.0.set_bitrate(kbps)
    }
    fn config(&self) -> EncoderConfig {
        self.0.config()
    }
}

#[test]
fn default_refine_forces_a_bigger_keyframe_and_restores_the_bitrate() {
    let (w, h) = (1280, 720);
    let cfg = EncoderConfig {
        width: w,
        height: h,
        fps: 30,
        bitrate_kbps: 2_500,
    };
    let mut enc = PlainEncoder(H264Encoder::new(cfg).unwrap());
    let mut dec = new_decoder().unwrap();
    let mut sizes = Vec::new();
    let mut moving = None;
    for i in 0..12u32 {
        let src = text_scene(w, h, 2, i * 37);
        let mut f = src.clone();
        f.timestamp_us = u64::from(i) * 33_333;
        let out = enc.encode(&f).unwrap().unwrap();
        moving = Some((psnr(&src, &decode_one(dec.as_mut(), &out.data)), src));
        sizes.push(out.data.len());
    }
    let (moving_psnr, src) = moving.unwrap();
    let mut f = src.clone();
    f.timestamp_us = 12 * 33_333;
    let refined = enc.encode_refine(&f).unwrap().unwrap();
    assert!(refined.keyframe);
    assert_eq!(enc.config().bitrate_kbps, 2_500, "bitrate restored");
    let refined_psnr = psnr(&src, &decode_one(dec.as_mut(), &refined.data));
    println!("default refine: {moving_psnr:.1} -> {refined_psnr:.1} dB");
    assert!(refined_psnr > moving_psnr + 2.0);
}

/// The numbers quoted in `docs/src/dev/performance.md`: 1080p60 at the default LAN bitrate,
/// 11 px text scrolled 37 px per frame (worst case for motion compensation), then stopped.
#[test]
fn report_1080p_default_bitrate() {
    let cfg = EncoderConfig::for_screen(1920, 1080, 60);
    let mut enc = H264Encoder::new(cfg).unwrap();
    let mut dec = new_decoder().unwrap();
    let mut last = None;
    let mut bytes = 0usize;
    for i in 0..30u32 {
        let src = text_scene(1920, 1080, 2, i * 37);
        let mut f = src.clone();
        f.timestamp_us = u64::from(i) * 16_667;
        let out = enc.encode(&f).unwrap().unwrap();
        bytes += out.data.len();
        let d = decode_one(dec.as_mut(), &out.data);
        last = Some((src, d));
    }
    let (src, moving) = last.unwrap();
    let mut f = src.clone();
    f.timestamp_us = 30 * 16_667;
    let refined = enc.encode_refine(&f).unwrap().unwrap();
    let r = decode_one(dec.as_mut(), &refined.data);
    println!(
        "1080p60 @ {} kbps, scrolling text: moving PSNR {:.1} dB SSIM {:.4} (avg {} KB/frame); \
         refined PSNR {:.1} dB SSIM {:.4} ({} KB once)",
        cfg.bitrate_kbps,
        psnr(&src, &moving),
        ssim(&src, &moving),
        bytes / 30 / 1024,
        psnr(&src, &r),
        ssim(&src, &r),
        refined.data.len() / 1024
    );
    assert!(psnr(&src, &r) > psnr(&src, &moving) + 3.0);
}
