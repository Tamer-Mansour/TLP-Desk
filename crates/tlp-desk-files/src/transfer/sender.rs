//! Sending side: reads files and pushes them on parallel `FileData` streams.
//!
//! Every file is cut into [`PIECE`]-sized pieces; a pool of workers pulls pieces from one
//! queue (in item order) and sends each on its own uni stream: `FileDataHeader` + raw bytes.
//! Small files are one piece each, so many of them are in flight at once; a large file is
//! spread over all workers. There is no per-piece round trip: QUIC flow control alone limits
//! how much is in flight. Each worker overlaps the next disk read with the current network
//! write, and hashes the piece while reading, off the async threads.

use std::collections::VecDeque;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tlp_desk_net::{SendStream, StreamKind};
use tlp_desk_proto::{FileDataHeader, FsError, Message, TransferItem};
use tokio::sync::oneshot;
use tokio::task::JoinSet;

use super::disk::{open_read, read_exact_at, IO_BUF};
use super::error::{io_to_fs_error, FilesError};
use super::hash::{
    combine, hash_pieces_from_disk, piece_count, piece_len, Digest, PieceHasher, PIECE,
};
use super::path::{join_under, validate_rel_path};
use super::scan::checked_abs;
use super::session::SessionState;
use super::state::{lock, Transfer};
use super::types::{TransferEvent, TransferState};

/// Answer to our offer: resume points, or why it was refused.
pub type AcceptResult = Result<Vec<(u32, u64)>, FsError>;

/// Sender half of a transfer.
pub struct Sending {
    /// Offered items.
    pub items: Vec<TransferItem>,
    /// Local path of each file item.
    paths: Vec<Option<PathBuf>>,
    accept: Mutex<Option<oneshot::Sender<AcceptResult>>>,
    item_failed: Vec<AtomicBool>,
    all_sent: AtomicBool,
    peer_complete: AtomicBool,
}

impl Sending {
    /// `accept` receives the peer's answer (viewer); the host passes `None`.
    pub fn new(
        items: Vec<TransferItem>,
        paths: Vec<Option<PathBuf>>,
        accept: Option<oneshot::Sender<AcceptResult>>,
    ) -> Self {
        let item_failed = items.iter().map(|_| AtomicBool::new(false)).collect();
        Self {
            items,
            paths,
            accept: Mutex::new(accept),
            item_failed,
            all_sent: AtomicBool::new(false),
            peer_complete: AtomicBool::new(false),
        }
    }

    /// The peer accepted or rejected our offer.
    pub fn accepted(&self, r: AcceptResult) {
        if let Some(tx) = lock(&self.accept).take() {
            let _ = tx.send(r);
        }
    }

    /// The receiver reported progress; `bytes_done == total` means every file is verified.
    pub fn peer_progress(&self, state: &SessionState, t: &Arc<Transfer>, bytes_done: u64) {
        if bytes_done >= t.info.total_bytes {
            self.peer_complete.store(true, Ordering::Release);
            self.maybe_complete(state, t);
        }
    }

    /// The receiver could not write or verify one item.
    pub fn on_receiver_item_error(
        &self,
        state: &SessionState,
        t: &Arc<Transfer>,
        item: u32,
        error: FsError,
    ) {
        let Some(it) = self.items.get(item as usize) else {
            return;
        };
        if let Some(f) = self.item_failed.get(item as usize) {
            if f.swap(true, Ordering::AcqRel) {
                return;
            }
        }
        t.failed.fetch_add(1, Ordering::Relaxed);
        state.emit(TransferEvent::ItemFailed {
            id: t.id,
            rel_path: it.rel_path.clone(),
            error,
        });
    }

    fn maybe_complete(&self, state: &SessionState, t: &Arc<Transfer>) {
        if self.all_sent.load(Ordering::Acquire) && self.peer_complete.load(Ordering::Acquire) {
            state.end(t, TransferState::Completed);
        }
    }

