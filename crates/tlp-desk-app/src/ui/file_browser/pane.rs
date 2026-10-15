//! One side of the file browser: navigation state, the sorted listing and its drawing.

use std::collections::BTreeSet;

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Ui};

use crate::backend::files::{FsEntry, FsFailure, FsRequestId, FsRoot, FsRootKind, FsSide};
use crate::i18n::{format, Tx, S};
use crate::ui::icons::{self, Icon};
use crate::ui::theme;

/// Height of one listing row, in points.
pub(super) const ROW_H: f32 = 22.0;
const SIZE_W: f32 = 84.0;
const DATE_W: f32 = 128.0;
const ICON_W: f32 = 22.0;
const PAD: f32 = 6.0;

/// Column the listing is sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SortKey {
    Name,
    Size,
    Modified,
}

/// What the user did in a pane this frame (handled by the browser).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PaneAction {
    /// Go to this folder (`None` = drives and folders).
    Navigate(Option<String>),
    Back,
    Forward,
    Refresh,
    NewFolder,
    /// Rename this entry (a name in the current folder).
    Rename(String),
    /// Delete these entries (names in the current folder).
    Delete(Vec<String>),
    /// Copy these entries to the other pane's folder.
    CopyToOther(Vec<String>),
}

/// Payload of a drag from one pane to the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DragFiles {
    /// Index of the pane the drag started in.
    pub from: usize,
    pub names: Vec<String>,
}

pub(super) struct Pane {
    pub side: FsSide,
    /// Current folder; `None` shows the drives and known folders.
    pub path: Option<String>,
    back: Vec<Option<String>>,
    forward: Vec<Option<String>>,
    pub entries: Vec<FsEntry>,
    pub roots: Vec<FsRoot>,
    pub selected: BTreeSet<String>,
    anchor: Option<String>,
    pub sort: SortKey,
    pub ascending: bool,
    /// The listing request in flight, if any.
    pub loading: Option<FsRequestId>,
    pub error: Option<FsFailure>,
    /// Whether the current folder was listed at least once (or a listing is pending).
    pub requested: bool,
}

impl Pane {
    pub fn new(side: FsSide) -> Self {
        Self {
            side,
            path: None,
            back: Vec::new(),
            forward: Vec::new(),
            entries: Vec::new(),
            roots: Vec::new(),
            selected: BTreeSet::new(),
            anchor: None,
            sort: SortKey::Name,
            ascending: true,
            loading: None,
            error: None,
            requested: false,
        }
    }

    /// Goes to `path`, remembering the current folder for Back.
    pub fn navigate(&mut self, path: Option<String>) {
        if path == self.path {
            self.requested = false;
            return;
        }
        let old = std::mem::replace(&mut self.path, path);
        self.back.push(old);
        self.forward.clear();
        self.reset_listing();
    }

    pub fn go_back(&mut self) {
        if let Some(p) = self.back.pop() {
            let cur = std::mem::replace(&mut self.path, p);
            self.forward.push(cur);
            self.reset_listing();
        }
    }

    pub fn go_forward(&mut self) {
        if let Some(p) = self.forward.pop() {
            let cur = std::mem::replace(&mut self.path, p);
            self.back.push(cur);
            self.reset_listing();
        }
    }

    fn reset_listing(&mut self) {
        self.entries.clear();
        self.selected.clear();
        self.anchor = None;
        self.error = None;
        self.requested = false;
    }

    /// Entries in display order: folders first, then by the sort column.
    pub fn sorted(&self) -> Vec<&FsEntry> {
        let mut v: Vec<&FsEntry> = self.entries.iter().collect();
        v.sort_by(|a, b| {
            let by = match self.sort {
                SortKey::Name => tlp_desk_files::fs::entry_order(a, b),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Modified => a.modified_unix_ms.cmp(&b.modified_unix_ms),
            };
            let by = if self.ascending { by } else { by.reverse() };
            // Folders stay on top whatever the column and direction.
            (!a.is_dir)
                .cmp(&!b.is_dir)
                .then(by)
                .then_with(|| a.name.cmp(&b.name))
        });
        v
    }

