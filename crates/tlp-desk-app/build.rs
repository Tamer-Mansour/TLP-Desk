//! Embeds the exe icon and VERSIONINFO into `TLP-Desk.exe` (Windows/MSVC only).
include!("winres.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=winres.rs");
    embed_windows_resources(
        "TLP-Desk - LAN remote desktop",
        "TLP-Desk.exe",
        std::path::Path::new("assets/icon/TLP-Desk.ico"),
    );
}
