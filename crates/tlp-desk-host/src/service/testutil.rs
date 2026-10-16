//! Test helpers: an in-memory, blocking, bidirectional byte pipe (like a named pipe pair).

use std::io::{ErrorKind, Read, Write};
use std::sync::mpsc::{channel, Receiver, Sender};

/// One end of an in-memory duplex pipe. Reading blocks until the other end writes; when the
/// other end is dropped, reads return end-of-stream and writes fail with `BrokenPipe`.
pub(crate) struct PipeEnd {
    tx: Sender<Vec<u8>>,
    rx: Receiver<Vec<u8>>,
    pending: Vec<u8>,
    pos: usize,
}

/// A connected pair of [`PipeEnd`]s.
pub(crate) fn duplex() -> (PipeEnd, PipeEnd) {
    let (a_tx, b_rx) = channel();
    let (b_tx, a_rx) = channel();
    (
        PipeEnd {
            tx: a_tx,
            rx: a_rx,
            pending: Vec::new(),
            pos: 0,
        },
        PipeEnd {
            tx: b_tx,
            rx: b_rx,
            pending: Vec::new(),
            pos: 0,
        },
    )
}

impl Read for PipeEnd {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.pos >= self.pending.len() {
            match self.rx.recv() {
                Ok(chunk) => {
                    self.pending = chunk;
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let avail = &self.pending[self.pos..];
        let n = avail.len().min(buf.len());
        buf[..n].copy_from_slice(&avail[..n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for PipeEnd {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.tx
            .send(buf.to_vec())
            .map_err(|_| std::io::Error::from(ErrorKind::BrokenPipe))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) mod features;
