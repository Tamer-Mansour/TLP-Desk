//! Update UI (TLP-updater): the Settings "Updates" card, the "Update available" window and
//! the home-screen notice about a newer version on the LAN. Logic lives in `crate::update`.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, RichText};

use super::icons::{self, Icon};
use super::{theme, widgets};
use crate::app::App;
use crate::backend::DiscoveredDevice;
use crate::i18n::{Tx, S};
use crate::update::{self, CheckConfig, UpdateError, UpdatePhase, UpdateWorker};

/// Seconds between automatic checks when "Check daily" is on.
const DAY_SECS: u64 = 24 * 60 * 60;
/// How often the daily-check timer is looked at.
const DAILY_POLL: Duration = Duration::from_secs(60);

/// Transient update state of the app window.
#[derive(Debug, Default)]
pub struct UpdateUi {
    worker: Option<UpdateWorker>,
    /// The user started the current step (errors are shown); automatic checks fail silently.
    interactive: bool,
    /// The newer LAN version the user hid with "Hide".
    lan_dismissed: Option<String>,
    last_daily_poll: Option<Instant>,
}

impl UpdateUi {
    fn worker(&mut self, ctx: &egui::Context) -> &UpdateWorker {
        self.worker.get_or_insert_with(|| {
            let ctx = ctx.clone();
            UpdateWorker::new(Arc::new(move || ctx.request_repaint()))
        })
    }

    /// Current phase (Idle before anything ran).
    pub fn phase(&self) -> UpdatePhase {
        self.worker
            .as_ref()
            .map(UpdateWorker::phase)
            .unwrap_or_default()
    }

