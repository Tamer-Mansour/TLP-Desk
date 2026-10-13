//! `id`: print this device's ID and key fingerprint.

use std::path::Path;

use anyhow::Result;
use clap::Args;
use serde_json::{json, Value};
use tlp_desk_crypto::{DeviceId, Fingerprint};

use super::common::{id_str, load_identity, Ctx};
use crate::text;

/// Arguments of `id` (none yet).
#[derive(Debug, Clone, Default, Args)]
pub struct IdArgs {}

/// Runs `id`.
pub fn run(ctx: &Ctx, _args: &IdArgs) -> Result<()> {
    let id = load_identity(&ctx.data_dir)?;
    let dir = ctx.data_dir.display().to_string();
    let fp = id.fingerprint();
    ctx.out.result(
        text::identity(id.device_id(), &fp, &dir),
        &identity_json(id.device_id(), &fp, &ctx.data_dir),
    );
    Ok(())
}

/// `{"type":"identity","device_id","fingerprint","data_dir"}`.
pub fn identity_json(id: DeviceId, fp: &Fingerprint, dir: &Path) -> Value {
    json!({
        "type": "identity",
        "device_id": id_str(id),
        "fingerprint": fp.to_hex(),
        "data_dir": dir.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_json_schema() {
        let dir = std::env::temp_dir().join(format!("tlp-cli-id-مجلد-{}", std::process::id()));
        let ident = load_identity(&dir).unwrap();
        let v = identity_json(ident.device_id(), &ident.fingerprint(), &dir);
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["data_dir", "device_id", "fingerprint", "type"]);
        assert_eq!(v["type"], "identity");
        let id = v["device_id"].as_str().unwrap();
        assert_eq!(id.len(), 9);
        assert!(id.bytes().all(|b| b.is_ascii_digit()));
        assert!(v["data_dir"].as_str().unwrap().contains("مجلد"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
