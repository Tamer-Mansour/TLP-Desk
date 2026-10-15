//! Shared by `ls`, `mkdir`, `rm` and `mv`: dial the host with the remote file-system feature
//! and run one request.

use std::future::Future;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tlp_desk_files::fs::{FsClient, FsConfig, FsFeature};
use tlp_desk_net::FeatureSet;
use tlp_desk_proto::{FsEntry, FsRoot};

use super::common::Ctx;
use super::common::{dial, DialArgs};

/// Dials `args`, runs `f` with the session's file-system client, then disconnects (also when
/// `f` fails).
pub async fn with_client<T, Fut>(
    ctx: &Ctx,
    args: &DialArgs,
    f: impl FnOnce(FsClient) -> Fut,
) -> Result<T>
where
    Fut: Future<Output = Result<T>>,
{
    // The CLI never serves its own disks: the viewer side of the feature only sends requests.
    let fs = FsFeature::new(FsConfig::default());
    let dialed = dial(ctx, args, FeatureSet::new().with(fs.clone())).await?;
    let result = match fs
        .client(dialed.session.feature_key())
        .context("the file feature did not start")
    {
        Ok(client) => f(client).await,
        Err(e) => Err(e),
    };
    dialed.close().await;
    result
}

/// `{"type":"entry", ...}` for one directory entry.
pub fn entry_json(e: &FsEntry) -> Value {
    json!({
        "type": "entry",
        "name": e.name,
        "is_dir": e.is_dir,
        "size": e.size,
        "modified_unix_ms": e.modified_unix_ms,
        "hidden": e.hidden,
        "readonly": e.readonly,
    })
}

/// `{"type":"root", ...}` for one drive or known folder.
pub fn root_json(r: &FsRoot) -> Value {
    json!({
        "type": "root",
        "path": r.path,
        "label": r.label,
        "kind": format!("{:?}", r.kind),
        "total_bytes": r.total_bytes,
        "free_bytes": r.free_bytes,
    })
}

/// `{"type":"done","op":...,"path":...}` after a change.
pub fn done_json(op: &str, path: &str) -> Value {
    json!({ "type": "done", "op": op, "path": path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_proto::FsRootKind;

    #[test]
    fn json_schemas_keep_arabic_names() {
        let e = FsEntry {
            name: "ملف تجريبي.txt".into(),
            is_dir: false,
            size: 5,
            modified_unix_ms: 1,
            readonly: false,
            hidden: true,
        };
        let v = entry_json(&e);
        assert_eq!(v["type"], "entry");
        assert_eq!(v["name"], "ملف تجريبي.txt");
        assert_eq!(v["hidden"], true);
        let r = root_json(&FsRoot {
            path: "C:\\".into(),
            label: "النظام".into(),
            kind: FsRootKind::Drive,
            total_bytes: Some(10),
            free_bytes: None,
        });
        assert_eq!(r["kind"], "Drive");
        assert_eq!(r["label"], "النظام");
        assert!(r["free_bytes"].is_null());
        assert_eq!(done_json("rm", "C:\\x")["op"], "rm");
    }
}
