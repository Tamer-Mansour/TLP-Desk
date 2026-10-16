//! Opus encode -> decode of a synthetic tone keeps the signal (SNR check).

use tlp_desk_audio::format::{FRAME_LEN, FRAME_SAMPLES, SAMPLE_RATE};
use tlp_desk_audio::{OpusDecoder, OpusEncoder, DEFAULT_BITRATE};

/// Two tones, different per channel, so a swapped or mixed channel would fail too.
fn tone(frames: usize) -> Vec<f32> {
    let mut v = Vec::with_capacity(frames * FRAME_LEN);
    for n in 0..frames * FRAME_SAMPLES {
        let t = n as f32 / SAMPLE_RATE as f32;
        v.push((2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.4);
        v.push((2.0 * std::f32::consts::PI * 1000.0 * t).sin() * 0.3);
    }
    v
}

/// SNR in dB of `decoded` against `original`, `delay` samples per channel later.
fn snr_db(original: &[f32], decoded: &[f32], delay: usize, skip: usize) -> f64 {
    let (mut sig, mut noise) = (0.0f64, 0.0f64);
    let frames = original.len() / 2;
    for i in skip..frames.saturating_sub(delay) {
        for c in 0..2 {
            let a = f64::from(original[i * 2 + c]);
            let b = f64::from(decoded[(i + delay) * 2 + c]);
            sig += a * a;
            noise += (a - b) * (a - b);
        }
    }
    10.0 * (sig / noise.max(1e-12)).log10()
}

#[test]
fn tone_survives_an_opus_round_trip() {
    let frames = 100; // one second
    let input = tone(frames);
    let mut enc = OpusEncoder::new(DEFAULT_BITRATE).unwrap();
    let mut dec = OpusDecoder::new().unwrap();
    let mut decoded = Vec::with_capacity(input.len());
    let mut bytes = 0usize;
    for frame in input.chunks_exact(FRAME_LEN) {
        let pkt = enc.encode(frame).unwrap();
        assert!(!pkt.is_empty() && pkt.len() <= 1275, "{} bytes", pkt.len());
        bytes += pkt.len();
        let mut out = [0.0f32; FRAME_LEN];
        dec.decode(&pkt, &mut out).unwrap();
        decoded.extend_from_slice(&out);
    }
    // Find the codec delay (look-ahead) instead of hard-coding libopus internals.
    let best = (0..1000)
        .map(|d| (d, snr_db(&input, &decoded, d, 4800)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    eprintln!(
        "opus round trip: SNR {:.1} dB, delay {} samples, {bytes} bytes/s",
        best.1, best.0
    );
    assert!(best.1 > 20.0, "SNR {:.1} dB at delay {}", best.1, best.0);
    assert_eq!(
        best.0,
        enc.lookahead(),
        "delay matches the reported look-ahead"
    );
    // About 128 kbit/s for one second of audio.
    assert!((10_000..=24_000).contains(&bytes), "{bytes} bytes");
}

#[test]
fn concealment_fades_instead_of_clicking() {
    let input = tone(20);
    let mut enc = OpusEncoder::new(DEFAULT_BITRATE).unwrap();
    let mut dec = OpusDecoder::new().unwrap();
    let mut out = [0.0f32; FRAME_LEN];
    for frame in input.chunks_exact(FRAME_LEN) {
        let pkt = enc.encode(frame).unwrap();
        dec.decode(&pkt, &mut out).unwrap();
    }
    let peak = |s: &[f32]| s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    // Several lost packets in a row: output keeps going, then decays towards silence.
    let mut peaks = Vec::new();
    for _ in 0..10 {
        dec.conceal(&mut out).unwrap();
        assert!(out.iter().all(|s| s.is_finite()));
        peaks.push(peak(&out));
    }
    assert!(
        peaks[0] > 0.05,
        "first concealed frame continues the tone: {peaks:?}"
    );
    assert!(peaks[9] < peaks[0], "concealment fades: {peaks:?}");
}