    fn failed(&self, item: u32) -> bool {
        self.item_failed
            .get(item as usize)
            .is_none_or(|f| f.load(Ordering::Acquire))
    }
}

/// Host side of a download: maps offered items to local files under `source_dir`.
///
/// # Errors
/// An invalid source folder or item path, or an item that does not exist as offered.
pub fn resolve_sources(
    source_dir: &str,
    items: &[TransferItem],
) -> Result<Vec<Option<PathBuf>>, FilesError> {
    let src = checked_abs(source_dir).map_err(FilesError::Remote)?;
    let src = std::fs::canonicalize(&src).map_err(|e| FilesError::io(&src, e))?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let comps = validate_rel_path(&item.rel_path).map_err(|issue| FilesError::InvalidPath {
            path: item.rel_path.clone(),
            issue,
        })?;
        let path = join_under(&src, &comps);
        let meta = std::fs::metadata(&path).map_err(|e| FilesError::io(&path, e))?;
        if meta.is_dir() != item.is_dir {
            return Err(FilesError::Remote(FsError::NotFound));
        }
        out.push((!item.is_dir).then_some(path));
    }
    Ok(out)
}

/// Starts sending with the receiver's resume points.
pub fn spawn(state: Arc<SessionState>, t: Arc<Transfer>, s: Arc<Sending>, resume: Vec<(u32, u64)>) {
    tokio::spawn(run(state, t, s, resume));
}

/// Start offset of every item, from the peer's resume points (piece-aligned or complete).
fn start_offsets(items: &[TransferItem], resume: &[(u32, u64)]) -> Result<Vec<u64>, FilesError> {
    let mut start = vec![0u64; items.len()];
    for &(i, off) in resume {
        let bad = || FilesError::Protocol(format!("bad resume point ({i}, {off})"));
        let item = items.get(i as usize).ok_or_else(bad)?;
        if item.is_dir || off > item.size || (off % PIECE != 0 && off != item.size) {
            return Err(bad());
        }
        if let Some(s) = start.get_mut(i as usize) {
            *s = off;
        }
    }
    Ok(start)
}

enum Job {
    /// Send piece `index` of `item`.
    Piece { item: u32, index: u64 },
    /// Nothing to send (empty, already present): only hash and report.
    Whole { item: u32 },
}

struct ItemTx {
    /// Pieces still to send.
    remaining: AtomicU64,
    /// First piece we send (pieces before it are already at the receiver).
    first: u64,
    digests: Mutex<Vec<Option<Digest>>>,
    file: Mutex<Option<Arc<File>>>,
}

struct Run {
    state: Arc<SessionState>,
    t: Arc<Transfer>,
    s: Arc<Sending>,
    items: Vec<ItemTx>,
    queue: Mutex<VecDeque<Job>>,
}

enum PieceError {
    Disk(FsError),
    Net,
    Stopped,
}

