//! Transfer throughput on loopback: a real host and viewer session in one process (QUIC +
//! TLS on 127.0.0.1), real files on disk.
//!
//! ```text
//! cargo run --release -p tlp-desk-files --example bench_transfer -- [big|small|all] [GiB] [streams] [dir]
//! ```
//!
//! * `big`: one file of `GiB` (default 2) uploaded, then downloaded; reports MB/s.
//! * `small`: 10 000 files of 4 KiB in 100 folders, uploaded; reports files/s.
//! * `dir`: work folder (default `C:\tlp-build\files-transfer-tmp\bench`); source files are
//!   kept between runs, destinations are deleted first.
//!
//! The numbers include planning, hashing on both sides, verification and the final renames.

#[path = "../tests/common/mod.rs"]
mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tlp_desk_files::{TransferConfig, TransferState};

const MIB: u64 = 1 << 20;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map_or("all", String::as_str).to_string();
    let gib: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let streams: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);
    let dir = PathBuf::from(
        args.get(3)
            .cloned()
            .unwrap_or_else(|| r"C:\tlp-build\files-transfer-tmp\bench".into()),
    );
    std::fs::create_dir_all(&dir).expect("work dir");
    let cfg = TransferConfig {
        streams,
        ..TransferConfig::default()
    };
    println!(
        "streams per transfer: {streams}, work dir: {}",
        dir.display()
    );
    if mode == "big" || mode == "all" {
        big(&dir, (gib * 1024.0) as u64 * MIB, &cfg).await;
    }
    if mode == "small" || mode == "all" {
        small(&dir, &cfg).await;
    }
    if mode == "raw" {
        raw(&dir, (gib * 1024.0) as u64 * MIB, streams).await;
    }
}

/// Baselines: the same session moving bytes without files, and the disk without a network.
async fn raw(dir: &Path, size: u64, streams: usize) {
    use tlp_desk_net::StreamKind;
    let p = common::pair().await;
    let per = size / streams as u64;
    let host = p.host_session.clone();
    let recv = tokio::spawn(async move {
        let mut tasks = Vec::new();
        for _ in 0..streams {
            let mut s = host
                .accept_side_stream(StreamKind::Clipboard)
                .await
                .unwrap();
            tasks.push(tokio::spawn(async move {
                let mut buf = vec![0u8; 2 << 20];
                let mut n = 0u64;
                while let Some(k) = s.recv.read(&mut buf).await.unwrap() {
                    n += k as u64;
                }
                n
            }));
        }
        let mut total = 0;
        for t in tasks {
            total += t.await.unwrap();
        }
        total
    });
    let t0 = Instant::now();
    let mut senders = Vec::new();
    for i in 0..streams {
        let v = p.viewer_session.clone();
        senders.push(tokio::spawn(async move {
            let mut s = v.open_uni(StreamKind::Clipboard, i as u64).await.unwrap();
            let buf = vec![7u8; 2 << 20];
            let mut left = per;
            while left > 0 {
                let k = left.min(buf.len() as u64) as usize;
                s.write_all(&buf[..k]).await.unwrap();
                left -= k as u64;
            }
            s.finish().unwrap();
            let _ = s.stopped().await;
        }));
    }
    for s in senders {
        s.await.unwrap();
    }
    let got = recv.await.unwrap();
    let secs = t0.elapsed().as_secs_f64();
    println!(
        "raw QUIC {streams} streams {:>6} MiB in {secs:>6.2} s = {:>7.1} MB/s",
        got / MIB,
        got as f64 / secs / 1e6
    );

    let path = dir.join("disk-test.bin");
    let buf = vec![3u8; 2 << 20];
    let t0 = Instant::now();
    {
        let mut f = std::fs::File::create(&path).unwrap();
        let mut off = 0u64;
        while off < size {
            f.write_all(&buf).unwrap();
            off += buf.len() as u64;
        }
    }
    let secs = t0.elapsed().as_secs_f64();
    println!(
        "disk write {:>6} MiB in {secs:>6.2} s = {:>7.1} MB/s",
        size / MIB,
        size as f64 / secs / 1e6
    );
    let t0 = Instant::now();
    let f = std::fs::File::open(&path).unwrap();
    let h = tlp_desk_files::transfer::hash_file_for_bench(&f, size);
    let secs = t0.elapsed().as_secs_f64();
    println!(
        "read+blake3 {:>6} MiB in {secs:>6.2} s = {:>7.1} MB/s ({})",
        size / MIB,
        size as f64 / secs / 1e6,
        h[0]
    );
    let _ = std::fs::remove_file(&path);
}

