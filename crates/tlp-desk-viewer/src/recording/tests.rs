//! Recorder, container and player tests with real H.264 from the project's own encoder.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_capture::BgraFrame;
use tlp_desk_codec::EncoderConfig;

use super::demux::{demux, probe, Demuxed};
use super::mp4::VIDEO_TIMESCALE;
use super::nal::{nal_type, parse_sps, sample_to_annex_b, split_annex_b, NAL_SPS};
use super::recorder::{Core, SpaceFn};
use super::*;

/// Frame interval chosen so it is a whole number of 90 kHz ticks (3006), keeping the
/// expected times exact.
const FRAME_US: u64 = 33_400;
const FRAME_TICKS: u64 = 3_006;
const PLENTY: u64 = 1 << 40;

fn at(frames: u64) -> Duration {
    Duration::from_micros(frames * FRAME_US)
}

/// Encodes `n` moving-gradient frames of `w` x `h`; keyframes at frame 0 and every index in
/// `keyframes`. Sequence numbers and timestamps start at `first_seq`.
fn encode(w: u32, h: u32, n: usize, keyframes: &[usize], first_seq: u64) -> Vec<TapFrame> {
    let mut enc = tlp_desk_codec::new_encoder(EncoderConfig {
        width: w,
        height: h,
        fps: 30,
        bitrate_kbps: 800,
    })
    .unwrap();
    let mut out = Vec::new();
    for i in 0..n {
        let mut f = BgraFrame::new_tight(w, h);
        for (p, px) in f.data.chunks_exact_mut(4).enumerate() {
            let x = (p as u32 % w + i as u32 * 7) as u8;
            let y = (p as u32 / w) as u8;
            px.copy_from_slice(&[x, y, x ^ y, 255]);
        }
        let seq = first_seq + i as u64;
        f.timestamp_us = seq * FRAME_US;
        if keyframes.contains(&i) {
            enc.request_keyframe();
        }
        let ef = enc.encode(&f).unwrap().expect("encoder never skips");
        out.push(TapFrame {
            seq,
            timestamp_us: seq * FRAME_US,
            keyframe: ef.keyframe,
            data: ef.data,
        });
    }
    out
}

fn counter() -> (KeyframeFn, Arc<AtomicUsize>) {
    let n = Arc::new(AtomicUsize::new(0));
    let c = n.clone();
    (
        Arc::new(move || {
            c.fetch_add(1, Ordering::Relaxed);
        }),
        n,
    )
}

fn space(bytes: Arc<AtomicU64>) -> SpaceFn {
    Box::new(move |_| Ok(bytes.load(Ordering::Relaxed)))
}

fn core(folder: &Path, peer: &str) -> (Core, Arc<AtomicUsize>) {
    let (kf, n) = counter();
    let c = Core::new(
        folder,
        peer,
        1 << 20,
        Duration::from_secs(4),
        kf,
        space(Arc::new(AtomicU64::new(PLENTY))),
        Instant::now(),
    )
    .unwrap();
    (c, n)
}

fn feed(core: &mut Core, frames: Vec<TapFrame>) {
    let now = Instant::now();
    for f in frames {
        core.on_frame(f, now).unwrap();
    }
}

fn open(path: &Path) -> Demuxed {
    demux(&mut File::open(path).unwrap()).unwrap()
}

/// Decodes every sample of `d` from `path` and returns the picture sizes.
fn decode_all(path: &Path, d: &Demuxed) -> Vec<(u32, u32)> {
    let bytes = std::fs::read(path).unwrap();
    let mut dec = tlp_desk_codec::new_decoder().unwrap();
    let params: Vec<&[u8]> = d.sps.iter().chain(&d.pps).map(Vec::as_slice).collect();
    let mut out = Vec::new();
    let mut annex = Vec::new();
    for s in &d.samples {
        let data = &bytes[s.offset as usize..(s.offset + u64::from(s.size)) as usize];
        let p: &[&[u8]] = if s.sync { &params } else { &[] };
        sample_to_annex_b(data, p, &mut annex).unwrap();
        if let Some(f) = dec.decode(&annex).unwrap() {
            out.push((f.width, f.height));
        }
    }
    out
}