async fn run(state: Arc<SessionState>, t: Arc<Transfer>, s: Arc<Sending>, resume: Vec<(u32, u64)>) {
    let start = match start_offsets(&s.items, &resume) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%e, "peer sent invalid resume points");
            state.abort(&t, TransferState::Failed(e.to_fs_error()));
            return;
        }
    };
    let resumed = start.iter().sum();
    t.bytes_done.store(resumed, Ordering::Relaxed);
    t.resumed.store(resumed, Ordering::Relaxed);
    let mut queue = VecDeque::new();
    let mut items = Vec::with_capacity(s.items.len());
    for (i, item) in s.items.iter().enumerate() {
        let n = piece_count(item.size, PIECE);
        let first = start.get(i).copied().unwrap_or(0) / PIECE;
        let first = if start.get(i) == Some(&item.size) {
            n
        } else {
            first
        };
        let idx = u32::try_from(i).unwrap_or(u32::MAX);
        if !item.is_dir {
            if first >= n {
                queue.push_back(Job::Whole { item: idx });
            } else {
                queue.extend((first..n).map(|index| Job::Piece { item: idx, index }));
            }
        }
        items.push(ItemTx {
            remaining: AtomicU64::new(n.saturating_sub(first)),
            first,
            digests: Mutex::new(vec![None; usize::try_from(n).unwrap_or(0)]),
            file: Mutex::new(None),
        });
    }
    let workers = worker_count(&state, &s, &queue);
    tracing::info!(transfer = t.id.0, jobs = queue.len(), workers, "sending");
    let run = Arc::new(Run {
        state: state.clone(),
        t: t.clone(),
        s: s.clone(),
        items,
        queue: Mutex::new(queue),
    });
    let mut set = JoinSet::new();
    for _ in 0..workers {
        set.spawn(worker(run.clone()));
    }
    let mut net_error = false;
    while let Some(r) = set.join_next().await {
        net_error |= !matches!(r, Ok(true));
    }
    if t.stopped() {
        return;
    }
    if net_error {
        // Usually a cancel or error message from the peer is about to arrive; give it a moment
        // so the user sees the real reason.
        tokio::time::sleep(Duration::from_secs(1)).await;
        state.abort(
            &t,
            TransferState::Failed(FsError::Other("connection problem".into())),
        );
        return;
    }
    s.all_sent.store(true, Ordering::Release);
    s.maybe_complete(&state, &t);
}

/// Large files: `streams` workers. Many small files: twice as many, since each small file is
/// a short stream and the receiver spends most of its time creating and renaming files.
fn worker_count(state: &SessionState, s: &Sending, queue: &VecDeque<Job>) -> usize {
    let streams = state.inner.cfg.streams.clamp(1, 32);
    let files = s.items.iter().filter(|i| !i.is_dir).count().max(1);
    let bytes: u64 = s.items.iter().map(|i| i.size).sum();
    let small = files > 2 * streams && bytes / files as u64 <= 4 << 20;
    let n = if small {
        (streams * 2).min(32)
    } else {
        streams
    };
    n.min(queue.len().max(1))
}

/// Pulls jobs until the queue is empty. `false` on a network error.
async fn worker(run: Arc<Run>) -> bool {
    loop {
        if run.t.stopped() {
            return true;
        }
        let Some(job) = lock(&run.queue).pop_front() else {
            return true;
        };
        match job {
            Job::Whole { item } => finish_item(&run, item).await,
            Job::Piece { item, index } => {
                if run.s.failed(item) {
                    continue;
                }
                match send_piece(&run, item, index).await {
                    Ok(d) => {
                        let Some(it) = run.items.get(item as usize) else {
                            continue;
                        };
                        if let Some(slot) =
                            lock(&it.digests).get_mut(usize::try_from(index).unwrap_or(usize::MAX))
                        {
                            *slot = Some(d);
                        }
                        if it.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
                            finish_item(&run, item).await;
                        }
                    }
                    Err(PieceError::Disk(e)) => item_failed(&run, item, e),
                    Err(PieceError::Stopped) => return true,
                    Err(PieceError::Net) => return run.t.stopped(),
                }
            }
        }
    }
}

/// The shared read handle of an item, opened on first use; checks the file still has the
/// offered size.
async fn item_file(run: &Run, item: u32) -> Result<Arc<File>, PieceError> {
    let it = run.items.get(item as usize).ok_or(PieceError::Stopped)?;
    if let Some(f) = lock(&it.file).clone() {
        return Ok(f);
    }
    let path = run
        .s
        .paths
        .get(item as usize)
        .cloned()
        .flatten()
        .ok_or(PieceError::Disk(FsError::NotFound))?;
    let size = run.s.items.get(item as usize).map_or(0, |i| i.size);
    let file = tokio::task::spawn_blocking(move || -> Result<File, FsError> {
        let f = open_read(&path).map_err(|e| io_to_fs_error(&e))?;
        let len = f.metadata().map_err(|e| io_to_fs_error(&e))?.len();
        if len != size {
            return Err(FsError::Other(
                "the file changed after it was listed".into(),
            ));
        }
        Ok(f)
    })
    .await
    .map_err(|_| PieceError::Disk(FsError::Other("read task failed".into())))?
    .map_err(PieceError::Disk)?;
    let mut slot = lock(&it.file);
    Ok(slot.get_or_insert_with(|| Arc::new(file)).clone())
}

