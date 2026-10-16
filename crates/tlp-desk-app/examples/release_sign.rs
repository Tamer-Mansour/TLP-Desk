//! Release signing tool behind `scripts/gen-release-key.ps1` and `scripts/sign-release.ps1`.
//!
//! ```text
//! release_sign keygen <dir>                 writes <dir>/release.key (SECRET) and <dir>/release.pub
//! release_sign sign <file> <comment>        key from TLP_RELEASE_SIGNING_KEY (path or base64);
//!                                           writes <file>.minisig
//! release_sign verify <file> <pubkey-b64>   checks <file>.minisig
//! ```
//!
//! Signatures are minisign-compatible (prehashed ed25519), so `minisign -Vm <file> -P <pub>`
//! verifies them too.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[path = "../src/update/sign.rs"]
mod sign;

use sign::SecretKey;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["keygen", dir] => keygen(Path::new(dir)),
        ["sign", file, comment] => sign_file(Path::new(file), comment),
        ["verify", file, pubkey] => verify(Path::new(file), pubkey),
        _ => Err(
            "usage: release_sign keygen <dir> | sign <file> <comment> | verify <file> <pubkey>"
                .into(),
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn keygen(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let key_path = dir.join("release.key");
    if key_path.exists() {
        return Err(format!(
            "{} exists; refusing to overwrite a key",
            key_path.display()
        ));
    }
    let key = SecretKey::generate()?;
    std::fs::write(&key_path, key.encode()).map_err(|e| e.to_string())?;
    let pub_path = dir.join("release.pub");
    std::fs::write(
        &pub_path,
        format!(
            "untrusted comment: TLP-Desk release public key\n{}\n",
            key.public_key_base64()
        ),
    )
    .map_err(|e| e.to_string())?;
    println!("secret key: {}", key_path.display());
    println!("public key: {}", pub_path.display());
    println!("public key (embed in crates/tlp-desk-app/src/update/pubkey.rs):");
    println!("{}", key.public_key_base64());
    Ok(())
}

fn load_key() -> Result<SecretKey, String> {
    let v = std::env::var("TLP_RELEASE_SIGNING_KEY")
        .map_err(|_| "TLP_RELEASE_SIGNING_KEY is not set".to_owned())?;
    let p = PathBuf::from(v.trim());
    let text = if p.is_file() {
        std::fs::read_to_string(&p).map_err(|e| e.to_string())?
    } else {
        v
    };
    SecretKey::decode(&text).ok_or_else(|| "TLP_RELEASE_SIGNING_KEY is not a valid key".to_owned())
}

fn sig_path(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(".minisig");
    PathBuf::from(s)
}

fn sign_file(file: &Path, comment: &str) -> Result<(), String> {
    let key = load_key()?;
    let data = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let out = sig_path(file);
    std::fs::write(&out, key.sign(&data, comment)).map_err(|e| e.to_string())?;
    println!("signed: {}", out.display());
    println!("public key: {}", key.public_key_base64());
    Ok(())
}

fn verify(file: &Path, pubkey: &str) -> Result<(), String> {
    let data = std::fs::read(file).map_err(|e| e.to_string())?;
    let sig = std::fs::read_to_string(sig_path(file)).map_err(|e| e.to_string())?;
    let pk = minisign_verify::PublicKey::from_base64(pubkey.trim()).map_err(|e| e.to_string())?;
    let sig = minisign_verify::Signature::decode(&sig).map_err(|e| e.to_string())?;
    pk.verify(&data, &sig, false).map_err(|e| e.to_string())?;
    println!("signature OK");
    Ok(())
}