/// Polls `cond` (the player works on its own thread) with a generous deadline.
fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn encoder_sends_parameter_sets_with_every_keyframe() {
    // The recorder relies on this to start files and to notice picture size changes.
    let frames = encode(320, 240, 20, &[10], 0);
    for i in [0, 10] {
        assert!(frames[i].keyframe, "frame {i}");
        let nals = split_annex_b(&frames[i].data);
        let sps = nals
            .iter()
            .find(|n| nal_type(n) == Some(NAL_SPS))
            .expect("SPS");
        let info = parse_sps(sps).unwrap();
        assert_eq!((info.width, info.height), (320, 240));
    }
    assert!(!frames[5].keyframe);
}

#[test]
fn mux_demux_round_trip_with_real_h264() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, _) = core(dir.path(), "office");
    feed(&mut c, encode(320, 240, 45, &[15, 30], 0));
    let (files, result) = c.finish();
    result.unwrap();
    assert_eq!(files.len(), 1);
    let path = &files[0].path;
    let d = open(path);
    assert!(d.complete, "closed file ends with its index");
    assert_eq!((d.width, d.height), (320, 240));
    assert_eq!(d.timescale, VIDEO_TIMESCALE);
    assert_eq!(d.samples.len(), 45);
    let sync: Vec<usize> = (0..45).filter(|i| d.samples[*i].sync).collect();
    assert_eq!(sync, vec![0, 15, 30]);
    for (i, s) in d.samples.iter().enumerate() {
        assert_eq!(s.dts, i as u64 * FRAME_TICKS, "sample {i}");
        assert_eq!(s.duration as u64, FRAME_TICKS, "sample {i}");
    }
    assert!(d.fragments.len() >= 3, "written in fragments");
    // Every sample decodes back to a 320x240 picture.
    assert_eq!(decode_all(path, &d), vec![(320, 240); 45]);
    // The header reports the duration for the list.
    let p = probe(&mut File::open(path).unwrap()).unwrap();
    assert!(p.complete);
    assert_eq!(p.duration, Some(Duration::from_millis(1503)));
    assert_eq!(files[0].duration, at(45));
}

#[test]
fn recording_waits_for_a_keyframe_and_asks_for_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, requests) = core(dir.path(), "peer");
    let mut frames = encode(320, 240, 30, &[20], 0);
    frames.remove(0); // joined mid-stream: no keyframe until frame 20
    feed(&mut c, frames);
    assert!(
        requests.load(Ordering::Relaxed) >= 1,
        "a keyframe was requested"
    );
    let (files, result) = c.finish();
    result.unwrap();
    let d = open(&files[0].path);
    assert_eq!(d.samples.len(), 10, "frames 20..30");
    assert!(d.samples[0].sync);
    assert_eq!(d.samples[0].dts, 0, "the file starts at its first keyframe");
}

#[test]
fn no_keyframe_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, _) = core(dir.path(), "peer");
    let mut frames = encode(160, 120, 5, &[], 0);
    frames.remove(0);
    feed(&mut c, frames);
    let (files, result) = c.finish();
    result.unwrap();
    assert!(files.is_empty());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn resolution_change_continues_in_a_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, _) = core(dir.path(), "مكتب");
    let mut frames = encode(320, 240, 10, &[], 0);
    // The host switched to a bigger monitor: a new encoder, the sequence continues.
    frames.extend(encode(640, 360, 12, &[], 10));
    feed(&mut c, frames);
    let (files, result) = c.finish();
    result.unwrap();
    assert_eq!(files.len(), 2);
    let names: Vec<String> = files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    assert!(names[0].starts_with("TLP-Desk مكتب ") && names[0].ends_with(".mp4"));
    assert_eq!(
        names[1],
        names[0].replace(".mp4", " (2).mp4"),
        "numbered part"
    );
    let first = open(&files[0].path);
    let second = open(&files[1].path);
    assert_eq!(
        (first.width, first.height, first.samples.len()),
        (320, 240, 10)
    );
    assert_eq!(
        (second.width, second.height, second.samples.len()),
        (640, 360, 12)
    );
    assert_eq!(second.samples[0].dts, 0, "each file starts at time zero");
    assert_eq!(decode_all(&files[0].path, &first), vec![(320, 240); 10]);
    assert_eq!(decode_all(&files[1].path, &second), vec![(640, 360); 12]);
}

