//! Receiving side: writes incoming `FileData` streams at their offsets, hashes them piece by
//! piece on the fly, verifies each file against the sender's BLAKE3 and renames it into place.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tlp_desk_net::{RecvStream, STREAM_REFUSED};
use tlp_desk_proto::{FileDataHeader, FsError, Message, TransferItem, FILE_DATA_HEADER_LEN};

use super::disk::{
    available_space, from_unix_ms, open_read, open_write, set_dir_modified, write_all_at, IO_BUF,
};
use super::error::{io_to_fs_error, FilesError};
use super::hash::{
    combine, hash_file, hash_pieces_from_disk, piece_count, piece_len, Digest, PieceHasher, PIECE,
};
use super::plan::{
    create_dirs, meta_of, plan_receive_with, remove_partial, write_meta, Decisions, ReceivePlan,
    Target,
};
use super::session::SessionState;
use super::state::{lock, RangeSet, Transfer};
use super::types::{FileState, TransferEvent, TransferState};

/// How long a new stream may take to deliver its 12-byte header.
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Receiving,
    Finalizing,
    Done,
    Failed,
}

struct ItemRx {
    phase: Phase,
    file: Option<Arc<File>>,
    /// Ranges handed to a writer (overlap detection).
    claimed: RangeSet,
    /// Ranges durably written.
    written: RangeSet,
    /// Per-piece digests hashed on the fly.
    pieces: Vec<Option<Digest>>,
    /// Some data did not arrive piece-aligned: hash the file from disk instead.
    rehash: bool,
    expected: Option<Digest>,
}

/// Receiver half of a transfer.
pub struct Receiving {
    /// Offered items.
    pub items: Vec<TransferItem>,
    /// Canonical destination folder.
    pub root: PathBuf,
    /// Where each item goes.
    targets: Vec<Target>,
    /// Resume points to announce (`TransferAccept` / offer).
    pub resume: Vec<(u32, u64)>,
    states: Vec<Mutex<ItemRx>>,
    /// File items not yet verified or failed.
    pending: AtomicUsize,
}

impl Receiving {
    /// Plans the destination, checks free space and creates the folders.
    ///
    /// # Errors
    /// Planning errors (see [`plan_receive`]), not enough space, folder creation errors.
    pub fn prepare(
        dest: &Path,
        items: Vec<TransferItem>,
        decisions: &Decisions,
    ) -> Result<Self, FilesError> {
        let plan = plan_receive_with(dest, &items, decisions)?;
        let free = available_space(&plan.root).map_err(|e| FilesError::io(&plan.root, e))?;
        if plan.need_bytes > free {
            return Err(FilesError::NoSpace {
                need: plan.need_bytes,
                free,
            });
        }
        create_dirs(&plan)?;
        Ok(Self::from_plan(items, plan))
    }

    fn from_plan(items: Vec<TransferItem>, plan: ReceivePlan) -> Self {
        let mut pending = 0;
        let states = items
            .iter()
            .zip(&plan.targets)
            .map(|(item, target)| {
                let phase = match target {
                    Target::File { .. } | Target::Identical { .. } => {
                        pending += 1;
                        Phase::Receiving
                    }
                    Target::Dir { .. } | Target::Skipped { .. } => Phase::Done,
                };
                Mutex::new(ItemRx {
                    phase,
                    file: None,
                    claimed: RangeSet::default(),
                    written: RangeSet::default(),
                    pieces: vec![None; usize::try_from(piece_count(item.size, PIECE)).unwrap_or(0)],
                    rehash: false,
                    expected: None,
                })
            })
            .collect();
        Self {
            items,
            root: plan.root,
            targets: plan.targets,
            resume: plan.resume,
            states,
            pending: AtomicUsize::new(pending),
        }
    }