async fn send_piece(run: &Run, item: u32, index: u64) -> Result<Digest, PieceError> {
    let file = item_file(run, item).await?;
    let mut stream = run
        .state
        .ctx
        .session
        .open_uni(StreamKind::FileData, run.t.wire)
        .await
        .map_err(|_| PieceError::Net)?;
    let res = tokio::select! {
        r = pump(run, &mut stream, file, item, index) => r,
        () = run.t.wait_stop() => Err(PieceError::Stopped),
    };
    match res {
        Ok(_) => {
            let _ = stream.finish();
        }
        Err(_) => {
            let _ = stream.reset(0u32.into());
        }
    }
    res
}

type ReadResult = std::io::Result<(Vec<u8>, usize, PieceHasher)>;

fn spawn_read(
    file: Arc<File>,
    mut buf: Vec<u8>,
    offset: u64,
    n: usize,
    mut hasher: PieceHasher,
) -> tokio::task::JoinHandle<ReadResult> {
    tokio::task::spawn_blocking(move || {
        if buf.len() < n {
            buf.resize(n, 0);
        }
        let slice = buf.get_mut(..n).unwrap_or_default();
        read_exact_at(&file, slice, offset)?;
        hasher.update(slice);
        Ok((buf, n, hasher))
    })
}

/// Writes one piece: header, then the bytes, reading the next buffer while writing this one.
async fn pump(
    run: &Run,
    stream: &mut SendStream,
    file: Arc<File>,
    item: u32,
    index: u64,
) -> Result<Digest, PieceError> {
    let size = run.s.items.get(item as usize).map_or(0, |i| i.size);
    let offset = index.saturating_mul(PIECE);
    let len = piece_len(size, PIECE, index);
    stream
        .write_all(&FileDataHeader { item, offset }.encode())
        .await
        .map_err(|_| PieceError::Net)?;
    let chunk = |remaining: u64| usize::try_from(remaining.min(IO_BUF as u64)).unwrap_or(IO_BUF);
    let mut pending = Some(spawn_read(
        file.clone(),
        vec![0u8; IO_BUF],
        offset,
        chunk(len),
        PieceHasher::new(offset, size, PIECE),
    ));
    let mut spare: Option<Vec<u8>> = None;
    let mut done = 0u64;
    let mut last_hasher = None;
    while let Some(read) = pending.take() {
        let (mut buf, n, hasher) = read
            .await
            .map_err(|_| PieceError::Disk(FsError::Other("read task failed".into())))?
            .map_err(|e| PieceError::Disk(io_to_fs_error(&e)))?;
        done += n as u64;
        if done < len {
            let next = spare.take().unwrap_or_else(|| vec![0u8; IO_BUF]);
            pending = Some(spawn_read(
                file.clone(),
                next,
                offset + done,
                chunk(len - done),
                hasher,
            ));
        } else {
            last_hasher = Some(hasher);
        }
        if run
            .state
            .inner
            .hooks
            .corrupt_next
            .swap(false, Ordering::AcqRel)
        {
            // Test hook: flip a byte after it was hashed, as a damaged link or disk would.
            if let Some(b) = buf.first_mut() {
                *b ^= 0xFF;
            }
        }
        let data = buf.get(..n).unwrap_or_default();
        stream.write_all(data).await.map_err(|_| PieceError::Net)?;
        run.t.bytes_done.fetch_add(n as u64, Ordering::Relaxed);
        throttle(run, n).await;
        spare = Some(buf);
    }
    last_hasher
        .map(|h| h.finish())
        .ok_or(PieceError::Disk(FsError::Other("empty piece".into())))
}