    /// Selected names in display order.
    pub fn selection(&self) -> Vec<String> {
        self.sorted()
            .into_iter()
            .filter(|e| self.selected.contains(&e.name))
            .map(|e| e.name.clone())
            .collect()
    }

    /// Click on `name` with the given modifiers (plain, Ctrl = toggle, Shift = range).
    pub fn click(&mut self, name: &str, ctrl: bool, shift: bool) {
        if shift {
            if let Some(anchor) = self.anchor.clone() {
                let order: Vec<String> = self.sorted().iter().map(|e| e.name.clone()).collect();
                let a = order.iter().position(|n| *n == anchor);
                let b = order.iter().position(|n| n == name);
                if let (Some(a), Some(b)) = (a, b) {
                    let (lo, hi) = (a.min(b), a.max(b));
                    if !ctrl {
                        self.selected.clear();
                    }
                    self.selected
                        .extend(order.get(lo..=hi).unwrap_or_default().iter().cloned());
                    return;
                }
            }
        }
        if ctrl {
            if !self.selected.remove(name) {
                self.selected.insert(name.to_owned());
            }
        } else {
            self.selected.clear();
            self.selected.insert(name.to_owned());
        }
        self.anchor = Some(name.to_owned());
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }
}

/// `dir` + `name` with the separator the path already uses (`\` for Windows paths).
pub(super) fn join(dir: &str, name: &str) -> String {
    let sep = if dir.starts_with('/') { '/' } else { '\\' };
    if dir.ends_with(['\\', '/']) {
        format!("{dir}{name}")
    } else {
        format!("{dir}{sep}{name}")
    }
}

/// The folder above `path`; `None` above a drive or `/` (the drives view).
pub(super) fn parent(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() || (trimmed.len() == 2 && trimmed.ends_with(':')) {
        return None;
    }
    let cut = trimmed.rfind(['\\', '/'])?;
    let head = trimmed.get(..cut)?;
    Some(if head.is_empty() {
        "/".to_owned()
    } else if head.len() == 2 && head.ends_with(':') {
        format!("{head}\\")
    } else {
        head.to_owned()
    })
}

/// Breadcrumbs `(label, path)` from the root to `path`.
pub(super) fn crumbs(path: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cur = Some(path.to_owned());
    while let Some(p) = cur {
        let label = p
            .trim_end_matches(['\\', '/'])
            .rsplit(['\\', '/'])
            .next()
            .filter(|s| !s.is_empty() && parent(&p).is_some())
            .map_or_else(|| p.clone(), str::to_owned);
        cur = parent(&p);
        out.push((label, p));
    }
    out.reverse();
    out
}

/// Display name of a known folder or drive.
pub(super) fn root_label(tx: Tx, r: &FsRoot) -> String {
    let kind = match r.kind {
        FsRootKind::Drive => {
            let name = if r.label.is_empty() {
                tx.raw(S::FsLocalDisk).to_owned()
            } else {
                r.label.clone()
            };
            return tx.d(&format!("{name} ({})", r.path.trim_end_matches('\\')));
        }
        FsRootKind::Home => S::FsHome,
        FsRootKind::Desktop => S::FsDesktop,
        FsRootKind::Documents => S::FsDocuments,
        FsRootKind::Downloads => S::FsDownloads,
        FsRootKind::Pictures => S::FsPictures,
        FsRootKind::Music => S::FsMusic,
        FsRootKind::Videos => S::FsVideos,
        FsRootKind::Other => {
            return if r.label.is_empty() {
                tx.d(&r.path)
            } else {
                tx.d(&r.label)
            };
        }
    };
    tx.t(kind).to_owned()
}

/// "YYYY-MM-DD HH:MM" in the given UTC offset (seconds).
pub(super) fn date_time(unix_ms: i64, offset_s: i32) -> String {
    let secs = unix_ms.div_euclid(1000).saturating_add(i64::from(offset_s));
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        (rem / 60) % 60
    )
}

