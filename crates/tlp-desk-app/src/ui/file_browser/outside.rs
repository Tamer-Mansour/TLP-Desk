//! Files to and from other programs: dragging remote rows out of the window into Explorer
//! (drag-out) and dropping Explorer files onto a pane (drop-in).
//!
//! * **Drag-out**: a drag that starts on remote rows prepares the backend at once (the remote
//!   folders are listed while the pointer moves). Released inside the window it stays the
//!   in-app pane-to-pane copy; when the pointer leaves the window the OS drag loop takes over
//!   ([`Backend::drag_out_run`], blocking until the drop), and Explorer copies the files with
//!   its own progress. The OS loop swallows the mouse-up, so the next frame is told about it
//!   ([`release_pointer_if_requested`], called from the app's raw input hook).
//! * **Drop-in**: files dropped on the remote pane upload into its current folder; on the
//!   local pane they are not copied (a single dropped folder is opened there). Dropped
//!   anywhere else they are left to the session window, which sends them to the remote
//!   Desktop. winit reports no pointer position while another program drags over the window,
//!   so the drop is hit-tested with the OS pointer position ([`os_pointer`]).

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{self, pos2, Color32, Id, Pos2, Rect};

use super::pane::{self, DragFiles};
use super::{FileBrowser, LOCAL, REMOTE};
use crate::backend::files::{DragOutcome, TransferRequest};
use crate::backend::Backend;
use crate::i18n::{Tx, S};
use crate::ui::theme;

/// Where files dragged in from another program would land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DropTarget {
    /// The remote pane: upload into this folder (`None`: no folder is open there).
    Remote(Option<String>),
    /// This PC's pane: nothing to copy.
    Local,
}

/// Memory key of the "mouse button is up" note for the next frame.
fn release_id() -> Id {
    Id::new("tlp-drag-out-release-pointer")
}

/// Whether the pointer has left the window (`None`: egui already saw it leave).
pub(super) fn leaves_window(pos: Option<Pos2>, window: Rect) -> bool {
    pos.is_none_or(|p| !window.shrink(1.0).contains(p))
}

/// A physical screen position as window points (`inner_min`: the window's client area in
/// points, as egui-winit computes it: physical / pixels per point).
pub(super) fn screen_to_window(screen_px: (i32, i32), inner_min: Pos2, ppp: f32) -> Pos2 {
    pos2(
        screen_px.0 as f32 / ppp - inner_min.x,
        screen_px.1 as f32 / ppp - inner_min.y,
    )
}

/// The pointer position during a drag from another program, in window points: the OS knows
/// it; egui's last position is the fallback (and what tests use).
pub(super) fn os_pointer(ctx: &egui::Context) -> Option<Pos2> {
    let from_os = tlp_desk_files::shellclip::drag::cursor_screen_position().and_then(|px| {
        let inner = ctx.input(|i| i.viewport().inner_rect)?;
        let ppp = ctx.pixels_per_point();
        (ppp > 0.0).then(|| screen_to_window(px, inner.min, ppp))
    });
    from_os.or_else(|| ctx.input(|i| i.pointer.hover_pos()))
}

/// Notes that the OS drag loop took the mouse-up: the next frame's input gets it back.
fn request_pointer_release(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(release_id(), true));
    ctx.request_repaint();
}

