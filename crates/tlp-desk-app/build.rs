//! Embeds the exe icon and VERSIONINFO into `tlp-desk.exe` (Windows/MSVC only).
include!("winres.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=winres.rs");
    embed_windows_resources(
        "tlp-desk - LAN remote desktop",
        "tlp-desk.exe",
        std::path::Path::new("assets/icon/tlp-desk.ico"),
    );
}