/// Test hook: limits the send rate so tests can act mid-transfer deterministically.
async fn throttle(run: &Run, n: usize) {
    let rate = run.state.inner.hooks.rate_limit.load(Ordering::Relaxed);
    if rate > 0 {
        tokio::time::sleep(Duration::from_secs_f64(n as f64 / rate as f64)).await;
    }
}

/// All pieces of `item` are sent: hash the part the receiver already had, merge, report.
async fn finish_item(run: &Run, item: u32) {
    if run.s.failed(item) || run.t.stopped() {
        return;
    }
    let Some(it) = run.items.get(item as usize) else {
        return;
    };
    let size = run.s.items.get(item as usize).map_or(0, |i| i.size);
    let hash = if size == 0 {
        Ok(*blake3::hash(&[]).as_bytes())
    } else {
        let file = match item_file(run, item).await {
            Ok(f) => f,
            Err(PieceError::Disk(e)) => return item_failed(run, item, e),
            Err(_) => return,
        };
        let first = it.first;
        let mut digests = lock(&it.digests).clone();
        tokio::task::spawn_blocking(move || -> Result<Digest, FsError> {
            if first > 0 {
                let prefix = hash_pieces_from_disk(&file, size, PIECE, 0, first)
                    .map_err(|e| io_to_fs_error(&e))?;
                for (slot, d) in digests.iter_mut().zip(prefix) {
                    *slot = Some(d);
                }
            }
            combine(size, PIECE, &digests).ok_or(FsError::Other("piece hash missing".into()))
        })
        .await
        .unwrap_or_else(|_| Err(FsError::Other("hash task failed".into())))
    };
    lock(&it.file).take();
    match hash {
        Ok(blake3) => {
            run.t.files_done.fetch_add(1, Ordering::Relaxed);
            run.state.send(Message::TransferDone {
                id: run.t.wire,
                item,
                blake3,
            });
        }
        Err(e) => item_failed(run, item, e),
    }
}

/// A local read failed: tell the receiver, the UI, and skip the rest of the item.
fn item_failed(run: &Run, item: u32, error: FsError) {
    if run.t.stopped() {
        return;
    }
    let Some(f) = run.s.item_failed.get(item as usize) else {
        return;
    };
    if f.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Some(it) = run.items.get(item as usize) {
        lock(&it.file).take();
    }
    tracing::warn!(
        transfer = run.t.id.0,
        item,
        ?error,
        "file could not be sent"
    );
    run.t.failed.fetch_add(1, Ordering::Relaxed);
    run.state.send(Message::TransferError {
        id: run.t.wire,
        item: Some(item),
        error: error.clone(),
    });
    run.state.emit(TransferEvent::ItemFailed {
        id: run.t.id,
        rel_path: run
            .s
            .items
            .get(item as usize)
            .map(|i| i.rel_path.clone())
            .unwrap_or_default(),
        error,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(size: u64) -> TransferItem {
        TransferItem {
            rel_path: "f".into(),
            size,
            modified_unix_ms: 0,
            is_dir: false,
        }
    }

    #[test]
    fn resume_points_are_validated() {
        let items = [file(3 * PIECE + 1), file(10)];
        assert_eq!(start_offsets(&items, &[]).unwrap(), [0, 0]);
        assert_eq!(
            start_offsets(&items, &[(0, 2 * PIECE), (1, 10)]).unwrap(),
            [2 * PIECE, 10]
        );
        assert!(start_offsets(&items, &[(0, 5)]).is_err());
        assert!(start_offsets(&items, &[(1, 11)]).is_err());
        assert!(start_offsets(&items, &[(2, 0)]).is_err());
    }
}