#[test]
fn a_gap_in_the_stream_skips_to_the_next_keyframe() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, requests) = core(dir.path(), "peer");
    let mut frames = encode(320, 240, 20, &[12], 0);
    frames.remove(5); // lost on the network
    feed(&mut c, frames);
    assert!(requests.load(Ordering::Relaxed) >= 1);
    let (files, result) = c.finish();
    result.unwrap();
    let d = open(&files[0].path);
    // 0..5 recorded, 6..12 skipped (their references are gone), 12..20 recorded.
    assert_eq!(d.samples.len(), 5 + 8);
    assert!(d.samples[5].sync);
    // The skipped time stays in the timeline.
    assert_eq!(d.samples[5].dts, 12 * FRAME_TICKS);
    assert_eq!(decode_all(&files[0].path, &d).len(), 13);
}

#[test]
fn garbage_frames_never_panic_or_create_files() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, _) = core(dir.path(), "peer");
    let now = Instant::now();
    for seed in 0u64..300 {
        let mut data: Vec<u8> = (0..(seed % 97) as usize)
            .map(|i| (seed.wrapping_mul(31).wrapping_add(i as u64 * 7) % 251) as u8)
            .collect();
        if seed % 3 == 0 {
            // Start codes with SPS/IDR headers and junk behind them.
            data.extend_from_slice(&[0, 0, 1, 0x67, 0xFF, 0xFF, 0, 0, 1, 0x65, 1]);
        }
        let f = TapFrame {
            seq: seed,
            timestamp_us: seed * 1000,
            keyframe: seed % 2 == 0,
            data,
        };
        let _ = c.on_frame(f, now);
    }
    let (files, _) = c.finish();
    assert!(files.is_empty());
}

#[test]
fn interrupted_recording_plays_and_is_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, _) = core(dir.path(), "peer");
    // 3.3 s of video in one-second fragments (a keyframe starts each).
    feed(&mut c, encode(320, 240, 100, &[30, 60, 90], 0));
    let path = c.files()[0].clone();
    drop(c); // the app crashed: no index, no duration, the pending fragment never written
    let mut f = OpenOptions::new().append(true).open(&path).unwrap();
    // ... and the next fragment was cut off in the middle of its write.
    f.write_all(&[0, 0, 0x40, 0, b'm', b'o', b'o', b'f', 1, 2, 3])
        .unwrap();
    drop(f);

    let d = open(&path);
    assert!(!d.complete);
    assert_eq!(d.samples.len(), 90, "complete fragments are readable");
    assert!(d.valid_end < d.file_len);
    assert_eq!(
        probe(&mut File::open(&path).unwrap()).unwrap().duration,
        None
    );
    // An interrupted file already plays.
    let player = Player::open(&path, crate::PixelFormat::Rgba, None).unwrap();
    assert_eq!(player.status().duration, at(90));
    drop(player);

    assert_eq!(
        recover_file(&path).unwrap(),
        Recovery::Repaired {
            kept: at(90),
            removed_bytes: 11
        }
    );
    let repaired = open(&path);
    assert!(repaired.complete);
    assert_eq!(repaired.samples.len(), 90);
    let p = probe(&mut File::open(&path).unwrap()).unwrap();
    assert!(p.complete);
    assert_eq!(p.duration, Some(Duration::from_millis(3006)));
    assert_eq!(recover_file(&path).unwrap(), Recovery::AlreadyComplete);
    assert_eq!(decode_all(&path, &repaired).len(), 90);
}

