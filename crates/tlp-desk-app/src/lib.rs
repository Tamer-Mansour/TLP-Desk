//! tlp-desk GUI library: the egui app, the backend boundary and i18n. The `tlp-desk` binary
//! (`main.rs`) wires it to a backend implementation.
#![forbid(unsafe_code)]

pub mod app;
pub mod backend;
pub mod i18n;
pub mod input_map;
pub mod keygrab;
pub mod settings;
pub mod ui;
