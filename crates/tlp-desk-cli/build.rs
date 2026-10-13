//! Embeds the exe icon and VERSIONINFO into `tlp-desk-cli.exe` (Windows/MSVC only).
//! The resource helper and the icon live in the tlp-desk-app crate.
include!("../tlp-desk-app/winres.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../tlp-desk-app/winres.rs");
    embed_windows_resources(
        "tlp-desk command-line tool",
        "tlp-desk-cli.exe",
        std::path::Path::new("../tlp-desk-app/assets/icon/tlp-desk.ico"),
    );
}
