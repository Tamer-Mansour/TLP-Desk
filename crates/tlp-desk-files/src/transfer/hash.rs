//! BLAKE3 hashing in independent pieces.
//!
//! Files travel in [`PIECE`]-sized ranges over several streams at once, so neither side sees
//! the bytes in order. BLAKE3 is a tree hash: a piece that starts at a multiple of a
//! power-of-two size is a complete subtree, so each piece is hashed on its own (on whatever
//! stream or thread has it) and the pieces are merged along the standard tree. The result is
//! exactly `blake3::hash(whole file)`, with no second pass over the data.

use std::fs::File;
use std::io;

use blake3::hazmat::{
    left_subtree_len, merge_subtrees_non_root, merge_subtrees_root, HasherExt, Mode,
};

/// Size of one transfer piece: the unit of parallel sending, hashing and resuming.
/// A power of two and a multiple of the BLAKE3 chunk size (1024).
pub const PIECE: u64 = 16 << 20;

/// A 32-byte BLAKE3 output (root hash or chaining value).
pub type Digest = [u8; 32];

/// Number of pieces of a file of `size` bytes (0 for an empty file).
pub fn piece_count(size: u64, piece: u64) -> u64 {
    size.div_ceil(piece.max(1))
}

/// Length of piece `index` of a file of `size` bytes.
pub fn piece_len(size: u64, piece: u64, index: u64) -> u64 {
    let start = index.saturating_mul(piece);
    size.saturating_sub(start).min(piece)
}

/// Incremental hash of one piece.
///
/// For a file of at most one piece, [`PieceHasher::finish`] is the file's root hash; for larger
/// files it is the piece's chaining value, to be merged with [`combine`].
pub struct PieceHasher {
    inner: blake3::Hasher,
    single: bool,
}

impl PieceHasher {
    /// Starts the piece that begins at `offset` (a multiple of `piece`) of a `size`-byte file.
    pub fn new(offset: u64, size: u64, piece: u64) -> Self {
        let mut inner = blake3::Hasher::new();
        let single = size <= piece;
        if !single {
            // `offset` is a multiple of `piece`, which is a multiple of 1024: a valid subtree
            // start (blake3 asserts this).
            inner.set_input_offset(offset);
        }
        Self { inner, single }
    }

    /// Feeds bytes of the piece, in order.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Root hash (single-piece file) or chaining value. Callers never finish an empty piece
    /// of a multi-piece file (pieces of such files are never empty).
    pub fn finish(&self) -> Digest {
        if self.single {
            *self.inner.finalize().as_bytes()
        } else {
            self.inner.finalize_non_root()
        }
    }
}

/// Merges per-piece digests of a `size`-byte file into its BLAKE3 hash. `pieces[i]` is
/// [`PieceHasher::finish`] of piece `i`. `None` if a piece is missing.
pub fn combine(size: u64, piece: u64, pieces: &[Option<Digest>]) -> Option<Digest> {
    if size <= piece {
        return match pieces.first() {
            Some(d) => *d,
            // An empty file has no pieces.
            None if size == 0 => Some(*blake3::hash(&[]).as_bytes()),
            None => None,
        };
    }
    let left = left_subtree_len(size);
    let l = subtree(0, left, piece, pieces)?;
    let r = subtree(left, size - left, piece, pieces)?;
    Some(*merge_subtrees_root(&l, &r, Mode::Hash).as_bytes())
}

/// Chaining value of the canonical subtree `[offset, offset + len)`. Every subtree of at most
/// one piece is exactly one piece because pieces are power-of-two aligned.
fn subtree(offset: u64, len: u64, piece: u64, pieces: &[Option<Digest>]) -> Option<Digest> {
    if len <= piece {
        let idx = usize::try_from(offset / piece).ok()?;
        return *pieces.get(idx)?;
    }
    let left = left_subtree_len(len);
    let l = subtree(offset, left, piece, pieces)?;
    let r = subtree(offset.checked_add(left)?, len - left, piece, pieces)?;
    Some(merge_subtrees_non_root(&l, &r, Mode::Hash))
}

