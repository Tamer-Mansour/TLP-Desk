// Shared build-script helper (included with `include!` by the build.rs of tlp-desk-app and
// tlp-desk-cli): compiles a Windows resource script (exe icon + VERSIONINFO) with the Windows
// SDK `rc.exe` and links the resulting `.res` into the crate's binaries.
//
// No extra crates: works with `cargo build --offline`. Non-Windows / non-MSVC targets skip it.
// rc.exe is taken from `$RC`, then PATH, then the newest `Windows Kits\10\bin\<ver>\<host>\`.
// If rc.exe is missing the build continues without an icon (cargo warning), unless
// `TLP_DESK_REQUIRE_EXE_ICON=1` is set (the installer build sets it), which makes it an error.

use std::path::{Path, PathBuf};

fn find_rc() -> Option<PathBuf> {
    if let Some(rc) = std::env::var_os("RC") {
        return Some(PathBuf::from(rc));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join("rc.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let host = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else {
        "x64"
    };
    let pf86 = std::env::var_os("ProgramFiles(x86)").map(PathBuf::from)?;
    let bin = pf86.join("Windows Kits").join("10").join("bin");
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for entry in std::fs::read_dir(&bin).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(ver) = name
            .split('.')
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
        else {
            continue;
        };
        let rc = entry.path().join(host).join("rc.exe");
        if rc.is_file() && best.as_ref().is_none_or(|(b, _)| ver > *b) {
            best = Some((ver, rc));
        }
    }
    best.map(|(_, p)| p)
}

/// Escape a string for an .rc string literal (ASCII only is expected).
fn rc_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\"\"")
}

#[allow(dead_code)]
fn embed_windows_resources(file_description: &str, original_filename: &str, icon: &Path) {
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-env-changed=TLP_DESK_REQUIRE_EXE_ICON");
    println!("cargo:rerun-if-env-changed=RC");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }
    let required = std::env::var("TLP_DESK_REQUIRE_EXE_ICON").is_ok_and(|v| v == "1");
    let fail = |msg: String| {
        if required {
            panic!("{msg}");
        }
        println!("cargo:warning={msg} (exe will have no icon/version info)");
    };
    let Some(rc) = find_rc() else {
        fail("rc.exe (Windows SDK) not found".into());
        return;
    };

    let v = |k: &str| std::env::var(k).unwrap_or_else(|_| "0".into());
    let (major, minor, patch) = (
        v("CARGO_PKG_VERSION_MAJOR"),
        v("CARGO_PKG_VERSION_MINOR"),
        v("CARGO_PKG_VERSION_PATCH"),
    );
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    // VS_FF_PRERELEASE = 0x2 for "-rc.N" style versions, so Explorer's details match the release.
    let prerelease = std::env::var("CARGO_PKG_VERSION_PRE").is_ok_and(|p| !p.is_empty());
    let flags = if prerelease { "0x2L" } else { "0x0L" };
    let icon_abs = icon.canonicalize().unwrap_or_else(|_| icon.to_path_buf());
    let icon_str = icon_abs
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();

    // Numeric constants instead of <winver.h> so rc.exe needs no INCLUDE path:
    // VOS_NT_WINDOWS32 = 0x40004, VFT_APP = 0x1, 0x0409/1200 = en-US, Unicode.
    // Keep every StringFileInfo field filled in (company, product, description, copyright,
    // original file name): Explorer shows them under Properties > Details, and the signature's
    // publisher should match CompanyName (docs/src/dev/code-signing.md).
    let script = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3fL
FILEFLAGS {flags}
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "Comments", "Free, open-source remote desktop for the local network. MIT OR Apache-2.0. https://github.com/Tamer-Mansour/TLP-Desk"
      VALUE "CompanyName", "TLP"
      VALUE "FileDescription", "{desc}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{orig}"
      VALUE "LegalCopyright", "Copyright (c) 2026 Tamer Mansour and the TLP-Desk contributors"
      VALUE "OriginalFilename", "{orig}"
      VALUE "ProductName", "TLP-Desk"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = rc_str(&icon_str),
        desc = rc_str(file_description),
        orig = rc_str(original_filename),
        version = rc_str(&version),
    );

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let rc_file = out.join("tlp-desk.rc");
    let res_file = out.join("tlp-desk.res");
    std::fs::write(&rc_file, script).expect("write .rc");
    let status = std::process::Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_file)
        .arg(&rc_file)
        .status();
    match status {
        Ok(s) if s.success() => {
            // link.exe accepts compiled .res files as inputs; only the binaries get it.
            println!("cargo:rustc-link-arg-bins={}", res_file.display());
        }
        Ok(s) => fail(format!("{} failed with {s}", rc.display())),
        Err(e) => fail(format!("could not run {}: {e}", rc.display())),
    }
}