/// Call from `eframe::App::raw_input_hook`: after a drag out of the window, tells egui that
/// the primary button is up (the OS drag loop consumed the real release), so egui does not
/// think a drag is still going on.
pub fn release_pointer_if_requested(ctx: &egui::Context, raw: &mut egui::RawInput) {
    let asked = ctx
        .data_mut(|d| d.remove_temp::<bool>(release_id()))
        .unwrap_or(false);
    if asked {
        raw.events.push(egui::Event::PointerButton {
            // Outside the window, so the release can never click a widget.
            pos: pos2(-1.0, -1.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: ctx.input(|i| i.modifiers),
        });
        raw.events.push(egui::Event::PointerGone);
    }
}

/// The name shown for a folder: its last component (the whole path for a drive).
fn folder_label(path: &str) -> String {
    pane::crumbs(path)
        .pop()
        .map_or_else(|| path.to_owned(), |(label, _)| label)
}

impl FileBrowser {
    /// Where a drop at `pos` (window points) goes; `None` outside both panes.
    pub(super) fn drop_target(&self, pos: Option<Pos2>) -> Option<DropTarget> {
        let pos = pos?;
        if !self.open {
            return None;
        }
        let inside = |i: usize| {
            self.pane_rects
                .get(i)
                .copied()
                .flatten()
                .is_some_and(|r| r.contains(pos))
        };
        if inside(REMOTE) {
            Some(DropTarget::Remote(
                self.panes.get(REMOTE).and_then(|p| p.path.clone()),
            ))
        } else if inside(LOCAL) {
            Some(DropTarget::Local)
        } else {
            None
        }
    }

    /// Whether a drag from another program is over a pane (its overlay replaces the
    /// session window's "drop to send" overlay).
    pub fn drop_hover_handled(&self) -> bool {
        self.drop_hover.is_some()
    }

    /// Hover feedback and routing of files dragged in from another program.
    pub(super) fn files_from_outside(
        &mut self,
        ctx: &egui::Context,
        tx: Tx,
        backend: &dyn Backend,
    ) {
        let (hovering, dropped) = ctx.input(|i| {
            (
                !i.raw.hovered_files.is_empty(),
                !i.raw.dropped_files.is_empty(),
            )
        });
        if !hovering && !dropped {
            self.drop_hover = None;
            return;
        }
        let target = self.drop_target((self.pointer)(ctx));
        if hovering {
            // No pointer events arrive while another program drags over us: poll.
            ctx.request_repaint_after(Duration::from_millis(50));
            if let Some(t) = &target {
                self.paint_drop_hover(ctx, tx, t);
            }
        }
        self.drop_hover = target.clone().filter(|_| hovering);
        if !dropped {
            return;
        }
        // Outside the panes the session window sends the files to the remote Desktop.
        let Some(target) = target else {
            return;
        };
        let paths: Vec<PathBuf> = ctx.input_mut(|i| {
            std::mem::take(&mut i.raw.dropped_files)
                .into_iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        self.drop_hover = None;
        match target {
            DropTarget::Remote(Some(remote_dir)) if !paths.is_empty() => {
                backend.fs_transfer(
                    self.session,
                    TransferRequest::Upload {
                        local: paths,
                        remote_dir,
                    },
                );
            }
            DropTarget::Remote(Some(_)) => {}
            DropTarget::Remote(None) => {
                self.notice = Some(tx.t(S::FsDropPickFolder).to_owned());
            }
            DropTarget::Local => {
                // Nothing to copy on this PC; one dropped folder is opened in the pane.
                if let [one] = paths.as_slice() {
                    if one.is_dir() {
                        if let Some(p) = self.panes.get_mut(LOCAL) {
                            p.navigate(Some(one.to_string_lossy().into_owned()));
                        }
                    }
                }
            }
        }
    }

    fn paint_drop_hover(&self, ctx: &egui::Context, tx: Tx, target: &DropTarget) {
        let (pane, text) = match target {
            DropTarget::Remote(Some(dir)) => (
                REMOTE,
                tx.f(S::FsDropIntoFolder, &[("folder", &folder_label(dir))]),
            ),
            DropTarget::Remote(None) => (REMOTE, tx.t(S::FsDropPickFolder).to_owned()),
            DropTarget::Local => (LOCAL, tx.t(S::FsDropOnRemoteHint).to_owned()),
        };
        let Some(rect) = self.pane_rects.get(pane).copied().flatten() else {
            return;
        };
        let dark = ctx.global_style().visuals.dark_mode;
        let accent = theme::accent(dark);
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            Id::new(("fs-drop-hover", self.session.0)),
        ));
        let usable = matches!(target, DropTarget::Remote(Some(_)));
        let fill = if usable {
            accent.gamma_multiply(0.30)
        } else {
            Color32::from_black_alpha(150)
        };
        painter.rect_filled(rect, 6.0, fill);
        if usable {
            painter.rect_stroke(
                rect,
                6.0,
                egui::Stroke::new(2.0, accent),
                egui::StrokeKind::Inside,
            );
        }
        let galley = painter.layout(
            text,
            egui::FontId::proportional(18.0),
            Color32::WHITE,
            (rect.width() - 24.0).max(40.0),
        );
        let at = rect.center() - galley.size() / 2.0;
        painter.galley(at, galley, Color32::WHITE);
    }

    /// Remote rows started moving: let the backend list them now.
    pub(super) fn prepare_drag_out(&mut self, names: Vec<String>, backend: &dyn Backend) {
        let Some(dir) = self.panes.get(REMOTE).and_then(|p| p.path.clone()) else {
            return;
        };
        if names.is_empty() {
            return;
        }
        let full = names.iter().map(|n| pane::join(&dir, n)).collect();
        backend.drag_out_prepare(self.session, full);
        self.dragging_out = true;
    }

    /// Hands a drag of remote rows to the OS once the pointer leaves the window.
    pub(super) fn drag_out_tick(&mut self, ctx: &egui::Context, tx: Tx, backend: &dyn Backend) {
        if !self.dragging_out {
            return;
        }
        let from_remote =
            egui::DragAndDrop::payload::<DragFiles>(ctx).is_some_and(|d| d.from == REMOTE);
        let (down, pos) = ctx.input(|i| (i.pointer.primary_down(), i.pointer.latest_pos()));
        if !from_remote || !down {
            // Released (or cancelled) inside the window: the in-app drop, if any, did the copy.
            self.dragging_out = false;
            backend.drag_out_cancel(self.session);
            return;
        }
        if !leaves_window(pos, ctx.content_rect()) {
            return;
        }
        self.dragging_out = false;
        egui::DragAndDrop::clear_payload(ctx);
        let outcome = backend.drag_out_run(self.session);
        request_pointer_release(ctx);
        match outcome {
            DragOutcome::Dropped => {
                self.notice = Some(tx.t(S::FsDragOutDropped).to_owned());
            }
            DragOutcome::Failed(detail) => {
                tracing::warn!(%detail, "drag out failed");
                self.notice = Some(tx.f(S::FsDragOutFailed, &[("detail", &detail)]));
            }
            DragOutcome::Cancelled | DragOutcome::Unsupported => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::files::{FsOp, FsRequestId, FsSide};
    use crate::backend::{AttemptId, ConnectTarget, DeviceId, LocalInfo, RequestId, SessionId};
    use crate::i18n::{DigitStyle, Lang};
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use tlp_desk_proto::InputEvent;

    /// A file dropped from Explorer, as egui-winit hands it over.
    #[derive(Debug)]
    struct Dropped(PathBuf);

    impl egui::DroppedFile for Dropped {
        fn path(&self) -> &Path {
            &self.0
        }
        fn bytes(&self) -> Result<Vec<u8>, String> {
            Err("not read in tests".into())
        }
    }

    /// Records file transfers and drag-out calls.
    #[derive(Default)]
    struct Rec {
        transfers: Mutex<Vec<TransferRequest>>,
        drags: Mutex<Vec<String>>,
    }

    impl Backend for Rec {
        fn local_info(&self) -> LocalInfo {
            LocalInfo {
                device_id: DeviceId(1),
                device_name: String::new(),
                addresses: vec![],
                one_time_code: String::new(),
                identity_conflict: None,
            }
        }
        fn set_device_name(&self, _: String) {}
        fn refresh_one_time_code(&self) {}
        fn connect(&self, _: ConnectTarget, _: Option<String>) -> AttemptId {
            AttemptId(0)
        }
        fn cancel_connect(&self, _: AttemptId) {}
        fn forget_host_key(&self, _: AttemptId) -> bool {
            false
        }
        fn respond_to_request(&self, _: RequestId, _: bool) {}
        fn send_input(&self, _: SessionId, _: InputEvent) {}
        fn set_view_only(&self, _: SessionId, _: bool) {}
        fn disconnect(&self, _: SessionId) {}
        fn select_monitor(&self, _: SessionId, _: u32) {}
        fn fs_request(&self, _: FsSide, _: FsOp) -> Option<FsRequestId> {
            Some(FsRequestId(1))
        }
        fn fs_transfer(&self, _: SessionId, request: TransferRequest) {
            self.transfers.lock().unwrap().push(request);
        }
        fn drag_out_prepare(&self, session: SessionId, remote: Vec<String>) {
            self.drags
                .lock()
                .unwrap()
                .push(format!("prepare {} {remote:?}", session.0));
        }
        fn drag_out_run(&self, session: SessionId) -> DragOutcome {
            self.drags
                .lock()
                .unwrap()
                .push(format!("run {}", session.0));
            DragOutcome::Dropped
        }
        fn drag_out_cancel(&self, session: SessionId) {
            self.drags
                .lock()
                .unwrap()
                .push(format!("cancel {}", session.0));
        }
    }

    const SCREEN: Rect = Rect {
        min: Pos2::ZERO,
        max: Pos2::new(1100.0, 700.0),
    };

    /// A browser with both panes in a folder and known pane rectangles (left local, right
    /// remote; mirrored in Arabic).
    fn browser(rtl: bool) -> FileBrowser {
        let mut fb = FileBrowser::new(SessionId(4));
        fb.open = true;
        fb.pointer = |ctx| ctx.input(|i| i.pointer.hover_pos());
        for (i, path) in [(LOCAL, "C:\\بيانات"), (REMOTE, "D:\\وجهة\\تقارير")] {
            fb.panes[i].path = Some(path.into());
            fb.panes[i].requested = true;
        }
        let left = Rect::from_min_max(pos2(0.0, 400.0), pos2(550.0, 700.0));
        let right = Rect::from_min_max(pos2(550.0, 400.0), pos2(1100.0, 700.0));
        let (local, remote) = if rtl { (right, left) } else { (left, right) };
        fb.pane_rects = [Some(local), Some(remote)];
        fb
    }

    #[test]
    fn drops_are_routed_by_pane_in_both_directions_of_reading() {
        for rtl in [false, true] {
            let fb = browser(rtl);
            let (remote_x, local_x) = if rtl { (100.0, 900.0) } else { (900.0, 100.0) };
            assert_eq!(
                fb.drop_target(Some(pos2(remote_x, 500.0))),
                Some(DropTarget::Remote(Some("D:\\وجهة\\تقارير".into())))
            );
            assert_eq!(
                fb.drop_target(Some(pos2(local_x, 500.0))),
                Some(DropTarget::Local)
            );
            // Above the panel (the remote screen) and without a position: the session's.
            assert_eq!(fb.drop_target(Some(pos2(500.0, 100.0))), None);
            assert_eq!(fb.drop_target(None), None);
        }
        let mut closed = browser(false);
        closed.open = false;
        assert_eq!(closed.drop_target(Some(pos2(900.0, 500.0))), None);
        let mut roots = browser(false);
        roots.panes[REMOTE].path = None;
        assert_eq!(
            roots.drop_target(Some(pos2(900.0, 500.0))),
            Some(DropTarget::Remote(None))
        );
    }

    /// One frame of `files_from_outside` with this raw input.
    fn outside_frame(fb: &mut FileBrowser, b: &Rec, tx: Tx, raw: egui::RawInput) {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(raw, |ui| {
            fb.files_from_outside(ui.ctx(), tx, b);
            // What the session window would still see afterwards.
            let left = ui.ctx().input(|i| i.raw.dropped_files.len());
            ui.ctx().data_mut(|d| d.insert_temp(Id::new("left"), left));
        });
        out.textures_delta.clear();
    }

    fn drop_at(pos: Pos2, paths: &[PathBuf]) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(SCREEN),
            events: vec![egui::Event::PointerMoved(pos)],
            dropped_files: paths
                .iter()
                .map(|p| Arc::new(Dropped(p.clone())) as egui::DroppedFileHandle)
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn files_dropped_on_the_remote_pane_upload_into_its_folder() {
        let tx = Tx::new(Lang::Ar, DigitStyle::Western);
        let b = Rec::default();
        let mut fb = browser(false);
        let files = vec![PathBuf::from("C:\\المستندات\\عقد.pdf")];
        outside_frame(&mut fb, &b, tx, drop_at(pos2(900.0, 500.0), &files));
        assert_eq!(
            *b.transfers.lock().unwrap(),
            vec![TransferRequest::Upload {
                local: files.clone(),
                remote_dir: "D:\\وجهة\\تقارير".into(),
            }]
        );
        // Dropped on the remote screen above the panel: left for the session (Desktop).
        outside_frame(&mut fb, &b, tx, drop_at(pos2(500.0, 100.0), &files));
        assert_eq!(b.transfers.lock().unwrap().len(), 1);
        // On the local pane: nothing is copied.
        outside_frame(&mut fb, &b, tx, drop_at(pos2(100.0, 500.0), &files));
        assert_eq!(b.transfers.lock().unwrap().len(), 1);
        // The remote pane shows the drives: ask for a folder instead of guessing one.
        fb.panes[REMOTE].path = None;
        outside_frame(&mut fb, &b, tx, drop_at(pos2(900.0, 500.0), &files));
        assert_eq!(b.transfers.lock().unwrap().len(), 1);
        assert_eq!(fb.notice.as_deref(), Some(tx.t(S::FsDropPickFolder)));
    }

    #[test]
    fn a_folder_dropped_on_the_local_pane_is_opened_there() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("مجلد عربي");
        std::fs::create_dir_all(&folder).unwrap();
        let b = Rec::default();
        let mut fb = browser(true);
        let tx = Tx::new(Lang::Ar, DigitStyle::Western);
        // Arabic layout: this PC's pane is on the right.
        outside_frame(
            &mut fb,
            &b,
            tx,
            drop_at(pos2(900.0, 500.0), std::slice::from_ref(&folder)),
        );
        assert_eq!(
            fb.panes[LOCAL].path.as_deref(),
            Some(folder.to_string_lossy().as_ref())
        );
        assert!(b.transfers.lock().unwrap().is_empty());
    }

    #[test]
    fn hovering_over_a_pane_takes_over_the_overlay() {
        let b = Rec::default();
        let mut fb = browser(false);
        let tx = Tx::new(Lang::En, DigitStyle::Western);
        let hover = |pos| egui::RawInput {
            screen_rect: Some(SCREEN),
            events: vec![egui::Event::PointerMoved(pos)],
            hovered_files: vec![egui::HoveredFile::default()],
            ..Default::default()
        };
        outside_frame(&mut fb, &b, tx, hover(pos2(900.0, 500.0)));
        assert!(fb.drop_hover_handled());
        outside_frame(&mut fb, &b, tx, hover(pos2(500.0, 100.0)));
        assert!(
            !fb.drop_hover_handled(),
            "over the remote screen: the session's overlay"
        );
        outside_frame(&mut fb, &b, tx, egui::RawInput::default());
        assert!(!fb.drop_hover_handled());
    }

    #[test]
    fn leaving_the_window_is_detected_from_position_or_its_absence() {
        assert!(!leaves_window(Some(pos2(500.0, 300.0)), SCREEN));
        assert!(leaves_window(Some(pos2(-5.0, 300.0)), SCREEN));
        assert!(leaves_window(Some(pos2(500.0, 900.0)), SCREEN));
        assert!(
            leaves_window(Some(pos2(1100.0, 300.0)), SCREEN),
            "the edge counts"
        );
        assert!(leaves_window(None, SCREEN));
    }

    #[test]
    fn screen_pixels_map_to_window_points_at_any_scale() {
        // Window client area at physical (300, 150); 150 % scaling.
        let inner_min = pos2(200.0, 100.0);
        assert_eq!(
            screen_to_window((450, 300), inner_min, 1.5),
            pos2(100.0, 100.0)
        );
        assert_eq!(screen_to_window((300, 150), inner_min, 1.5), Pos2::ZERO);
        assert_eq!(
            screen_to_window((100, 100), pos2(0.0, 0.0), 1.0),
            pos2(100.0, 100.0)
        );
    }

    #[test]
    fn the_release_note_reaches_the_next_frame_once() {
        let ctx = egui::Context::default();
        request_pointer_release(&ctx);
        let mut raw = egui::RawInput::default();
        release_pointer_if_requested(&ctx, &mut raw);
        assert!(raw.events.iter().any(|e| matches!(
            e,
            egui::Event::PointerButton {
                pressed: false,
                button: egui::PointerButton::Primary,
                ..
            }
        )));
        let mut again = egui::RawInput::default();
        release_pointer_if_requested(&ctx, &mut again);
        assert!(again.events.is_empty());
    }

    #[test]
    fn a_remote_drag_is_prepared_then_run_when_the_pointer_leaves() {
        let b = Rec::default();
        let tx = Tx::new(Lang::Ar, DigitStyle::Western);
        let mut fb = browser(false);
        fb.prepare_drag_out(vec!["ملف.txt".into(), "مجلد".into()], &b);
        assert!(fb.dragging_out);
        assert_eq!(
            b.drags.lock().unwrap()[0],
            format!(
                "prepare 4 {:?}",
                vec!["D:\\وجهة\\تقارير\\ملف.txt", "D:\\وجهة\\تقارير\\مجلد"]
            )
        );
        // The pointer is down with the remote payload and moves out of the window.
        let ctx = egui::Context::default();
        let frame = |ctx: &egui::Context, fb: &mut FileBrowser, events| {
            let raw = egui::RawInput {
                screen_rect: Some(SCREEN),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| {
                egui::DragAndDrop::set_payload(
                    ui.ctx(),
                    DragFiles {
                        from: REMOTE,
                        names: vec!["ملف.txt".into()],
                    },
                );
                fb.drag_out_tick(ui.ctx(), tx, &b);
            });
            out.textures_delta.clear();
        };
        let press = egui::Event::PointerButton {
            pos: pos2(900.0, 500.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            &ctx,
            &mut fb,
            vec![egui::Event::PointerMoved(pos2(900.0, 500.0)), press],
        );
        assert!(fb.dragging_out, "still inside: nothing yet");
        frame(&ctx, &mut fb, vec![egui::Event::PointerGone]);
        assert!(!fb.dragging_out);
        assert_eq!(b.drags.lock().unwrap().last().unwrap(), "run 4");
        assert_eq!(fb.notice.as_deref(), Some(tx.t(S::FsDragOutDropped)));
        // The next frame learns that the button is up.
        let mut raw = egui::RawInput::default();
        release_pointer_if_requested(&ctx, &mut raw);
        assert!(!raw.events.is_empty());
    }

    #[test]
    fn a_remote_drag_released_inside_is_cancelled() {
        let b = Rec::default();
        let tx = Tx::new(Lang::En, DigitStyle::Western);
        let mut fb = browser(false);
        fb.prepare_drag_out(vec!["a.txt".into()], &b);
        let ctx = egui::Context::default();
        // No payload any more (the pane drop zone took it) and the button is up.
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            fb.drag_out_tick(ui.ctx(), tx, &b);
        });
        out.textures_delta.clear();
        assert!(!fb.dragging_out);
        assert_eq!(b.drags.lock().unwrap().last().unwrap(), "cancel 4");
    }
}