/// Reads and hashes pieces `first..last` of `file` (a `size`-byte file) from disk, several
/// pieces in parallel. Used for the already-present prefix of a resumed file and as a
/// fallback when data did not arrive piece-aligned.
pub fn hash_pieces_from_disk(
    file: &File,
    size: u64,
    piece: u64,
    first: u64,
    last: u64,
) -> io::Result<Vec<Digest>> {
    let count = usize::try_from(last.saturating_sub(first))
        .map_err(|_| io::Error::other("too many pieces"))?;
    let mut out = vec![[0u8; 32]; count];
    if count == 0 {
        return Ok(out);
    }
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 8)
        .min(count);
    let per = count.div_ceil(threads);
    std::thread::scope(|s| -> io::Result<()> {
        let mut handles = Vec::new();
        for (t, slot) in out.chunks_mut(per).enumerate() {
            let start = first + (t * per) as u64;
            handles.push(s.spawn(move || -> io::Result<()> {
                let mut buf = vec![0u8; super::disk::IO_BUF];
                for (i, d) in slot.iter_mut().enumerate() {
                    let idx = start + i as u64;
                    *d = hash_one_piece(file, size, piece, idx, &mut buf)?;
                }
                Ok(())
            }));
        }
        for h in handles {
            h.join()
                .map_err(|_| io::Error::other("hash thread panicked"))??;
        }
        Ok(())
    })?;
    Ok(out)
}

fn hash_one_piece(
    file: &File,
    size: u64,
    piece: u64,
    idx: u64,
    buf: &mut [u8],
) -> io::Result<Digest> {
    let start = idx.saturating_mul(piece);
    let len = piece_len(size, piece, idx);
    let mut h = PieceHasher::new(start, size, piece);
    let mut done = 0u64;
    while done < len {
        let want = usize::try_from((len - done).min(buf.len() as u64)).unwrap_or(buf.len());
        let slice = buf
            .get_mut(..want)
            .ok_or_else(|| io::Error::other("buffer"))?;
        super::disk::read_exact_at(file, slice, start + done)?;
        h.update(slice);
        done += want as u64;
    }
    Ok(h.finish())
}

/// BLAKE3 of a whole file, hashed piece-parallel from disk.
pub fn hash_file(file: &File, size: u64) -> io::Result<Digest> {
    let n = piece_count(size, PIECE);
    let pieces = hash_pieces_from_disk(file, size, PIECE, 0, n)?;
    let pieces: Vec<Option<Digest>> = pieces.into_iter().map(Some).collect();
    combine(size, PIECE, &pieces).ok_or_else(|| io::Error::other("piece missing"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    fn piecewise(input: &[u8], piece: u64) -> Option<Digest> {
        let size = input.len() as u64;
        let n = piece_count(size, piece);
        let mut pieces = Vec::new();
        for i in 0..n {
            let start = (i * piece) as usize;
            let end = start + piece_len(size, piece, i) as usize;
            let mut h = PieceHasher::new(i * piece, size, piece);
            // Feed in odd-sized slices like a network stream would.
            for part in input[start..end].chunks(777) {
                h.update(part);
            }
            pieces.push(Some(h.finish()));
        }
        combine(size, piece, &pieces)
    }

    #[test]
    fn piecewise_hash_equals_plain_blake3() {
        let piece = 4096;
        for len in [
            0usize,
            1,
            1023,
            1024,
            1025,
            4095,
            4096,
            4097,
            8192,
            8193,
            12288,
            12289,
            3 * 4096 + 5,
            7 * 4096,
            9 * 4096 + 1,
            16 * 4096,
            17 * 4096 + 100,
            33 * 4096 - 1,
        ] {
            let input = data(len);
            assert_eq!(
                piecewise(&input, piece),
                Some(*blake3::hash(&input).as_bytes()),
                "len {len}"
            );
        }
    }

    #[test]
    fn missing_piece_is_detected() {
        let piece = 4096u64;
        let size = 3 * piece + 1;
        let pieces = vec![Some([0u8; 32]), None, Some([0u8; 32]), Some([0u8; 32])];
        assert_eq!(combine(size, piece, &pieces), None);
        assert_eq!(combine(size, piece, &pieces[..1]), None);
    }

    #[test]
    fn disk_hash_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ملف.bin");
        let input = data(3 * 1024 * 1024 + 17);
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&input)
            .unwrap();
        let f = std::fs::File::open(&path).unwrap();
        let size = input.len() as u64;
        let piece = 1 << 20;
        let n = piece_count(size, piece);
        let d = hash_pieces_from_disk(&f, size, piece, 0, n).unwrap();
        let d: Vec<_> = d.into_iter().map(Some).collect();
        assert_eq!(
            combine(size, piece, &d),
            Some(*blake3::hash(&input).as_bytes())
        );
        assert_eq!(
            hash_file(&f, size).unwrap(),
            *blake3::hash(&input).as_bytes()
        );
    }
}