    /// Test/README hook: show a given state.
    pub fn set_phase(&mut self, ctx: &egui::Context, p: UpdatePhase, interactive: bool) {
        self.interactive = interactive;
        self.worker(ctx).set_phase(p);
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `true` when a daily check is due.
pub fn daily_check_due(check_daily: bool, last_check_unix: u64, now_unix: u64) -> bool {
    check_daily && now_unix.saturating_sub(last_check_unix) >= DAY_SECS
}

/// Display text for a failure (fail-closed messages for anything verification related).
fn error_text(tx: Tx, e: &UpdateError) -> String {
    let key = match e {
        UpdateError::Network(_) | UpdateError::Status(_) => S::UpdErrNetwork,
        UpdateError::BadSignature | UpdateError::NotInManifest | UpdateError::HashMismatch => {
            S::UpdErrVerify
        }
        UpdateError::NoInstaller => S::UpdErrNoInstaller,
        UpdateError::Launch(_) => S::UpdErrLaunch,
        _ => S::UpdErrOther,
    };
    tx.f(S::UpdFailed, &[("detail", tx.raw(key))])
}

/// The newer version running on a LAN peer, if any: `(version, device name)`.
pub fn lan_newer(devices: &[DiscoveredDevice]) -> Option<(String, String)> {
    update::version::newest_peer(
        devices
            .iter()
            .map(|d| (d.name.as_str(), d.app_version.as_deref())),
        &update::version::current(),
    )
    .map(|(v, name)| (v.to_string(), name))
}

impl App {
    fn update_cfg(&self) -> CheckConfig {
        CheckConfig::github(self.settings.updates.include_prereleases)
    }

    /// Per frame: daily check timer, the "Update available" window, exit after launch.
    pub(crate) fn update_frame(&mut self, ctx: &egui::Context) {
        let poll_due = self
            .update
            .last_daily_poll
            .is_none_or(|t| t.elapsed() >= DAILY_POLL);
        if poll_due {
            self.update.last_daily_poll = Some(Instant::now());
            let u = &self.settings.updates;
            if daily_check_due(u.check_daily, u.last_check_unix, now_unix())
                && matches!(
                    self.update.phase(),
                    UpdatePhase::Idle | UpdatePhase::UpToDate
                )
            {
                self.settings.updates.last_check_unix = now_unix();
                self.settings.save();
                self.update.interactive = false;
                let cfg = self.update_cfg();
                self.update.worker(ctx).check(cfg);
            }
        }
        match self.update.phase() {
            UpdatePhase::Launched => {
                // The installer runs elevated now; leave so it can replace the files.
                let cmds = self.tray.shell.quit();
                crate::tray::glue::apply_window_cmds(ctx, &cmds);
            }
            UpdatePhase::Failed(_) if !self.update.interactive => {
                self.update.worker(ctx).dismiss();
            }
            _ => {}
        }
        if self.session.is_none() {
            self.update_window(ctx);
        }
    }

    fn update_window(&mut self, ctx: &egui::Context) {
        let tx = self.tx;
        let phase = self.update.phase();
        let show = match &phase {
            UpdatePhase::Available(_)
            | UpdatePhase::Downloading { .. }
            | UpdatePhase::Verified { .. } => true,
            UpdatePhase::Failed(_) => self.update.interactive,
            _ => false,
        };
        if !show {
            return;
        }
        let mut later = false;
        let mut download = None;
        let mut install = None;
        let mut cancel = false;
        let mut open_page = None;
        egui::Window::new(tx.t(S::UpdAvailableTitle))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .default_width(460.0)
            .show(ctx, |ui| {
                theme::column(ui, tx.rtl(), |ui| match &phase {
                    UpdatePhase::Available(rel) => {
                        let cur = update::version::current().to_string();
                        ui.label(
                            RichText::new(tx.f(
                                S::UpdAvailable,
                                &[
                                    ("ver", &tx.digits(&rel.version.to_string())),
                                    ("cur", &tx.digits(&cur)),
                                ],
                            ))
                            .strong(),
                        );
                        if !rel.notes.is_empty() {
                            ui.add_space(6.0);
                            ui.label(theme::caption(ui, tx.t(S::UpdNotes)));
                            egui::ScrollArea::vertical()
                                .max_height(180.0)
                                .show(ui, |ui| {
                                    for line in rel.notes.lines() {
                                        ui.label(tx.d(line));
                                    }
                                });
                        }
                        ui.add_space(10.0);
                        theme::row(ui, tx.rtl(), |ui| {
                            if rel.files.is_some() {
                                if ui
                                    .add(widgets::primary_icon_button(
                                        ui,
                                        tx,
                                        Icon::Download,
                                        tx.t(S::UpdInstall),
                                    ))
                                    .clicked()
                                {
                                    download = Some(rel.clone());
                                }
                            } else if ui
                                .add(widgets::primary_icon_button(
                                    ui,
                                    tx,
                                    Icon::Globe,
                                    tx.t(S::UpdOpenPage),
                                ))
                                .clicked()
                            {
                                open_page = Some(rel.html_url.clone());
                            }
                            if ui.button(tx.t(S::UpdLater)).clicked() {
                                later = true;
                            }
                        });
                    }
                    UpdatePhase::Downloading { done, total, .. } => {
                        let frac = if *total > 0 {
                            (*done as f64 / *total as f64).clamp(0.0, 1.0) as f32
                        } else {
                            0.0
                        };
                        let pct = if *total > 0 {
                            format!("{}%", (frac * 100.0).round() as u32)
                        } else {
                            format!("{} MB", done >> 20)
                        };
                        ui.label(tx.f(S::UpdDownloading, &[("pct", &tx.digits(&pct))]));
                        ui.add(egui::ProgressBar::new(frac));
                        if ui.button(tx.t(S::UpdCancel)).clicked() {
                            cancel = true;
                        }
                    }
                    UpdatePhase::Verified { installer, .. } => {
                        ui.label(tx.t(S::UpdVerified));
                        ui.add_space(10.0);
                        theme::row(ui, tx.rtl(), |ui| {
                            if ui
                                .add(widgets::primary_icon_button(
                                    ui,
                                    tx,
                                    Icon::ShieldCheck,
                                    tx.t(S::UpdInstallNow),
                                ))
                                .clicked()
                            {
                                install = Some(installer.clone());
                            }
                            if ui.button(tx.t(S::UpdLater)).clicked() {
                                later = true;
                            }
                        });
                    }
                    UpdatePhase::Failed(e) => {
                        ui.label(
                            RichText::new(error_text(tx, e))
                                .color(theme::danger(ui.visuals().dark_mode)),
                        );
                        ui.add_space(10.0);
                        theme::row(ui, tx.rtl(), |ui| {
                            if ui
                                .add(icons::icon_text_button(
                                    tx,
                                    Icon::Globe,
                                    tx.t(S::UpdOpenPage),
                                ))
                                .clicked()
                            {
                                open_page = Some(update::RELEASES_PAGE.to_owned());
                            }
                            if ui.button(tx.t(S::UpdLater)).clicked() {
                                later = true;
                            }
                        });
                    }
                    _ => {}
                });
            });
        let cfg = self.update_cfg();
        let worker = self.update.worker(ctx);
        if let Some(rel) = download {
            worker.download(cfg.clone(), rel);
        }
        if let Some(path) = install {
            worker.install(&cfg, &path);
        }
        if cancel {
            worker.cancel();
        }
        if later {
            worker.dismiss();
        }
        if let Some(url) = open_page {
            if let Err(e) = update::install::open_release_page(&url) {
                tracing::warn!(%e, "release page not opened");
            }
        }
    }

    /// Settings → Updates.
    pub(crate) fn updates_card(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let dark = ui.visuals().dark_mode;
        let phase = self.update.phase();
        let mut check = false;
        let mut changed = false;
        theme::card(ui, |ui| {
            theme::column(ui, tx.rtl(), |ui| {
                widgets::icon_heading(ui, tx, Icon::Download, tx.t(S::UpdSection));
                ui.add_space(6.0);
                let cur = update::version::current().to_string();
                ui.label(tx.f(S::UpdInstalled, &[("ver", &tx.digits(&cur))]));
                ui.add_space(4.0);
                theme::row(ui, tx.rtl(), |ui| {
                    if ui
                        .add_enabled(
                            !phase.busy(),
                            icons::icon_text_button(tx, Icon::Refresh, tx.t(S::UpdCheckNow)),
                        )
                        .clicked()
                    {
                        check = true;
                    }
                    match &phase {
                        UpdatePhase::Checking => {
                            ui.spinner();
                            ui.label(tx.t(S::UpdChecking));
                        }
                        UpdatePhase::UpToDate => {
                            ui.label(
                                RichText::new(tx.t(S::UpdUpToDate)).color(theme::accent(dark)),
                            );
                        }
                        _ => {}
                    }
                });
                changed |= super::settings::check_row(
                    ui,
                    tx,
                    &mut self.settings.updates.check_daily,
                    tx.t(S::UpdCheckDaily),
                );
                changed |= super::settings::check_row(
                    ui,
                    tx,
                    &mut self.settings.updates.include_prereleases,
                    tx.t(S::UpdIncludePre),
                );
                ui.label(theme::caption(ui, tx.t(S::UpdNote)));
            });
        });
        if changed {
            self.settings.save();
        }
        if check {
            self.update.interactive = true;
            let cfg = self.update_cfg();
            self.update.worker(ui.ctx()).check(cfg);
        }
    }

    /// Home screen: "A newer version (x) is running on <device> — Download".
    pub(crate) fn lan_update_notice(&mut self, ui: &mut egui::Ui) {
        let tx = self.tx;
        let Some((ver, device)) = self.devices.as_deref().and_then(lan_newer) else {
            return;
        };
        if self.update.lan_dismissed.as_deref() == Some(ver.as_str()) {
            return;
        }
        let mut hide = false;
        theme::card(ui, |ui| {
            theme::row(ui, tx.rtl(), |ui| {
                ui.label(tx.f(
                    S::UpdLanNotice,
                    &[("ver", &tx.digits(&ver)), ("device", &device)],
                ));
                if ui
                    .add(icons::icon_text_button(
                        tx,
                        Icon::Download,
                        tx.t(S::UpdDownload),
                    ))
                    .clicked()
                {
                    if let Err(e) = update::install::open_release_page(update::RELEASES_PAGE) {
                        tracing::warn!(%e, "release page not opened");
                    }
                }
                if ui.small_button(tx.t(S::UpdDismiss)).clicked() {
                    hide = true;
                }
            });
        });
        ui.add_space(theme::space::S);
        if hide {
            self.update.lan_dismissed = Some(ver);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::DeviceId;

    fn dev(name: &str, ver: Option<&str>) -> DiscoveredDevice {
        DiscoveredDevice {
            device_id: DeviceId(123_456_789),
            name: name.into(),
            address: "192.168.1.5".parse().unwrap(),
            online: true,
            app_version: ver.map(Into::into),
        }
    }

    #[test]
    fn daily_check_is_off_by_default_and_due_after_a_day() {
        let s = crate::settings::Settings::default();
        assert!(!s.updates.check_daily, "daily check must default to OFF");
        assert!(s.updates.include_prereleases);
        assert!(!daily_check_due(s.updates.check_daily, 0, 10 * DAY_SECS));
        assert!(daily_check_due(true, 0, DAY_SECS));
        assert!(!daily_check_due(true, DAY_SECS, DAY_SECS + 5));
        // A clock that went backwards never underflows.
        assert!(!daily_check_due(true, 5 * DAY_SECS, DAY_SECS));
    }

    #[test]
    fn lan_notice_finds_a_newer_arabic_named_peer() {
        assert!(lan_newer(&[dev("قديم", Some("0.1.0")), dev("x", None)]).is_none());
        let (ver, name) = lan_newer(&[
            dev("قديم", Some("0.1.0")),
            dev("حاسوب المدير", Some("99.0.0")),
        ])
        .unwrap();
        assert_eq!(ver, "99.0.0");
        assert_eq!(name, "حاسوب المدير");
    }

    #[test]
    fn errors_map_to_fail_closed_messages_in_both_languages() {
        use crate::i18n::{DigitStyle, Lang};
        for lang in [Lang::En, Lang::Ar] {
            let tx = Tx::new(lang, DigitStyle::Western);
            let verify = error_text(tx, &UpdateError::HashMismatch);
            assert_eq!(verify, error_text(tx, &UpdateError::BadSignature));
            assert_ne!(verify, error_text(tx, &UpdateError::Network("x".into())));
        }
    }
}