#[test]
fn hostile_files_are_errors_not_panics() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, _) = core(dir.path(), "peer");
    feed(&mut c, encode(160, 120, 20, &[10], 0));
    let (files, result) = c.finish();
    result.unwrap();
    let good = std::fs::read(&files[0].path).unwrap();
    let scratch = dir.path().join("scratch.mp4");
    // Truncations, and single-byte corruption at a few hundred offsets (header bytes densely,
    // the rest sparsely).
    let step = (good.len() / 150).max(1);
    let cuts = (0..400.min(good.len())).chain((400..good.len()).step_by(step));
    for cut in cuts.clone() {
        let _ = demux(&mut std::io::Cursor::new(&good[..cut]));
        let _ = probe(&mut std::io::Cursor::new(&good[..cut]));
    }
    for pos in cuts.filter(|p| *p < good.len()) {
        let mut bad = good.clone();
        bad[pos] ^= 0xA5;
        if let Ok(d) = demux(&mut std::io::Cursor::new(&bad)) {
            assert!(d
                .samples
                .iter()
                .all(|s| s.offset + u64::from(s.size) <= d.file_len));
        }
    }
    std::fs::write(&scratch, b"not a recording at all").unwrap();
    assert!(Player::open(&scratch, crate::PixelFormat::Rgba, None).is_err());
    assert!(recover_file(&scratch).is_err());
}

#[test]
fn disk_space_is_checked_before_and_while_recording() {
    let dir = tempfile::tempdir().unwrap();
    let (kf, _) = counter();
    let free = Arc::new(AtomicU64::new(1000));
    let err = Core::new(
        dir.path(),
        "peer",
        1 << 20,
        Duration::from_secs(4),
        kf.clone(),
        space(free.clone()),
        Instant::now(),
    )
    .err()
    .unwrap();
    assert!(matches!(
        err,
        ViewerError::DiskFull {
            available: 1000,
            needed: 1_048_576
        }
    ));

    free.store(PLENTY, Ordering::Relaxed);
    let start = Instant::now();
    let mut c = Core::new(
        dir.path(),
        "peer",
        1 << 20,
        Duration::from_secs(4),
        kf,
        space(free.clone()),
        start,
    )
    .unwrap();
    for f in encode(160, 120, 5, &[], 0) {
        c.on_frame(f, start).unwrap();
    }
    free.store(1000, Ordering::Relaxed);
    let later = start + Duration::from_secs(11);
    assert!(matches!(
        c.on_tick(later),
        Err(ViewerError::DiskFull { .. })
    ));
    // What was recorded is still closed properly.
    let (files, result) = c.finish();
    result.unwrap();
    assert!(open(&files[0].path).complete);
}

#[test]
fn keyframes_are_requested_regularly() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, requests) = core(dir.path(), "peer");
    let start = Instant::now();
    for f in encode(160, 120, 3, &[], 0) {
        c.on_frame(f, start).unwrap();
    }
    let before = requests.load(Ordering::Relaxed);
    c.on_tick(start + Duration::from_secs(5)).unwrap();
    assert_eq!(requests.load(Ordering::Relaxed), before + 1);
    let _ = c.finish();
}

#[test]
fn arabic_file_names_round_trip_through_the_list() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, _) = core(dir.path(), "حاسوب المكتب ٣");
    feed(&mut c, encode(160, 120, 10, &[], 0));
    let (files, result) = c.finish();
    result.unwrap();
    let list = list_recordings(dir.path()).unwrap();
    assert_eq!(list.len(), 1);
    let e = &list[0];
    assert!(
        e.file_name.starts_with("TLP-Desk حاسوب المكتب ٣ "),
        "{}",
        e.file_name
    );
    assert_eq!(e.path, files[0].path);
    assert!(e.complete);
    assert_eq!(e.duration, Some(Duration::from_millis(334)));
    assert!(e.bytes > 0);
    // The exact name opens and deletes.
    assert!(File::open(dir.path().join(&e.file_name)).is_ok());
    delete_recording(dir.path(), &e.path).unwrap();
    assert!(list_recordings(dir.path()).unwrap().is_empty());
}