    /// Counts resumed, identical and skipped bytes and skipped files as already done.
    pub fn init_progress(&self, t: &Transfer) {
        let resumed: u64 = self.resume.iter().map(|(_, off)| off).sum();
        t.bytes_done.store(resumed, Ordering::Relaxed);
        t.resumed.store(resumed, Ordering::Relaxed);
        let skipped = self
            .targets
            .iter()
            .filter(|t| matches!(t, Target::Skipped { .. }))
            .count();
        t.files_done.store(
            u32::try_from(skipped).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
        for (i, target) in self.targets.iter().enumerate() {
            match target {
                Target::Skipped { .. } => t.item_state(i, FileState::Skipped),
                Target::File { resume, .. } => t.item_resumed(i, *resume),
                _ => {}
            }
        }
    }

    fn resume_of(&self, item: usize) -> u64 {
        match self.targets.get(item) {
            Some(Target::File { resume, .. }) => *resume,
            _ => 0,
        }
    }

    /// After the transfer stopped: record how much of each unfinished file is valid so the
    /// next attempt resumes; on completion only folder times are restored.
    pub fn save_partials(&self, complete: bool) {
        if complete {
            // Deepest folders first: setting a parent's time does not touch its children.
            let mut dirs: Vec<(&Path, i64)> = self
                .targets
                .iter()
                .zip(&self.items)
                .filter_map(|(t, i)| match t {
                    Target::Dir { path } => Some((path.as_path(), i.modified_unix_ms)),
                    _ => None,
                })
                .collect();
            dirs.sort_by_key(|(p, _)| std::cmp::Reverse(p.components().count()));
            for (p, ms) in dirs {
                set_dir_modified(p, ms);
            }
            return;
        }
        for ((target, item), st) in self.targets.iter().zip(&self.items).zip(&self.states) {
            let Target::File {
                final_path, resume, ..
            } = target
            else {
                continue;
            };
            let mut st = lock(st);
            st.file = None;
            if matches!(st.phase, Phase::Receiving | Phase::Failed) {
                let valid = st.written.contiguous_end(*resume).max(*resume);
                if valid > 0 {
                    write_meta(final_path, item, valid);
                }
            }
        }
    }
}

/// Handles one incoming data stream of a transfer we receive.
pub async fn handle_stream(
    state: Arc<SessionState>,
    t: Arc<Transfer>,
    rx: Arc<Receiving>,
    mut recv: RecvStream,
) {
    let header = match read_header(&mut recv).await {
        Some(h) => h,
        None => {
            let _ = recv.stop(STREAM_REFUSED.into());
            return;
        }
    };
    let res = tokio::select! {
        r = receive(&rx, &t, &mut recv, header) => r,
        () = t.wait_stop() => Err(StreamError::Stopped),
    };
    let item = header.item;
    match res {
        Ok(()) => maybe_finalize(&state, &t, &rx, item),
        Err(StreamError::Stopped) => {
            let _ = recv.stop(STREAM_REFUSED.into());
        }
        Err(StreamError::Net) => {
            tracing::debug!(transfer = t.id.0, item, "file data stream ended abnormally");
        }
        Err(StreamError::Protocol(m)) => {
            let _ = recv.stop(STREAM_REFUSED.into());
            tracing::warn!(transfer = t.id.0, item, why = m, "bad file data stream");
            fail_item(&state, &t, &rx, item, FsError::Other(m.into()), true);
        }
        Err(StreamError::Disk(e)) => {
            let _ = recv.stop(STREAM_REFUSED.into());
            if e == FsError::NoSpace {
                state.abort(&t, TransferState::Failed(FsError::NoSpace));
            } else {
                fail_item(&state, &t, &rx, item, e, true);
            }
        }
    }
}

async fn read_header(recv: &mut RecvStream) -> Option<FileDataHeader> {
    let mut b = [0u8; FILE_DATA_HEADER_LEN];
    let mut got = 0;
    let read = async {
        while got < b.len() {
            match recv.read(b.get_mut(got..)?).await {
                Ok(Some(n)) => got += n,
                _ => return None,
            }
        }
        Some(())
    };
    tokio::time::timeout(HEADER_TIMEOUT, read).await.ok()??;
    Some(FileDataHeader::decode(&b))
}

enum StreamError {
    Stopped,
    Net,
    Protocol(&'static str),
    Disk(FsError),
}

/// Hashing state of a stream: the piece being filled.
struct Hashing {
    index: u64,
    hasher: PieceHasher,
    filled: u64,
}

type WriteResult = Result<(Vec<u8>, Option<Hashing>, Vec<(u64, Digest)>), FsError>;

async fn receive(
    rx: &Receiving,
    t: &Transfer,
    recv: &mut RecvStream,
    h: FileDataHeader,
) -> Result<(), StreamError> {
    let idx = h.item as usize;
    let item = rx
        .items
        .get(idx)
        .ok_or(StreamError::Protocol("unknown item"))?;
    let (part, resume) = match rx.targets.get(idx) {
        Some(Target::File { part, resume, .. }) => (part.clone(), *resume),
        _ => return Err(StreamError::Protocol("no data expected for this item")),
    };
    let size = item.size;
    if h.offset < resume || h.offset >= size {
        return Err(StreamError::Protocol("offset outside the file"));
    }
    let file = {
        let st_mutex = rx
            .states
            .get(idx)
            .ok_or(StreamError::Protocol("unknown item"))?;
        let mut st = lock(st_mutex);
        if st.phase != Phase::Receiving {
            return Err(StreamError::Stopped);
        }
        match &st.file {
            Some(f) => f.clone(),
            None => {
                let f = Arc::new(
                    open_write(&part, resume == 0)
                        .map_err(|e| StreamError::Disk(io_to_fs_error(&e)))?,
                );
                st.file = Some(f.clone());
                f
            }
        }
    };
    let mut hashing = (h.offset % PIECE == 0).then(|| Hashing {
        index: h.offset / PIECE,
        hasher: PieceHasher::new(h.offset, size, PIECE),
        filled: 0,
    });
    if hashing.is_none() {
        mark_rehash(rx, idx);
    }
    let mut pos = h.offset;
    let mut pending: Option<(tokio::task::JoinHandle<WriteResult>, u64, u64)> = None;
    let mut spare: Option<Vec<u8>> = None;
    loop {
        let mut buf = spare.take().unwrap_or_else(|| vec![0u8; IO_BUF]);
        let n = fill(recv, &mut buf).await?;
        // Wait for the previous write while this buffer was being received.
        if let Some((job, start, end)) = pending.take() {
            let (old, hs, digests) = finish_write(job).await?;
            record(rx, t, idx, start, end, &digests);
            hashing = hs;
            spare = Some(old);
        }
        if n == 0 {
            break;
        }
        let end = pos
            .checked_add(n as u64)
            .ok_or(StreamError::Protocol("offset overflow"))?;
        if end > size {
            return Err(StreamError::Protocol("more data than the file size"));
        }
        if !claim(rx, idx, pos, end) {
            return Err(StreamError::Protocol("overlapping data"));
        }
        let f = file.clone();
        let hs = hashing.take();
        let job = tokio::task::spawn_blocking(move || write_and_hash(&f, buf, n, pos, size, hs));
        pending = Some((job, pos, end));
        pos = end;
    }
    drop(file);
    if let Some(hs) = hashing {
        // A piece that did not reach its end: its hash cannot be used.
        if hs.filled > 0 {
            mark_rehash(rx, idx);
        }
    }
    Ok(())
}

/// Reads until `buf` is full or the stream ends; returns the byte count.
async fn fill(recv: &mut RecvStream, buf: &mut [u8]) -> Result<usize, StreamError> {
    let mut n = 0;
    while n < buf.len() {
        let slot = buf.get_mut(n..).unwrap_or_default();
        match recv.read(slot).await {
            Ok(Some(k)) => n += k,
            Ok(None) => break,
            Err(_) => return Err(StreamError::Net),
        }
    }
    Ok(n)
}

async fn finish_write(
    job: tokio::task::JoinHandle<WriteResult>,
) -> Result<(Vec<u8>, Option<Hashing>, Vec<(u64, Digest)>), StreamError> {
    job.await
        .map_err(|_| StreamError::Disk(FsError::Other("write task failed".into())))?
        .map_err(StreamError::Disk)
}

/// Blocking: writes `buf[..n]` at `pos` and feeds it to the piece hashers.
fn write_and_hash(
    file: &File,
    buf: Vec<u8>,
    n: usize,
    pos: u64,
    size: u64,
    mut hs: Option<Hashing>,
) -> WriteResult {
    let data = buf.get(..n).unwrap_or_default();
    write_all_at(file, data, pos).map_err(|e| io_to_fs_error(&e))?;
    let mut done = Vec::new();
    let mut rest = data;
    while let Some(h) = hs.as_mut() {
        if rest.is_empty() {
            break;
        }
        let want = piece_len(size, PIECE, h.index) - h.filled;
        let take = usize::try_from(want).unwrap_or(usize::MAX).min(rest.len());
        let (now, later) = rest.split_at(take);
        h.hasher.update(now);
        h.filled += take as u64;
        rest = later;
        if h.filled == piece_len(size, PIECE, h.index) {
            done.push((h.index, h.hasher.finish()));
            let next = h.index + 1;
            let offset = next * PIECE;
            hs = (offset < size).then(|| Hashing {
                index: next,
                hasher: PieceHasher::new(offset, size, PIECE),
                filled: 0,
            });
        }
    }
    Ok((buf, hs, done))
}

fn claim(rx: &Receiving, idx: usize, start: u64, end: u64) -> bool {
    rx.states
        .get(idx)
        .is_some_and(|st| lock(st).claimed.insert(start, end))
}

fn record(
    rx: &Receiving,
    t: &Transfer,
    idx: usize,
    start: u64,
    end: u64,
    digests: &[(u64, Digest)],
) {
    if let Some(st) = rx.states.get(idx) {
        let mut st = lock(st);
        st.written.insert(start, end);
        for (i, d) in digests {
            if let Some(slot) = st.pieces.get_mut(usize::try_from(*i).unwrap_or(usize::MAX)) {
                *slot = Some(*d);
            }
        }
    }
    t.bytes_done.fetch_add(end - start, Ordering::Relaxed);
    t.item_bytes(idx, end - start);
}

fn mark_rehash(rx: &Receiving, idx: usize) {
    if let Some(st) = rx.states.get(idx) {
        lock(st).rehash = true;
    }
}

/// The sender finished an item and sent its hash.
pub fn on_done(
    state: &Arc<SessionState>,
    t: &Arc<Transfer>,
    rx: &Arc<Receiving>,
    item: u32,
    hash: Digest,
) {
    let Some(st) = rx.states.get(item as usize) else {
        tracing::warn!(transfer = t.id.0, item, "hash for an unknown item");
        return;
    };
    {
        let mut st = lock(st);
        if st.phase != Phase::Receiving {
            return;
        }
        st.expected = Some(hash);
    }
    maybe_finalize(state, t, rx, item);
}

/// The sender could not read an item: keep what we have for a later resume.
pub fn on_sender_item_error(
    state: &Arc<SessionState>,
    t: &Arc<Transfer>,
    rx: &Arc<Receiving>,
    item: u32,
    error: FsError,
) {
    fail_item(state, t, rx, item, error, false);
}

/// Marks an item failed (once), tells the UI and, if `tell_peer`, the sender.
fn fail_item(
    state: &Arc<SessionState>,
    t: &Arc<Transfer>,
    rx: &Arc<Receiving>,
    item: u32,
    error: FsError,
    tell_peer: bool,
) {
    let Some(st) = rx.states.get(item as usize) else {
        return;
    };
    {
        let mut st = lock(st);
        if matches!(st.phase, Phase::Done | Phase::Failed) {
            return;
        }
        st.phase = Phase::Failed;
        st.file = None;
    }
    if t.stopped() {
        return;
    }
    t.failed.fetch_add(1, Ordering::Relaxed);
    t.item_state(item as usize, FileState::Failed);
    if tell_peer {
        state.send(Message::TransferError {
            id: t.wire,
            item: Some(item),
            error: error.clone(),
        });
    }
    state.emit(TransferEvent::ItemFailed {
        id: t.id,
        rel_path: rx
            .items
            .get(item as usize)
            .map(|i| i.rel_path.clone())
            .unwrap_or_default(),
        error,
    });
    item_settled(state, t, rx);
}

/// Verifies and moves a file into place once all its bytes and its hash are here.
fn maybe_finalize(state: &Arc<SessionState>, t: &Arc<Transfer>, rx: &Arc<Receiving>, item: u32) {
    let idx = item as usize;
    let (Some(st), Some(info)) = (rx.states.get(idx), rx.items.get(idx)) else {
        return;
    };
    let resume = rx.resume_of(idx);
    let job = {
        let mut st = lock(st);
        let need = info.size.saturating_sub(resume);
        let complete = matches!(rx.targets.get(idx), Some(Target::Identical { .. }))
            || st.written.total() >= need;
        let Some(expected) = st.expected else { return };
        if st.phase != Phase::Receiving || !complete {
            return;
        }
        st.phase = Phase::Finalizing;
        FinalizeJob {
            target: rx.targets.get(idx).cloned(),
            size: info.size,
            mtime: info.modified_unix_ms,
            resume,
            pieces: std::mem::take(&mut st.pieces),
            rehash: st.rehash,
            expected,
            file: st.file.take(),
        }
    };
    let (state, t, rx) = (state.clone(), t.clone(), rx.clone());
    tokio::spawn(async move {
        let res = tokio::task::spawn_blocking(move || job.run())
            .await
            .unwrap_or_else(|_| Err(FsError::Other("verify task failed".into())));
        let Some(st) = rx.states.get(idx) else { return };
        match res {
            Ok(()) => {
                lock(st).phase = Phase::Done;
                t.files_done.fetch_add(1, Ordering::Relaxed);
                t.item_state(idx, FileState::Done);
                item_settled(&state, &t, &rx);
            }
            Err(e) => {
                tracing::warn!(transfer = t.id.0, item, ?e, "file failed verification");
                lock(st).phase = Phase::Receiving;
                fail_item(&state, &t, &rx, item, e, true);
            }
        }
    });
}

/// One more file is finished; the last one completes the transfer.
fn item_settled(state: &Arc<SessionState>, t: &Arc<Transfer>, rx: &Arc<Receiving>) {
    if rx.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
        complete(state, t);
    }
}

/// Completes a transfer that has no files left to wait for (also right after accepting an
/// offer with only folders, empty or identical files).
pub fn check_complete(state: &Arc<SessionState>, t: &Arc<Transfer>, rx: &Arc<Receiving>) {
    if rx.pending.load(Ordering::Acquire) == 0 {
        complete(state, t);
    }
}

fn complete(state: &Arc<SessionState>, t: &Arc<Transfer>) {
    if t.stopped() {
        return;
    }
    t.bytes_done.store(t.info.total_bytes, Ordering::Relaxed);
    // `bytes_done == total_bytes` tells the sender every file is settled.
    state.send(Message::TransferProgress {
        id: t.wire,
        bytes_done: t.info.total_bytes,
    });
    state.end(t, TransferState::Completed);
}

struct FinalizeJob {
    target: Option<Target>,
    size: u64,
    mtime: i64,
    resume: u64,
    pieces: Vec<Option<Digest>>,
    rehash: bool,
    expected: Digest,
    file: Option<Arc<File>>,
}

impl FinalizeJob {
    fn run(self) -> Result<(), FsError> {
        let io = |e: std::io::Error| io_to_fs_error(&e);
        match &self.target {
            Some(Target::Identical { path }) => {
                let f = open_read(path).map_err(io)?;
                if hash_file(&f, self.size).map_err(io)? != self.expected {
                    return Err(FsError::HashMismatch);
                }
                Ok(())
            }
            Some(Target::File {
                part, final_path, ..
            }) => {
                let file = match self.file.clone() {
                    Some(f) => f,
                    None => {
                        Arc::new(open_write(part, self.resume == 0 && self.size == 0).map_err(io)?)
                    }
                };
                let got = self.hash(&file).map_err(io)?;
                if got != self.expected {
                    drop(file);
                    remove_partial(final_path);
                    return Err(FsError::HashMismatch);
                }
                if let Some(t) = from_unix_ms(self.mtime) {
                    if let Err(e) = file.set_modified(t) {
                        tracing::debug!(%e, "could not restore a file time");
                    }
                }
                drop(file);
                std::fs::rename(part, final_path).map_err(io)?;
                if self.resume > 0 {
                    // Only resumed files have a record (stale ones are removed while planning).
                    let _ = std::fs::remove_file(meta_of(final_path));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn hash(&self, file: &File) -> std::io::Result<Digest> {
        if self.size == 0 {
            return Ok(*blake3::hash(&[]).as_bytes());
        }
        if self.rehash {
            return hash_file(file, self.size);
        }
        let mut pieces = self.pieces.clone();
        let first = self.resume / PIECE;
        if first > 0 {
            let prefix = hash_pieces_from_disk(file, self.size, PIECE, 0, first)?;
            for (slot, d) in pieces.iter_mut().zip(prefix) {
                *slot = Some(d);
            }
        }
        match combine(self.size, PIECE, &pieces) {
            Some(d) => Ok(d),
            None => hash_file(file, self.size),
        }
    }
}
