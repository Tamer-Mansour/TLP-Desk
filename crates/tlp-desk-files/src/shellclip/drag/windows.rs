//! OLE drag source of drag-out (Windows only): `DoDragDrop` with an `IDropSource` on the GUI
//! thread, and the virtual-files data object on a thread of its own.
//!
//! Why two threads: `DoDragDrop` runs on the thread that holds the mouse, the GUI thread
//! (already an OLE apartment: winit registers its drop target there). The drop target reads
//! the file contents through the data object, and those reads wait for the transfer engine;
//! if the object lived on the GUI thread, every read would freeze the window. So each drag
//! creates its object on a fresh single-threaded apartment (`tlp-dragout`) and marshals it to
//! the GUI thread (`CoMarshalInterThreadInterfaceInStream`). The GUI thread hands that proxy to
//! `DoDragDrop`, so the drop target's calls go straight to the object's thread (the object is
//! not agile, which is what makes it a proxy). The thread ends when the target releases the
//! object: it finished copying, or the drag was cancelled.
//!
//! Every `unsafe` block carries a `// SAFETY:` comment.

use std::mem::ManuallyDrop;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use windows::core::{implement, Interface, BOOL, HRESULT};
use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, POINT, S_OK,
};
use windows::Win32::System::Com::Marshal::CoMarshalInterThreadInterfaceInStream;
use windows::Win32::System::Com::StructuredStorage::CoGetInterfaceAndReleaseStream;
use windows::Win32::System::Com::{IDataObject, IStream};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize, DROPEFFECT,
    DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetCursorPos, GetMessageW, TranslateMessage, MSG,
};

use super::super::staged::StagedItems;
use super::super::windows::{Formats, Release, VirtualFiles};
use super::{next_step, DragOutcome, DragStep};

/// How long the GUI thread waits for the object thread to hand over its data object.
const OBJECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The drag source: decides when the drag drops or is cancelled and lets the system draw the
/// cursors (Microsoft Learn, "IDropSource interface").
#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        match next_step(fescapepressed.as_bool(), grfkeystate.0 & MK_LBUTTON.0 != 0) {
            DragStep::Continue => S_OK,
            DragStep::Drop => DRAGDROP_S_DROP,
            DragStep::Cancel => DRAGDROP_S_CANCEL,
        }
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Balances one successful `OleInitialize` of the current thread.
struct OleGuard;

impl Drop for OleGuard {
    fn drop(&mut self) {
        // SAFETY: only created after OleInitialize succeeded on this same thread.
        unsafe { OleUninitialize() };
    }
}

/// Initialises OLE on the current thread (`S_FALSE`, already initialised, counts too).
fn ole() -> Result<OleGuard, String> {
    // SAFETY: plain apartment initialisation of the calling thread; balanced by the guard.
    match unsafe { OleInitialize(None) } {
        Ok(()) => Ok(OleGuard),
        Err(e) => Err(format!("OLE unavailable on this thread: {e}")),
    }
}

/// See [`super::cursor_screen_position`].
pub(super) fn cursor_screen_position() -> Option<(i32, i32)> {
    let mut pt = POINT::default();
    // SAFETY: `pt` is a valid out-pointer for the duration of the call.
    unsafe { GetCursorPos(&mut pt) }.ok()?;
    Some((pt.x, pt.y))
}

/// Runs the drag on the calling (GUI) thread. See [`super::DragOut::run`].
pub(super) fn run(staged: Arc<StagedItems>) -> DragOutcome {
    let _ole = match ole() {
        Ok(g) => g,
        Err(e) => {
            super::super::discard(&staged);
            return DragOutcome::Failed(e);
        }
    };
    let Some(raw) = spawn_object(staged.clone()) else {
        super::super::discard(&staged);
        return DragOutcome::Failed("the drag data object could not be created".into());
    };
    // SAFETY: `raw` came from `IStream::into_raw` on the object thread, which passed its only
    // reference to us through the channel. The call below releases that reference (even when
    // it fails, as documented), so the wrapper must never release it again: ManuallyDrop.
    let stream = ManuallyDrop::new(unsafe { IStream::from_raw(raw as *mut core::ffi::c_void) });
    // SAFETY: the stream holds marshal data of an IDataObject made for this process.
    let unmarshalled: windows::core::Result<IDataObject> =
        unsafe { CoGetInterfaceAndReleaseStream(&*stream) };
    let data = match unmarshalled {
        Ok(d) => d,
        Err(e) => return DragOutcome::Failed(format!("cannot unmarshal the data object: {e}")),
    };
    let source: IDropSource = DropSource.into();
    let mut effect = DROPEFFECT_NONE;
    tracing::info!(dir = %staged.dir().display(), "drag out of the file browser started");
    // SAFETY: both interfaces are live for the whole call and `effect` is a valid out-pointer.
    let hr = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
    // Our proxy goes; the drop target keeps its own reference while it copies.
    drop(data);
    let outcome = if hr == DRAGDROP_S_DROP {
        if effect == DROPEFFECT_NONE {
            DragOutcome::Cancelled
        } else {
            DragOutcome::Dropped
        }
    } else if hr == DRAGDROP_S_CANCEL {
        DragOutcome::Cancelled
    } else {
        DragOutcome::Failed(format!("DoDragDrop failed: {hr}"))
    };
    tracing::info!(?outcome, effect = effect.0, "drag out ended");
    outcome
}

