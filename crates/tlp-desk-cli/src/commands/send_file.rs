//! `send-file`: upload local files and folders to a host (viewer side), with progress.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use clap::Args;
use serde_json::json;
use tlp_desk_files::{
    FileTransfer, FilesError, TransferConfig, TransferEvent, TransferId, TransferState,
};
use tlp_desk_net::FeatureSet;
use tlp_desk_proto::{FsError, Permissions, TransferDirection};
use tlp_desk_viewer::ViewerEvent;

use super::common::OnConflict;
use tokio::sync::broadcast;

use super::common::{dial, peer_json, Ctx, DialArgs, Dialed};
use crate::exit::CliError;
use crate::text;

/// Arguments of `send-file`.
#[derive(Debug, Clone, Args)]
pub struct SendFileArgs {
    #[command(flatten)]
    pub dial: DialArgs,
    /// Local files and folders to send (folders are sent recursively).
    #[arg(required = true, num_args = 1..)]
    pub paths: Vec<PathBuf>,
    /// Destination folder on the host (absolute host path; default: the host's Desktop).
    #[arg(long)]
    pub to: Option<String>,
}

/// Runs `send-file`.
pub async fn run(ctx: &Ctx, args: &SendFileArgs) -> Result<()> {
    let (ft, mut dialed) = connect(ctx, &args.dial, OnConflict::default()).await?;
    let mut sub = ft.subscribe();
    let key = dialed.session.feature_key();
    let id = ft
        .start_upload(key, args.paths.clone(), args.to.clone().unwrap_or_default())
        .await?;
    let res = watch(ctx, &ft, &mut dialed, id, &mut sub).await;
    dialed.close().await;
    res
}

/// Dials with the transfer feature and checks the session may move files.
pub(super) async fn connect(
    ctx: &Ctx,
    dial_args: &DialArgs,
    on_conflict: OnConflict,
) -> Result<(Arc<FileTransfer>, Dialed)> {
    let ft = FileTransfer::new(TransferConfig {
        download_conflict: on_conflict.policy(),
        ..TransferConfig::default()
    });
    let dialed = dial(ctx, dial_args, FeatureSet::new().with(ft.clone())).await?;
    ctx.out.event(
        text::connected_host(dialed.session.peer(), dialed.session.permissions()),
        &json!({ "type": "connected", "peer": peer_json(dialed.session.peer()) }),
    );
    if !dialed.session.permissions().contains(Permissions::FILES) {
        dialed.close().await;
        return Err(FilesError::PermissionDenied.into());
    }
    Ok((ft, dialed))
}

/// Prints progress until transfer `id` finishes; Ctrl+C cancels it.
pub(super) async fn watch(
    ctx: &Ctx,
    ft: &FileTransfer,
    dialed: &mut Dialed,
    id: TransferId,
    sub: &mut broadcast::Receiver<(u64, TransferEvent)>,
) -> Result<()> {
    let out = ctx.out;
    let start = Instant::now();
    let mut last_line = Instant::now() - Duration::from_secs(1);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                ft.cancel(id);
                return Err(CliError::Interrupted.into());
            }
            ev = dialed.events.recv() => {
                if matches!(ev, Some(ViewerEvent::Ended(_)) | None) {
                    bail!(FilesError::Remote(FsError::Other("session ended".into())));
                }
            }
            ev = sub.recv() => {
                let ev = match ev {
                    Ok((_, ev)) if ev.id() == id => ev,
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => bail!("transfer events closed"),
                };
                match ev {
                    TransferEvent::Started(info) => out.event(
                        text::transfer_started(
                            &info.label,
                            info.total_files,
                            info.total_bytes,
                            info.direction == TransferDirection::Upload,
                        ),
                        &json!({
                            "type": "transfer_started",
                            "label": info.label,
                            "files": info.total_files,
                            "folders": info.total_dirs,
                            "bytes": info.total_bytes,
                            "source": info.source_dir,
                            "destination": info.dest_dir,
                        }),
                    ),
                    TransferEvent::Progress { bytes_done, total_bytes, files_done, total_files, bytes_per_sec, .. } => {
                        // At most one line per second for people; every update for --json.
                        if out.json || last_line.elapsed() >= Duration::from_secs(1) {
                            last_line = Instant::now();
                            out.event(
                                text::transfer_progress(bytes_done, total_bytes, bytes_per_sec, files_done, total_files),
                                &json!({
                                    "type": "progress",
                                    "bytes_done": bytes_done,
                                    "bytes_total": total_bytes,
                                    "files_done": files_done,
                                    "files_total": total_files,
                                    "bytes_per_sec": bytes_per_sec,
                                }),
                            );
                        }
                    }
                    TransferEvent::ItemFailed { rel_path, error, .. } => {
                        let e = format!("{error:?}");
                        out.event(
                            text::transfer_item_failed(&rel_path, &e),
                            &json!({ "type": "item_failed", "path": rel_path, "error": e }),
                        );
                    }
                    // The command line never asks (its policy is fixed by `--on-conflict`).
                    TransferEvent::ConflictsFound(_) | TransferEvent::ConflictsResolved(_) => {}
                    TransferEvent::Finished { state, failed_items, local_dest, .. } => {
                        return finish(ctx, ft, id, start, state, failed_items, local_dest);
                    }
                }
            }
        }
    }
}