/// Proleptic Gregorian date from days since 1970-01-01 (H. Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Column header with sort buttons. Returns the column clicked.
pub(super) fn header(ui: &mut Ui, tx: Tx, pane: &Pane) -> Option<SortKey> {
    let mut clicked = None;
    theme::row(ui, tx.rtl(), |ui| {
        for (key, label) in [
            (SortKey::Name, S::FsName),
            (SortKey::Size, S::FsSize),
            (SortKey::Modified, S::FsModified),
        ] {
            let arrow = match (pane.sort == key, pane.ascending) {
                (false, _) => "",
                (true, true) => " ▲",
                (true, false) => " ▼",
            };
            let text = RichText::new(format!("{}{arrow}", tx.t(label)))
                .small()
                .strong();
            if ui
                .add(egui::Button::new(text).frame(false))
                .on_hover_text(tx.t(S::FsSortHint))
                .clicked()
            {
                clicked = Some(key);
            }
        }
    });
    clicked
}

/// What happened to one row.
pub(super) struct RowResponse {
    pub response: egui::Response,
}

/// Draws one entry row across the full width (mirrored in RTL) and returns its response.
pub(super) fn entry_row(
    ui: &mut Ui,
    tx: Tx,
    entry: &FsEntry,
    selected: bool,
    offset_s: i32,
) -> RowResponse {
    let width = ui.available_width();
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, ROW_H), Sense::click_and_drag());
    let visuals = ui.visuals().clone();
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, 4.0, visuals.selection.bg_fill);
    } else if response.hovered() {
        painter.rect_filled(rect, 4.0, visuals.widgets.hovered.weak_bg_fill);
    }
    let text_color = if selected {
        visuals.selection.stroke.color
    } else if entry.hidden {
        visuals.weak_text_color()
    } else {
        visuals.text_color()
    };
    let rtl = tx.rtl();
    // Positions measured from the reading start.
    let x = |from_start: f32, w: f32| -> f32 {
        if rtl {
            rect.max.x - from_start - w
        } else {
            rect.min.x + from_start
        }
    };
    let size_text = if entry.is_dir {
        String::new()
    } else {
        tx.digits(&format::file_size(entry.size, tx.lang, tx.digits))
    };
    let date = tx.digits(&date_time(entry.modified_unix_ms, offset_s));
    let name_w = (width - ICON_W - SIZE_W - DATE_W - PAD * 4.0).max(40.0);

    let icon = if entry.is_dir {
        Icon::Folder
    } else {
        Icon::File
    };
    let icon_rect = Rect::from_min_size(
        Pos2::new(x(PAD, 16.0), rect.center().y - 8.0),
        egui::vec2(16.0, 16.0),
    );
    let tint = if entry.is_dir {
        theme::accent(visuals.dark_mode)
    } else {
        text_color
    };
    icon.image(16.0, tint).paint_at(ui, icon_rect);

    let font = egui::TextStyle::Body.resolve(ui.style());
    let small = egui::TextStyle::Small.resolve(ui.style());
    let cell = |text: String, font: egui::FontId, start: f32, w: f32, color: Color32| {
        let galley = ui.painter().layout(text, font, color, f32::INFINITY);
        let gw = galley.size().x.min(w);
        // Text hugs the reading start of its cell.
        let gx = if rtl {
            x(start, w) + w - gw
        } else {
            x(start, w)
        };
        let clip = Rect::from_min_size(Pos2::new(x(start, w), rect.min.y), egui::vec2(w, ROW_H));
        ui.painter().with_clip_rect(clip).galley(
            Pos2::new(gx, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
    };
    let name = tx.d(&entry.name);
    cell(name, font, PAD * 2.0 + ICON_W, name_w, text_color);
    let weak = if selected {
        text_color
    } else {
        visuals.weak_text_color()
    };
    cell(
        size_text,
        small.clone(),
        PAD * 3.0 + ICON_W + name_w,
        SIZE_W,
        weak,
    );
    cell(
        date,
        small,
        PAD * 4.0 + ICON_W + name_w + SIZE_W,
        DATE_W,
        weak,
    );
    RowResponse { response }
}

/// A drive or known folder in the roots view. Returns `true` when opened.
pub(super) fn root_row(ui: &mut Ui, tx: Tx, root: &FsRoot) -> bool {
    let mut open = false;
    theme::row(ui, tx.rtl(), |ui| {
        let icon = match root.kind {
            FsRootKind::Drive => Icon::HardDrive,
            FsRootKind::Home => Icon::Home,
            FsRootKind::Downloads => Icon::Download,
            _ => Icon::Folder,
        };
        let button = icons::icon_text_button(tx, icon, root_label(tx, root)).frame(false);
        if ui.add(button).clicked() {
            open = true;
        }
        if let (Some(free), Some(total)) = (root.free_bytes, root.total_bytes) {
            let text = tx.f(
                S::FsFreeOf,
                &[
                    (
                        "free",
                        &tx.digits(&format::file_size(free, tx.lang, tx.digits)),
                    ),
                    (
                        "total",
                        &tx.digits(&format::file_size(total, tx.lang, tx.digits)),
                    ),
                ],
            );
            ui.label(theme::caption(ui, &text));
        }
    });
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool, size: u64, t: i64) -> FsEntry {
        FsEntry {
            name: name.into(),
            is_dir,
            size,
            modified_unix_ms: t,
            readonly: false,
            hidden: false,
        }
    }

    #[test]
    fn paths_join_split_and_crumb_on_windows_and_unix() {
        assert_eq!(join(r"C:\", "مجلد"), r"C:\مجلد");
        assert_eq!(join(r"C:\a", "b.txt"), r"C:\a\b.txt");
        assert_eq!(join("/home", "x"), "/home/x");
        assert_eq!(parent(r"C:\a\b"), Some(r"C:\a".into()));
        assert_eq!(parent(r"C:\a"), Some(r"C:\".into()));
        assert_eq!(parent(r"C:\"), None);
        assert_eq!(parent("/a"), Some("/".into()));
        assert_eq!(parent("/"), None);
        let c = crumbs(r"C:\Users\مستخدم");
        let labels: Vec<_> = c.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, [r"C:\", "Users", "مستخدم"]);
        assert_eq!(c[1].1, r"C:\Users");
    }

    #[test]
    fn sorting_keeps_folders_first_and_flips() {
        let mut p = Pane::new(FsSide::Local);
        p.entries = vec![
            entry("b.txt", false, 10, 3),
            entry("ملف.txt", false, 5, 1),
            entry("z-dir", true, 0, 2),
            entry("A.txt", false, 20, 2),
        ];
        let names = |p: &Pane| {
            p.sorted()
                .iter()
                .map(|e| e.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&p), ["z-dir", "A.txt", "b.txt", "ملف.txt"]);
        p.sort = SortKey::Size;
        p.ascending = false;
        assert_eq!(names(&p), ["z-dir", "A.txt", "b.txt", "ملف.txt"]);
        p.ascending = true;
        assert_eq!(names(&p), ["z-dir", "ملف.txt", "b.txt", "A.txt"]);
    }

    #[test]
    fn click_ctrl_and_shift_select() {
        let mut p = Pane::new(FsSide::Local);
        p.entries = ["a", "b", "c", "d"]
            .iter()
            .map(|n| entry(n, false, 1, 1))
            .collect();
        p.click("b", false, false);
        p.click("d", false, true);
        assert_eq!(p.selection(), ["b", "c", "d"]);
        p.click("c", true, false);
        assert_eq!(p.selection(), ["b", "d"]);
        p.click("a", false, false);
        assert_eq!(p.selection(), ["a"]);
    }

    #[test]
    fn history_back_and_forward() {
        let mut p = Pane::new(FsSide::Local);
        p.navigate(Some(r"C:\".into()));
        p.navigate(Some(r"C:\a".into()));
        p.go_back();
        assert_eq!(p.path.as_deref(), Some(r"C:\"));
        assert!(p.can_forward());
        p.go_forward();
        assert_eq!(p.path.as_deref(), Some(r"C:\a"));
        p.navigate(Some(r"C:\b".into()));
        assert!(!p.can_forward());
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(date_time(0, 0), "1970-01-01 00:00");
        assert_eq!(date_time(951_782_400_000, 0), "2000-02-29 00:00");
        assert_eq!(date_time(1_700_000_000_000, 3 * 3600), "2023-11-15 01:13");
        assert_eq!(date_time(-86_400_000, 0), "1969-12-31 00:00");
    }
}