/// Starts the object thread and returns its marshalled data object (raw `IStream`).
fn spawn_object(staged: Arc<StagedItems>) -> Option<usize> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("tlp-dragout".into())
        .spawn(move || object_thread(staged, &tx));
    if let Err(e) = spawned {
        tracing::warn!(%e, "cannot start the drag thread");
        return None;
    }
    rx.recv_timeout(OBJECT_TIMEOUT).ok().flatten()
}

/// The data object's apartment: creates and marshals it, then serves the drop target's calls
/// until the object is released (its `Drop` posts `WM_QUIT`).
fn object_thread(staged: Arc<StagedItems>, ready: &mpsc::Sender<Option<usize>>) {
    let _ole = match ole() {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(%e, "no drag-out");
            let _ = ready.send(None);
            return;
        }
    };
    let Some(fmt) = Formats::register() else {
        let _ = ready.send(None);
        return;
    };
    let obj: IDataObject = VirtualFiles::new(staged, fmt, Release::Drag).into();
    // SAFETY: `obj` is a live COM object; the stream keeps its own reference until it is
    // unmarshalled.
    let marshalled = unsafe { CoMarshalInterThreadInterfaceInStream(&IDataObject::IID, &obj) };
    // From here the stream's reference keeps the object alive.
    drop(obj);
    let stream = match marshalled {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(%e, "cannot marshal the drag data object");
            let _ = ready.send(None);
            return;
        }
    };
    let raw = stream.into_raw() as usize;
    if ready.send(Some(raw)).is_err() {
        // The GUI thread stopped waiting: unmarshal here so the object gets released.
        // SAFETY: `raw` is the stream created above, not handed to anyone; the call releases
        // it (also on failure), so the wrapper never does (ManuallyDrop).
        unsafe {
            let stream = ManuallyDrop::new(IStream::from_raw(raw as *mut core::ffi::c_void));
            let _ = CoGetInterfaceAndReleaseStream::<_, IDataObject>(&*stream);
        }
    }
    loop {
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid out-pointer.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            break;
        }
        // SAFETY: `msg` was just filled in by GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    tracing::debug!("drag data object released");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shellclip::descriptor::build_group_descriptor;
    use crate::shellclip::{StageState, Stager};
    use tlp_desk_proto::TransferItem;
    use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL, TYMED_ISTREAM};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::ReleaseStgMedium;

    struct Done;
    impl Stager for Done {
        fn begin(&self) {}
        fn state(&self) -> StageState {
            StageState::Finished
        }
        fn cancel(&self) {}
    }

    /// The marshalling half of a drag without the OS drag loop (nothing on screen, no mouse,
    /// no clipboard): the data object made on its own thread is used from this thread through
    /// COM like a drop target would, and releasing it removes the staging folder and ends the
    /// object's thread.
    #[test]
    fn the_drag_data_object_serves_reads_from_its_thread_and_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let stage = tmp.path().join("staging");
        std::fs::create_dir_all(stage.join("مجلد")).unwrap();
        std::fs::write(stage.join("مجلد").join("ملف.txt"), "محتوى الملف").unwrap();
        let items = vec![
            TransferItem {
                rel_path: "مجلد".into(),
                size: 0,
                modified_unix_ms: 0,
                is_dir: true,
            },
            TransferItem {
                rel_path: "مجلد/ملف.txt".into(),
                size: "محتوى الملف".len() as u64,
                modified_unix_ms: 1_700_000_000_000,
                is_dir: false,
            },
        ];
        let staged = StagedItems::new(stage.clone(), items.clone(), Arc::new(Done));
        let _ole = ole().unwrap();
        let fmt = Formats::register().unwrap();
        let raw = spawn_object(staged).expect("data object");
        // SAFETY: test of the same hand-over `run` does: the stream is released by the call.
        let data: IDataObject = unsafe {
            let stream = ManuallyDrop::new(IStream::from_raw(raw as *mut core::ffi::c_void));
            CoGetInterfaceAndReleaseStream(&*stream).unwrap()
        };
        let mut fe = FORMATETC {
            cfFormat: fmt.descriptor,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        // SAFETY: COM calls on live interfaces with valid FORMATETCs; every medium is locked
        // only while read and released afterwards.
        unsafe {
            let mut med = data.GetData(&fe).expect("descriptor");
            let h = med.u.hGlobal;
            let len = GlobalSize(h);
            let p = GlobalLock(h);
            let got = std::slice::from_raw_parts(p.cast::<u8>(), len).to_vec();
            let _ = GlobalUnlock(h);
            ReleaseStgMedium(&mut med);
            let want = build_group_descriptor(&items).bytes;
            assert_eq!(&got[..want.len()], &want[..]);

            fe.cfFormat = fmt.contents;
            fe.lindex = 1; // the file (index 0 is the folder)
            fe.tymed = TYMED_ISTREAM.0 as u32;
            let mut med = data.GetData(&fe).expect("contents");
            let stream = (*med.u.pstm).clone().expect("stream");
            let mut buf = [0u8; 64];
            let mut n = 0u32;
            assert!(stream
                .Read(buf.as_mut_ptr().cast(), 64, Some(&mut n))
                .is_ok());
            assert_eq!(&buf[..n as usize], "محتوى الملف".as_bytes());
            drop(stream);
            ReleaseStgMedium(&mut med);
        }
        drop(data);
        for _ in 0..150 {
            if !stage.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !stage.exists(),
            "the released drag object removes its staging folder"
        );
    }
}
