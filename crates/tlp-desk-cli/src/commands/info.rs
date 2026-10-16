//! `info <target>`: connect, ask the host for its device info (OS, CPU, RAM, disks, uptime,
//! host name), print it and disconnect. Needs the `VIEW` permission on the host.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Args;
use serde_json::{json, Value};
use tlp_desk_net::FeatureSet;
use tlp_desk_proto::Permissions;
use tlp_desk_viewer::{
    request_device_info, DeviceInfo, DeviceInfoClient, ViewerEvent, ViewerEvents,
    DEVICE_INFO_FEATURE,
};

use super::common::{dial, peer_json, Ctx, DialArgs};
use crate::exit::CliError;
use crate::text;

/// Arguments of `info`.
#[derive(Debug, Clone, Args)]
pub struct InfoArgs {
    #[command(flatten)]
    pub dial: DialArgs,
    /// Give up if the host has not answered after this many seconds (once connected).
    #[arg(long, default_value_t = 10)]
    pub timeout: u64,
}

/// Runs `info`.
pub async fn run(ctx: &Ctx, args: &InfoArgs) -> Result<()> {
    let features = FeatureSet::new().with(Arc::new(DeviceInfoClient));
    let mut dialed = dial(ctx, &args.dial, features).await?;
    let result = ask(ctx, &mut dialed.events, &dialed.session, args.timeout).await;
    if let Ok(info) = &result {
        ctx.out.result(
            text::device_info(info),
            &device_info_json(info, Some(peer_json(dialed.session.peer()))),
        );
    }
    dialed.session.disconnect();
    dialed.close().await;
    result.map(|_| ())
}

async fn ask(
    ctx: &Ctx,
    events: &mut ViewerEvents,
    session: &tlp_desk_viewer::ViewerSession,
    timeout_s: u64,
) -> Result<DeviceInfo> {
    if !session.permissions().contains(Permissions::VIEW) {
        return Err(CliError::PermissionDenied("view").into());
    }
    ctx.out.progress(text::requesting_device_info());
    if !request_device_info(session) {
        return Err(CliError::SessionEnded("closed".into()).into());
    }
    let wait = wait_for_info(events);
    tokio::select! {
        r = tokio::time::timeout(Duration::from_secs(timeout_s), wait) => {
            r.map_err(|_| CliError::Timeout(timeout_s))?
        }
        _ = tokio::signal::ctrl_c() => Err(CliError::Interrupted.into()),
    }
}

/// The first device-info answer on `events`.
pub async fn wait_for_info(events: &mut ViewerEvents) -> Result<DeviceInfo> {
    loop {
        match events.recv().await {
            Some(ViewerEvent::Feature(e)) if e.feature == DEVICE_INFO_FEATURE => {
                if let Some(info) = e.payload::<DeviceInfo>() {
                    return Ok(info.clone());
                }
            }
            Some(ViewerEvent::Ended(r)) => {
                return Err(CliError::SessionEnded(format!("{r:?}")).into())
            }
            Some(_) => {}
            None => return Err(CliError::SessionEnded("closed".into()).into()),
        }
    }
}

/// `{"type":"device_info","peer"?,"os","version","cpu","cores","ram_bytes","disks":[...],
/// "uptime_s","hostname"}`.
pub fn device_info_json(info: &DeviceInfo, peer: Option<Value>) -> Value {
    let disks: Vec<Value> = info
        .disks
        .iter()
        .map(|d| {
            json!({
                "mount": d.mount,
                "label": d.label,
                "total_bytes": d.total_bytes,
                "free_bytes": d.free_bytes,
            })
        })
        .collect();
    let mut v = json!({
        "type": "device_info",
        "os": info.os,
        "version": info.version,
        "cpu": info.cpu,
        "cores": info.cores,
        "ram_bytes": info.ram_bytes,
        "disks": disks,
        "uptime_s": info.uptime_s,
        "hostname": info.hostname,
    });
    if let (Some(peer), Some(obj)) = (peer, v.as_object_mut()) {
        obj.insert("peer".into(), peer);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_viewer::DiskInfo;

    #[tokio::test]
    async fn waits_for_the_device_info_event_and_skips_others() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let info = DeviceInfo {
            os: "Windows 11".into(),
            version: String::new(),
            cpu: String::new(),
            cores: 1,
            ram_bytes: 1,
            disks: vec![],
            uptime_s: 1,
            hostname: "مكتب".into(),
        };
        tx.send(ViewerEvent::HostInputPriority(true)).unwrap();
        tx.send(ViewerEvent::Feature(tlp_desk_viewer::FeatureEvent::new(
            "chat",
            0,
            String::from("x"),
        )))
        .unwrap();
        tx.send(ViewerEvent::Feature(tlp_desk_viewer::FeatureEvent::new(
            DEVICE_INFO_FEATURE,
            0,
            info.clone(),
        )))
        .unwrap();
        assert_eq!(wait_for_info(&mut rx).await.unwrap(), info);
    }

    #[tokio::test]
    async fn session_end_before_answer_is_an_error() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(ViewerEvent::Ended(tlp_desk_viewer::EndReason::Local))
            .unwrap();
        let err = wait_for_info(&mut rx).await.unwrap_err();
        assert_eq!(crate::exit::Exit::of(&err), crate::exit::Exit::Failure);
        drop(tx);
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel::<ViewerEvent>();
        drop(tx2);
        assert!(wait_for_info(&mut rx2).await.is_err());
    }

    #[test]
    fn device_info_json_schema() {
        let info = DeviceInfo {
            os: "Windows 11 Pro".into(),
            version: "10.0.26100".into(),
            cpu: "Test CPU".into(),
            cores: 8,
            ram_bytes: 17_179_869_184,
            disks: vec![DiskInfo {
                mount: "C:\\".into(),
                label: "النظام".into(),
                total_bytes: 100,
                free_bytes: 40,
            }],
            uptime_s: 3600,
            hostname: "حاسوب-المكتب".into(),
        };
        let v = device_info_json(&info, None);
        assert_eq!(
            v,
            json!({
                "type": "device_info",
                "os": "Windows 11 Pro",
                "version": "10.0.26100",
                "cpu": "Test CPU",
                "cores": 8,
                "ram_bytes": 17_179_869_184u64,
                "disks": [{"mount": "C:\\", "label": "النظام", "total_bytes": 100, "free_bytes": 40}],
                "uptime_s": 3600,
                "hostname": "حاسوب-المكتب",
            })
        );
        let with_peer = device_info_json(&info, Some(json!({"device_id": "123456789"})));
        assert_eq!(with_peer["peer"]["device_id"], "123456789");
    }
}
