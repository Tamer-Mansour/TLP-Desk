//! File transfer and clipboard sync.
//!
//! * [`fs`]: browse and change the other device's files (protocol v4 `FsRequest`/`FsResponse`).
#![forbid(unsafe_code)]

pub mod fs;
