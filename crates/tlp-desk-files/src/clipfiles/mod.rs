//! Explorer copy/paste of files between the devices (protocol v4 `ClipboardFiles`).
//!
//! This part: turning a copied file list into transfer items.

mod items;

pub use items::{build, is_safe_rel_path, CopiedSet, MAX_DEPTH, MAX_ITEMS};
