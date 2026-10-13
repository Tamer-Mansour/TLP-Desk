//! File transfer and clipboard sync.
//!
//! * [`fs`]: browse and change the other device's files (protocol v4 `FsRequest`/`FsResponse`).
// `deny`, not `forbid`: only the Win32 wrapper `fs::windows` opts back in (drive labels, free
// space, known folders), with a `// SAFETY:` comment on every block.
#![deny(unsafe_code)]

pub mod fs;
