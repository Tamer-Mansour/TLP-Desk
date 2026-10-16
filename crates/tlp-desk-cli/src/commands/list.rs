//! `list`: discover TLP-Desk devices on the LAN.

use std::time::Duration;

use anyhow::Result;
use clap::Args;
use serde_json::{json, Value};
use tlp_desk_net::{DiscoveredDevice, Discovery, DiscoveryConfig};

use super::common::{id_str, Ctx};
use crate::text;

/// Arguments of `list`.
#[derive(Debug, Clone, Args)]
pub struct ListArgs {
    /// How long to listen for announcements.
    #[arg(long, default_value_t = 5)]
    pub seconds: u64,
}

/// Runs `list`.
pub async fn run(ctx: &Ctx, args: &ListArgs) -> Result<()> {
    ctx.out.progress(text::searching(args.seconds));
    let disc = Discovery::start(DiscoveryConfig::default()).await?;
    disc.refresh().await;
    tokio::time::sleep(Duration::from_secs(args.seconds)).await;
    let devices = disc.devices();
    let human = if devices.is_empty() {
        text::no_devices().to_owned()
    } else {
        devices
            .iter()
            .map(text::device_row)
            .collect::<Vec<_>>()
            .join("\n")
    };
    ctx.out.result(human, &devices_json(&devices));
    Ok(())
}

/// `{"type":"devices","devices":[{device_id,name,addrs,protocol_version,via}]}`.
pub fn devices_json(devices: &[DiscoveredDevice]) -> Value {
    let list: Vec<Value> = devices
        .iter()
        .map(|d| {
            json!({
                "device_id": id_str(d.device_id),
                "name": d.name,
                "addrs": d.addrs.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "protocol_version": d.protocol_version,
                "via": text::discovered_via(d),
            })
        })
        .collect();
    json!({ "type": "devices", "devices": list })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_is_still_an_object() {
        assert_eq!(
            devices_json(&[]).to_string(),
            r#"{"devices":[],"type":"devices"}"#
        );
    }

    #[test]
    fn device_schema_with_arabic_name() {
        let d = DiscoveredDevice {
            device_id: "123 456 789".parse().unwrap(),
            name: "حاسوب المحاسبة".into(),
            addrs: vec!["192.168.1.20:47400".parse().unwrap()],
            protocol_version: 4,
            app_version: Some("1.0.0".into()),
            via_mdns: true,
            via_broadcast: false,
            last_seen: std::time::Instant::now(),
        };
        let v = devices_json(&[d]);
        assert_eq!(
            v["devices"][0],
            json!({
                "device_id": "123456789",
                "name": "حاسوب المحاسبة",
                "addrs": ["192.168.1.20:47400"],
                "protocol_version": 4,
                "via": "mdns",
            })
        );
    }
}
