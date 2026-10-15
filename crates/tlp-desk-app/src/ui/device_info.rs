//! Remote device-info panel: the other PC's computer name, OS, CPU, memory, uptime and disks,
//! opened from the session toolbar ([`Icon::DeviceInfo`]) and refreshed on demand.
//!
//! Values are formatted with the shared helpers (`format::number`, `format::file_size`), so
//! the user's digit style and Arabic units apply; names from the network go through `Tx::p`.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use super::icons::{self, Icon};
use super::theme;
use crate::backend::{Backend, DeviceInfo, SessionId};
use crate::i18n::{format, Tx, S};

/// Show "no answer" if the host stays silent this long (e.g. an older version).
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-session state of the panel.
#[derive(Debug, Default)]
pub struct DeviceInfoPanel {
    /// Whether the window is shown.
    pub open: bool,
    info: Option<DeviceInfo>,
    requested_at: Option<Instant>,
}

impl DeviceInfoPanel {
    /// An answer arrived.
    pub fn set_info(&mut self, info: DeviceInfo) {
        self.info = Some(info);
        self.requested_at = None;
    }

    /// The last answer, if any.
    pub fn info(&self) -> Option<&DeviceInfo> {
        self.info.as_ref()
    }

    /// Whether a request is in flight.
    pub fn pending(&self) -> bool {
        self.requested_at.is_some()
    }

    fn request(&mut self, backend: &dyn Backend, session: SessionId) {
        self.requested_at = Some(Instant::now());
        backend.request_device_info(session);
    }

    /// Opens the panel (asking the host on first open) or closes it.
    pub fn toggle(&mut self, backend: &dyn Backend, session: SessionId) {
        self.open = !self.open;
        if self.open && self.info.is_none() && !self.pending() {
            self.request(backend, session);
        }
    }

    /// The toolbar toggle button.
    pub fn toolbar_button(
        &mut self,
        ui: &mut egui::Ui,
        tx: Tx,
        backend: &dyn Backend,
        session: SessionId,
    ) {
        if icons::icon_toggle(
            ui,
            tx,
            self.open,
            Icon::DeviceInfo,
            tx.t(S::DeviceInfoTitle),
        )
        .clicked()
        {
            self.toggle(backend, session);
        }
    }

    /// The floating window (call every frame; draws nothing while closed).
    pub fn window(
        &mut self,
        ctx: &egui::Context,
        tx: Tx,
        backend: &dyn Backend,
        session: SessionId,
    ) {
        if !self.open {
            return;
        }
        let mut open = true;
        let mut refresh = false;
        egui::Window::new(tx.t(S::DeviceInfoTitle))
            .id(egui::Id::new(("device-info", session.0)))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .show(ctx, |ui| {
                theme::column(ui, tx.rtl(), |ui| {
                    let busy = self.pending();
                    let b = icons::icon_text_button(tx, Icon::Refresh, tx.t(S::DeviceInfoRefresh));
                    if ui.add_enabled(!busy, b).clicked() {
                        refresh = true;
                    }
                    ui.add_space(6.0);
                    if let Some(at) = self.requested_at {
                        if at.elapsed() > ANSWER_TIMEOUT {
                            ui.label(RichText::new(tx.t(S::DeviceInfoNoAnswer)).weak());
                        } else {
                            theme::row(ui, tx.rtl(), |ui| {
                                ui.spinner();
                                ui.label(tx.t(S::DeviceInfoLoading));
                            });
                            ctx.request_repaint_after(Duration::from_millis(250));
                        }
                    }
                    if let Some(info) = &self.info {
                        for (key, value) in rows(tx, info) {
                            ui.label(tx.p(&format!("{}: {}", tx.raw(key), value)));
                        }
                        ui.add_space(6.0);
                        ui.label(RichText::new(tx.t(S::DeviceInfoDisks)).strong());
                        for disk in disks(tx, info) {
                            ui.label(tx.p(&disk.title));
                            ui.add(egui::ProgressBar::new(disk.used).text(tx.p(&disk.free_text)));
                        }
                    }
                });
            });
        if refresh {
            self.request(backend, session);
        }
        if !open {
            self.open = false;
        }
    }
}

/// `(label, value)` lines in logical order (shape with `Tx::p` after joining).
pub fn rows(tx: Tx, info: &DeviceInfo) -> Vec<(S, String)> {
    let d = tx.digits;
    vec![
        (S::DeviceInfoComputer, info.hostname.clone()),
        (S::DeviceInfoSystem, info.os.clone()),
        (S::DeviceInfoVersion, tx.digits(&info.version)),
        (S::DeviceInfoCpu, info.cpu.clone()),
        (
            S::DeviceInfoThreads,
            format::number(f64::from(info.cores), 0, d),
        ),
        (
            S::DeviceInfoMemory,
            format::file_size(info.ram_bytes, tx.lang, d),
        ),
        (S::DeviceInfoUptime, uptime(tx, info.uptime_s)),
    ]
}

/// One disk line: title (`C:\ "label"`), used fraction for the bar, and "x free of y".
#[derive(Debug, Clone, PartialEq)]
pub struct DiskLine {
    pub title: String,
    pub used: f32,
    pub free_text: String,
}

/// Disk lines in logical order.
pub fn disks(tx: Tx, info: &DeviceInfo) -> Vec<DiskLine> {
    info.disks
        .iter()
        .map(|disk| {
            let title = if disk.label.is_empty() {
                disk.mount.clone()
            } else {
                format!("{} \"{}\"", disk.mount, disk.label)
            };
            let free = disk.free_bytes.min(disk.total_bytes);
            let used = if disk.total_bytes == 0 {
                0.0
            } else {
                (disk.total_bytes - free) as f64 / disk.total_bytes as f64
            };
            let free_text = crate::i18n::tr_raw(tx.lang, S::DeviceInfoDiskFree)
                .replace("{free}", &format::file_size(free, tx.lang, tx.digits))
                .replace(
                    "{total}",
                    &format::file_size(disk.total_bytes, tx.lang, tx.digits),
                );
            DiskLine {
                title,
                used: used as f32,
                free_text,
            }
        })
        .collect()
}

