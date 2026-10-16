//! Real sound-card tests. **Ignored**: they record what this PC is playing and play a tone
//! through its speakers. Run them only on purpose, on a machine where that is fine:
//!
//! ```text
//! cargo test -p tlp-desk-audio --test real_devices -- --ignored --nocapture --test-threads 1
//! ```
//!
//! * `real_loopback_capture_delivers_chunks`: start some audio (a video, music) first; the
//!   test records 2 s of the default output device and prints the format and peak level.
//! * `real_playback_plays_a_tone`: plays a quiet 440 Hz tone for 1 s on the default output
//!   device. You should hear it.

use std::time::{Duration, Instant};

use tlp_desk_audio::{
    AudioCapture, AudioPlayback, PcmChunk, PcmSink, PcmSource, SystemCapture, SystemPlayback,
};

#[test]
#[ignore = "records the real default output device; run manually"]
fn real_loopback_capture_delivers_chunks() {
    let (sink, mut rx) = PcmSink::channel();
    let handle = SystemCapture.start(sink).expect("loopback capture starts");
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut chunks: Vec<PcmChunk> = Vec::new();
    while Instant::now() < deadline {
        if let Ok(c) = rx.try_recv() {
            chunks.push(c);
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    drop(handle);
    let peak = chunks
        .iter()
        .flat_map(|c| c.samples.iter())
        .fold(0.0f32, |m, s| m.max(s.abs()));
    let first = chunks.first().expect("chunks arrive while audio plays");
    println!(
        "{} chunks, {} Hz, {} ch, peak {peak:.3}",
        chunks.len(),
        first.rate,
        first.channels
    );
}

struct Tone {
    n: u64,
}

impl PcmSource for Tone {
    fn fill(&mut self, out: &mut [f32], rate: u32, channels: u16) {
        for frame in out.chunks_mut(usize::from(channels)) {
            let t = self.n as f32 / rate as f32;
            let v = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.1;
            frame.fill(v);
            self.n += 1;
        }
    }
}

#[test]
#[ignore = "plays a tone on the real speakers; run manually"]
fn real_playback_plays_a_tone() {
    let handle = SystemPlayback
        .start(Box::new(Tone { n: 0 }))
        .expect("playback starts");
    std::thread::sleep(Duration::from_secs(1));
    drop(handle);
}
