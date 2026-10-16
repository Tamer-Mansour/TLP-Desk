//! TLP-Desk GUI library: the egui app, the backend boundary and i18n. The `TLP-Desk` binary
//! (`main.rs`) wires it to a backend implementation.
#![forbid(unsafe_code)]

pub mod address_book;
pub mod app;
pub mod backend;
pub mod banner;
pub mod connection_log;
pub mod i18n;
pub mod input_map;
pub mod keygrab;
#[cfg(windows)]
pub mod service_entry;
pub mod service_host;
pub mod settings;
pub mod text_route;
pub mod tray;
pub mod ui;