/// Uptime as "3 d 5 h 42 min" / "3 ي 5 س 42 د" (logical order, user's digits).
pub fn uptime(tx: Tx, secs: u64) -> String {
    let n = |v: u64| format::number(v as f64, 0, tx.digits);
    let (days, hours, mins) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    let (key, args) = if days > 0 {
        (
            S::UptimeDays,
            vec![("d", n(days)), ("h", n(hours)), ("m", n(mins))],
        )
    } else if hours > 0 {
        (S::UptimeHours, vec![("h", n(hours)), ("m", n(mins))])
    } else {
        (S::UptimeMinutes, vec![("m", n(mins))])
    };
    let mut s = tx.raw(key).to_owned();
    for (name, value) in args {
        s = s.replace(&format!("{{{name}}}"), &value);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::device_info::fake_info;
    use crate::backend::DiskInfo;
    use crate::i18n::{DigitStyle, Lang};

    fn en() -> Tx {
        Tx::new(Lang::En, DigitStyle::Western)
    }

    fn ar() -> Tx {
        Tx::new(Lang::Ar, DigitStyle::ArabicIndic)
    }

    #[test]
    fn uptime_in_both_languages_and_digit_styles() {
        assert_eq!(
            uptime(en(), 3 * 86_400 + 5 * 3600 + 42 * 60),
            "3 d 5 h 42 min"
        );
        assert_eq!(uptime(en(), 3_660), "1 h 1 min");
        assert_eq!(uptime(en(), 59), "0 min");
        assert_eq!(
            uptime(ar(), 3 * 86_400 + 5 * 3600 + 42 * 60),
            "٣ ي ٥ س ٤٢ د"
        );
    }

    #[test]
    fn rows_keep_arabic_names_and_localize_numbers() {
        let info = fake_info();
        let r = rows(ar(), &info);
        assert_eq!(r[0], (S::DeviceInfoComputer, "حاسوب-المحاسبة".to_owned()));
        let threads = &r
            .iter()
            .find(|(k, _)| *k == S::DeviceInfoThreads)
            .unwrap()
            .1;
        assert_eq!(threads, "٢٠");
        let memory = &r.iter().find(|(k, _)| *k == S::DeviceInfoMemory).unwrap().1;
        assert!(memory.contains("غ.ب"), "{memory}");
        let r = rows(en(), &info);
        let memory = &r.iter().find(|(k, _)| *k == S::DeviceInfoMemory).unwrap().1;
        assert_eq!(memory, "32.0 GB");
    }

    #[test]
    fn disk_lines_clamp_hostile_values() {
        let mut info = fake_info();
        info.disks.push(DiskInfo {
            mount: "E:\\".into(),
            label: String::new(),
            total_bytes: 0,
            free_bytes: 10,
        });
        info.disks.push(DiskInfo {
            mount: "F:\\".into(),
            label: "نسخ".into(),
            total_bytes: 100,
            free_bytes: 500,
        });
        let lines = disks(en(), &info);
        assert_eq!(lines[0].title, "C:\\ \"النظام\"");
        assert!(lines[0].used > 0.6 && lines[0].used < 0.7, "{:?}", lines[0]);
        assert_eq!(lines[0].free_text, "187.0 GB free of 511.0 GB");
        assert_eq!(lines[2].title, "E:\\");
        assert_eq!(lines[2].used, 0.0);
        assert_eq!(lines[3].used, 0.0);
        let ar_lines = disks(ar(), &info);
        assert!(ar_lines[0].free_text.contains("متاح من"));
    }

    #[test]
    fn panel_requests_on_first_open_only() {
        struct Count(std::sync::atomic::AtomicUsize);
        impl Backend for Count {
            fn local_info(&self) -> crate::backend::LocalInfo {
                crate::backend::LocalInfo {
                    device_id: crate::backend::DeviceId(123_456_789),
                    device_name: "اختبار".into(),
                    addresses: Vec::new(),
                    one_time_code: "000000".into(),
                }
            }
            fn set_device_name(&self, _: String) {}
            fn refresh_one_time_code(&self) {}
            fn connect(
                &self,
                _: crate::backend::ConnectTarget,
                _: Option<String>,
            ) -> crate::backend::AttemptId {
                crate::backend::AttemptId(0)
            }
            fn cancel_connect(&self, _: crate::backend::AttemptId) {}
            fn forget_host_key(&self, _: crate::backend::AttemptId) -> bool {
                false
            }
            fn respond_to_request(&self, _: crate::backend::RequestId, _: bool) {}
            fn send_input(&self, _: SessionId, _: tlp_desk_proto::InputEvent) {}
            fn set_view_only(&self, _: SessionId, _: bool) {}
            fn disconnect(&self, _: SessionId) {}
            fn select_monitor(&self, _: SessionId, _: u32) {}
            fn request_device_info(&self, _: SessionId) {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let b = Count(Default::default());
        let mut p = DeviceInfoPanel::default();
        p.toggle(&b, SessionId(1));
        assert!(p.open && p.pending());
        p.set_info(fake_info());
        assert!(!p.pending());
        p.toggle(&b, SessionId(1)); // close
        p.toggle(&b, SessionId(1)); // reopen: shows the cached info, no new request
        assert!(p.open);
        assert_eq!(b.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(p.info().map(|i| i.cores), Some(20));
    }
}