fn finish(
    ctx: &Ctx,
    ft: &FileTransfer,
    id: TransferId,
    start: Instant,
    state: TransferState,
    failed: u32,
    local_dest: Option<PathBuf>,
) -> Result<()> {
    let snap = ft.snapshot(id);
    let (files, bytes, dest) = snap.as_ref().map_or((0, 0, String::new()), |s| {
        (
            s.info.total_files,
            s.info.total_bytes,
            s.info.dest_dir.clone(),
        )
    });
    let dest = local_dest.map_or(dest, |d| d.display().to_string());
    match state {
        TransferState::Completed => {
            let secs = start.elapsed().as_secs_f64();
            ctx.out.result(
                text::transfer_done(files.saturating_sub(failed), bytes, secs, failed, &dest),
                &json!({
                    "type": "transfer_done",
                    "files": files,
                    "failed": failed,
                    "bytes": bytes,
                    "seconds": secs,
                    "destination": dest,
                }),
            );
            if failed > 0 {
                bail!(text::transfer_items_failed(failed));
            }
            Ok(())
        }
        TransferState::Failed(FsError::PermissionDenied) => {
            Err(FilesError::PermissionDenied.into())
        }
        TransferState::Failed(e) => {
            ctx.out.progress(text::transfer_stopped(&format!("{e:?}")));
            Err(FilesError::Remote(e).into())
        }
        other => {
            ctx.out
                .progress(text::transfer_stopped(&format!("{other:?}")));
            Err(FilesError::Cancelled.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::commands::{Cli, Cmd};
    use crate::text::{self, Lang};

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("TLP-Desk-cli").chain(args.iter().copied()))
    }

    #[test]
    fn send_file_parses_paths_destination_and_auth() {
        let cli = parse(&[
            "send-file",
            "123456789",
            "C:\\بيانات\\تقرير.pdf",
            "D:\\مجلد",
            "--to",
            "C:\\Users\\مستخدم\\Desktop",
            "--code",
            "123456",
        ])
        .unwrap();
        let Cmd::SendFile(a) = cli.cmd else {
            panic!("not send-file")
        };
        assert_eq!(a.dial.target, "123456789");
        assert_eq!(a.paths.len(), 2);
        assert_eq!(a.to.as_deref(), Some("C:\\Users\\مستخدم\\Desktop"));
        assert_eq!(a.dial.auth.code.as_deref(), Some("123456"));
        assert!(
            parse(&["send-file", "123456789"]).is_err(),
            "paths are required"
        );
    }

    #[test]
    fn get_file_defaults_to_the_current_folder() {
        let cli = parse(&["get-file", "10.0.0.2", "C:\\مجلد المشروع"]).unwrap();
        let Cmd::GetFile(a) = cli.cmd else {
            panic!("not get-file")
        };
        assert_eq!(a.remote_paths, ["C:\\مجلد المشروع"]);
        assert_eq!(a.to, std::path::PathBuf::from("."));
    }

    #[test]
    fn progress_text_in_both_languages() {
        assert_eq!(text::bytes(512), "512 B");
        assert_eq!(text::bytes(3 << 30), "3.0 GiB");
        let en = text::with_lang(Lang::En, || {
            text::transfer_progress(1 << 30, 2 << 30, 100 << 20, 3, 7)
        });
        assert_eq!(
            en,
            " 50%  1.0 GiB / 2.0 GiB  100.0 MiB/s  ETA 0:10  files 3/7"
        );
        let ar = text::with_lang(Lang::Ar, || {
            text::transfer_progress(1 << 30, 2 << 30, 0, 3, 7)
        });
        assert!(
            ar.contains("الملفات 3/7") && ar.contains("المتبقي -"),
            "{ar}"
        );
        let name = "ملف تجريبي.txt";
        let ar = text::with_lang(Lang::Ar, || text::transfer_started(name, 1, 10, true));
        assert!(ar.contains(name));
    }
}