async fn big(dir: &Path, size: u64, cfg: &TransferConfig) {
    let src = dir.join("src-big");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("ملف كبير.bin");
    if std::fs::metadata(&file).map(|m| m.len()).ok() != Some(size) {
        println!("writing {} MiB source file...", size / MIB);
        let mut f =
            std::io::BufWriter::with_capacity(8 << 20, std::fs::File::create(&file).unwrap());
        let block = common::content(4 << 20, 7);
        let mut left = size;
        let mut n = 0u8;
        while left > 0 {
            let take = left.min(block.len() as u64) as usize;
            // Vary every block so nothing along the way can deduplicate it.
            let mut b = block[..take].to_vec();
            b[0] = n;
            n = n.wrapping_add(1);
            f.write_all(&b).unwrap();
            left -= take as u64;
        }
        f.flush().unwrap();
    }
    let up = fresh(dir, "dst-big-up");
    let p = common::pair_with(cfg.clone(), cfg.clone()).await;
    let mut ev = p.viewer_ft.subscribe();
    let t0 = Instant::now();
    let id = p
        .viewer_ft
        .start_upload(
            p.viewer_key,
            vec![file.clone()],
            up.to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let (_, state, _) = common::finished(&mut ev, Some(id)).await;
    let secs = t0.elapsed().as_secs_f64();
    assert_eq!(state, TransferState::Completed);
    println!(
        "upload   1 file  {:>6} MiB in {:>6.2} s = {:>7.1} MB/s",
        size / MIB,
        secs,
        size as f64 / secs / 1e6
    );

    let down = fresh(dir, "dst-big-down");
    let t0 = Instant::now();
    let id = p
        .viewer_ft
        .start_download(
            p.viewer_key,
            vec![file.to_str().unwrap().into()],
            down.clone(),
        )
        .await
        .unwrap();
    let (_, state, _) = common::finished(&mut ev, Some(id)).await;
    let secs = t0.elapsed().as_secs_f64();
    assert_eq!(state, TransferState::Completed);
    println!(
        "download 1 file  {:>6} MiB in {:>6.2} s = {:>7.1} MB/s",
        size / MIB,
        secs,
        size as f64 / secs / 1e6
    );
    let _ = std::fs::remove_dir_all(&up);
    let _ = std::fs::remove_dir_all(&down);
}

async fn small(dir: &Path, cfg: &TransferConfig) {
    let src = dir.join("src-small").join("ملفات صغيرة");
    let (dirs, per, len) = (100usize, 100usize, 4096usize);
    if std::fs::read_dir(&src).map(|r| r.count()).unwrap_or(0) != dirs {
        println!("writing {} small source files...", dirs * per);
        for d in 0..dirs {
            let sub = src.join(format!("مجلد {d:03}"));
            std::fs::create_dir_all(&sub).unwrap();
            for f in 0..per {
                std::fs::write(
                    sub.join(format!("ملف {f:03}.txt")),
                    common::content(len, (d * per + f) as u64),
                )
                .unwrap();
            }
        }
    }
    let dst = fresh(dir, "dst-small");
    let p = common::pair_with(cfg.clone(), cfg.clone()).await;
    let mut ev = p.viewer_ft.subscribe();
    let t0 = Instant::now();
    let id = p
        .viewer_ft
        .start_upload(
            p.viewer_key,
            vec![src.clone()],
            dst.to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let (_, state, failed) = common::finished(&mut ev, Some(id)).await;
    let secs = t0.elapsed().as_secs_f64();
    assert_eq!((state, failed), (TransferState::Completed, 0));
    let files = dirs * per;
    println!(
        "upload {files} files of {len} B in {secs:>6.2} s = {:>7.0} files/s ({:.1} MB/s)",
        files as f64 / secs,
        (files * len) as f64 / secs / 1e6
    );
    let _ = std::fs::remove_dir_all(&dst);
}

fn fresh(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}
