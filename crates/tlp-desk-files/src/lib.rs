//! File transfer and clipboard sync.
//!
//! * [`clipfiles`]: Explorer copy/paste of files between the devices (`ClipboardFiles`).
//! * [`fs`]: browse and change the other device's files (protocol v4 `FsRequest`/`FsResponse`).
// `deny`, not `forbid`: only the Win32 wrapper `fs::windows` opts back in (drive labels, free
// space, known folders), with a `// SAFETY:` comment on every block.
#![deny(unsafe_code)]

pub mod clipfiles;
pub mod fs;
