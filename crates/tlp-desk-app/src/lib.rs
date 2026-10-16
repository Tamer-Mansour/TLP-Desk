//! TLP-Desk GUI library: the egui app, the backend boundary and i18n. The `TLP-Desk` binary
//! (`main.rs`) wires it to a backend implementation.
// `deny`, not `forbid`: the Win32 helpers of the accept popup (`prompt::windows`) opt back in
// with `#[allow(unsafe_code)]` and a `// SAFETY:` comment on every block.
#![deny(unsafe_code)]

pub mod address_book;
pub mod app;
pub mod backend;
pub mod banner;
pub mod connection_log;
pub mod i18n;
pub mod input_map;
pub mod keygrab;
pub mod prompt;
#[cfg(windows)]
pub mod service_entry;
pub mod service_host;
pub mod settings;
pub mod text_route;
pub mod tray;
pub mod ui;
pub mod update;