fn record_file(dir: &Path, n: usize, keyframes: &[usize]) -> PathBuf {
    let (mut c, _) = core(dir, "player");
    feed(&mut c, encode(320, 240, n, keyframes, 0));
    let (files, result) = c.finish();
    result.unwrap();
    files[0].path.clone()
}

#[test]
fn player_seeks_from_the_nearest_keyframe() {
    let dir = tempfile::tempdir().unwrap();
    let path = record_file(dir.path(), 45, &[15, 30]);
    let player = Player::open(&path, crate::PixelFormat::Rgba, None).unwrap();
    assert_eq!(player.keyframes(), &[at(0), at(15), at(30)]);
    // Opens paused on the first frame.
    wait_for("first frame", || player.take_frame().is_some());
    let st = player.status();
    assert!(!st.playing);
    assert_eq!((st.width, st.height), (320, 240));
    assert_eq!(st.duration, at(45));

    // 1.2 s shows frame 35 (1.169 s), decoded starting from the keyframe at frame 30.
    player.seek(Duration::from_millis(1200));
    let mut got = None;
    wait_for("frame after seeking", || {
        got = player.take_frame();
        got.is_some()
    });
    let f = got.unwrap();
    assert_eq!(f.seq, 35);
    assert_eq!((f.width, f.height), (320, 240));
    assert_eq!(f.pixels.len(), 320 * 240 * 4);
    assert_eq!(player.status().position, at(35));
    assert!(player.status().error.is_none());
}

#[test]
fn player_lands_on_the_last_of_many_quick_seeks() {
    let dir = tempfile::tempdir().unwrap();
    let path = record_file(dir.path(), 45, &[15, 30]);
    let player = Player::open(&path, crate::PixelFormat::Rgba, None).unwrap();
    // A timeline drag: many positions in a row, ending at frame 20.
    for ms in (0..1400).step_by(25) {
        player.seek(Duration::from_millis(ms));
    }
    player.seek(at(20));
    wait_for("last seek", || player.status().position == at(20));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(player.status().position, at(20), "no older seek wins");
    assert!(!player.status().playing);
}

#[test]
fn player_plays_to_the_end_at_high_speed() {
    let dir = tempfile::tempdir().unwrap();
    let path = record_file(dir.path(), 30, &[]);
    let player = Player::open(&path, crate::PixelFormat::Bgra, None).unwrap();
    player.set_speed(100.0); // clamped to 4x
    player.play();
    wait_for("end of playback", || player.status().ended);
    let st = player.status();
    assert_eq!(st.speed, MAX_SPEED);
    assert!(!st.playing);
    assert_eq!(st.position, at(29));
    assert!(st.error.is_none());
}

#[test]
fn recording_thread_records_what_the_tap_sees() {
    let dir = tempfile::tempdir().unwrap();
    let (kf, _) = counter();
    let stopped = Arc::new(AtomicUsize::new(0));
    let s = stopped.clone();
    let mut cfg = RecordingConfig::new(dir.path().to_owned(), "مكتب");
    cfg.on_stopped = Some(Arc::new(move |_| {
        s.fetch_add(1, Ordering::Relaxed);
    }));
    let (rec, tap) = Recording::start_with(cfg, kf).unwrap();
    for f in encode(320, 240, 20, &[], 0) {
        tap(&tlp_desk_proto::VideoFrame {
            seq: f.seq,
            timestamp_us: f.timestamp_us,
            width: 320,
            height: 240,
            codec: tlp_desk_proto::Codec::H264,
            keyframe: f.keyframe,
            data: f.data,
        });
    }
    wait_for("frames written", || rec.status().files.len() == 1);
    let summary = rec.stop();
    assert!(summary.failure.is_none());
    assert_eq!(summary.files.len(), 1);
    assert_eq!(summary.duration, at(20));
    assert_eq!(open(&summary.files[0]).samples.len(), 20);
    assert_eq!(
        stopped.load(Ordering::Relaxed),
        0,
        "not called for a requested stop"
    );
}
