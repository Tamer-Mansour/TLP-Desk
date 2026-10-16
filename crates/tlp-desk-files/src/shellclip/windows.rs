//! COM side of the virtual clipboard files (Windows only).
//!
//! One thread, `tlp-shellclip`, is an OLE single-threaded apartment with a message loop. It owns
//! the clipboard object: `OleSetClipboard` needs a pumping STA, and Explorer's `GetData`
//! calls arrive on that thread. [`VirtualFiles`] is the `IDataObject` (plus
//! `IDataObjectAsyncCapability`, so Explorer copies on a worker thread with its own progress
//! dialog); [`VirtualStream`] is the `IStream` it hands out per file, which waits (pumping
//! messages) until the transfer engine has staged the whole file.
//!
//! Every `unsafe` block carries a `// SAFETY:` comment. COM pointers that come from the caller
//! are checked for null before use.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};

use windows::core::{implement, w, BOOL, HRESULT};
use windows::Win32::Foundation::{
    GlobalFree, E_FAIL, E_INVALIDARG, E_NOTIMPL, E_POINTER, HGLOBAL, STG_E_INVALIDFUNCTION,
    STG_E_READFAULT, S_OK,
};
use windows::Win32::System::Com::{
    IAdviseSink, IBindCtx, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA,
    ISequentialStream_Impl, IStream, IStream_Impl, DATADIR_GET, DVASPECT_CONTENT, FORMATETC,
    LOCKTYPE, STATFLAG, STATSTG, STGC, STGMEDIUM, STGMEDIUM_0, STGTY_STREAM, STREAM_SEEK,
    STREAM_SEEK_CUR, STREAM_SEEK_END, STREAM_SEEK_SET, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{
    OleInitialize, OleIsCurrentClipboard, OleSetClipboard, OleUninitialize, ReleaseStgMedium,
    DROPEFFECT_COPY,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::{
    IDataObjectAsyncCapability, IDataObjectAsyncCapability_Impl, SHCreateStdEnumFmtEtc,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW, TranslateMessage, MSG,
    PM_NOREMOVE, PM_REMOVE, WM_APP, WM_QUIT,
};

use super::descriptor::{build_group_descriptor, GroupDescriptor};
use super::staged::StagedItems;
use super::ClipboardPort;

/// Registered clipboard format ids of the shell formats we serve.
#[derive(Clone, Copy)]
struct Formats {
    descriptor: u16,
    contents: u16,
    preferred_effect: u16,
    performed_effect: u16,
    paste_succeeded: u16,
}

impl Formats {
    fn register() -> Option<Self> {
        let reg = |name: windows::core::PCWSTR| {
            // SAFETY: `name` is a valid NUL-terminated wide string literal.
            let id = unsafe { RegisterClipboardFormatW(name) };
            u16::try_from(id).ok().filter(|&i| i != 0)
        };
        Some(Self {
            descriptor: reg(w!("FileGroupDescriptorW"))?,
            contents: reg(w!("FileContents"))?,
            preferred_effect: reg(w!("Preferred DropEffect"))?,
            performed_effect: reg(w!("Performed DropEffect"))?,
            paste_succeeded: reg(w!("Paste Succeeded"))?,
        })
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Handles pending window messages for a moment so COM calls (and our own `Release`s) are
/// served while a read waits. Our command message is put back for the main loop.
fn pump() {
    let mut deferred = 0u32;
    let mut quit: Option<i32> = None;
    loop {
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid out-pointer; no window filter, remove the message.
        let got = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) };
        if !got.as_bool() {
            break;
        }
        if msg.message == WM_QUIT {
            // Hand the quit to the outer loop once we are done here.
            quit = Some(i32::try_from(msg.wParam.0).unwrap_or(0));
            break;
        }
        if msg.hwnd.is_invalid() && msg.message == WM_APP {
            deferred += 1;
            continue;
        }
        // SAFETY: `msg` was just filled in by PeekMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // SAFETY: plain calls with no pointers; they only touch this thread's message queue.
    unsafe {
        let tid = GetCurrentThreadId();
        for _ in 0..deferred {
            let _ = PostThreadMessageW(tid, WM_APP, Default::default(), Default::default());
        }
        if let Some(code) = quit {
            windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(code);
        }
    }
}

/// `IStream` of one virtual file.
#[implement(IStream)]
struct VirtualStream {
    staged: Arc<StagedItems>,
    index: usize,
    size: u64,
    pos: Cell<u64>,
    file: RefCell<Option<File>>,
    failed: Cell<bool>,
}

impl VirtualStream {
    /// Reads up to `buf.len()` bytes at the current position, waiting for the file first.
    fn read_some(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.failed.get() {
            return Err(std::io::Error::other("earlier failure"));
        }
        let mut file = self.file.borrow_mut();
        if file.is_none() {
            match self.staged.open(self.index, &mut pump) {
                Ok(f) => *file = Some(f),
                Err(e) => {
                    self.failed.set(true);
                    tracing::debug!(%e, index = self.index, "virtual file not available");
                    return Err(e);
                }
            }
        }
        let Some(f) = file.as_mut() else {
            return Err(std::io::Error::other("no file"));
        };
        f.seek(SeekFrom::Start(self.pos.get()))?;
        let n = f.read(buf)?;
        self.pos.set(self.pos.get().saturating_add(n as u64));
        Ok(n)
    }
}

impl ISequentialStream_Impl for VirtualStream_Impl {
    fn Read(&self, pv: *mut core::ffi::c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        if pv.is_null() {
            return E_POINTER;
        }
        let want = usize::try_from(cb).unwrap_or(0);
        // SAFETY: the caller promises `pv` points to `cb` writable bytes (IStream::Read
        // contract); null was rejected above and `want == cb`.
        let buf = unsafe { std::slice::from_raw_parts_mut(pv.cast::<u8>(), want) };
        let res = self.read_some(buf);
        let n = match &res {
            Ok(n) => u32::try_from(*n).unwrap_or(0),
            Err(_) => 0,
        };
        if !pcbread.is_null() {
            // SAFETY: non-null out-pointer supplied by the caller for the byte count.
            unsafe { pcbread.write(n) };
        }
        match res {
            Ok(_) => S_OK,
            Err(_) => STG_E_READFAULT,
        }
    }

    fn Write(&self, _pv: *const core::ffi::c_void, _cb: u32, pcbwritten: *mut u32) -> HRESULT {
        if !pcbwritten.is_null() {
            // SAFETY: non-null out-pointer supplied by the caller.
            unsafe { pcbwritten.write(0) };
        }
        STG_E_INVALIDFUNCTION
    }
}

impl IStream_Impl for VirtualStream_Impl {
    fn Seek(
        &self,
        dlibmove: i64,
        dworigin: STREAM_SEEK,
        plibnewposition: *mut u64,
    ) -> windows::core::Result<()> {
        let base = match dworigin {
            STREAM_SEEK_SET => 0i128,
            STREAM_SEEK_CUR => i128::from(self.pos.get()),
            STREAM_SEEK_END => i128::from(self.size),
            _ => return Err(E_INVALIDARG.into()),
        };
        let target = base + i128::from(dlibmove);
        let target = u64::try_from(target).map_err(|_| windows::core::Error::from(E_INVALIDARG))?;
        self.pos.set(target);
        if !plibnewposition.is_null() {
            // SAFETY: non-null out-pointer supplied by the caller.
            unsafe { plibnewposition.write(target) };
        }
        Ok(())
    }

    fn SetSize(&self, _libnewsize: u64) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn CopyTo(
        &self,
        pstm: windows::core::Ref<'_, IStream>,
        cb: u64,
        pcbread: *mut u64,
        pcbwritten: *mut u64,
    ) -> windows::core::Result<()> {
        let dest: &IStream = pstm.ok()?;
        let mut left = cb;
        let (mut read, mut written) = (0u64, 0u64);
        let mut buf = vec![0u8; 256 * 1024];
        let mut result = Ok(());
        while left > 0 {
            let want = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
            let n = match self.read_some(&mut buf[..want]) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => {
                    result = Err(STG_E_READFAULT.into());
                    break;
                }
            };
            read += n as u64;
            let mut w = 0u32;
            // SAFETY: `buf[..n]` is valid for `n` bytes and `w` is a valid out-pointer.
            let hr = unsafe {
                dest.Write(
                    buf.as_ptr().cast(),
                    u32::try_from(n).unwrap_or(0),
                    Some(&mut w),
                )
            };
            written += u64::from(w);
            if hr.is_err() {
                result = Err(hr.into());
                break;
            }
            left -= n as u64;
        }
        // SAFETY: non-null out-pointers supplied by the caller.
        unsafe {
            if !pcbread.is_null() {
                pcbread.write(read);
            }
            if !pcbwritten.is_null() {
                pcbwritten.write(written);
            }
        }
        result
    }

    fn Commit(&self, _grfcommitflags: &STGC) -> windows::core::Result<()> {
        Ok(())
    }

    fn Revert(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn LockRegion(&self, _o: u64, _cb: u64, _t: &LOCKTYPE) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn UnlockRegion(&self, _o: u64, _cb: u64, _t: u32) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn Stat(&self, pstatstg: *mut STATSTG, _flag: &STATFLAG) -> windows::core::Result<()> {
        if pstatstg.is_null() {
            return Err(E_POINTER.into());
        }
        let st = STATSTG {
            r#type: STGTY_STREAM.0 as u32,
            cbSize: self.size,
            ..Default::default()
        };
        // SAFETY: non-null out-pointer supplied by the caller; the name pointer stays null
        // (STATFLAG_NONAME semantics), so nothing needs freeing.
        unsafe { pstatstg.write(st) };
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IStream> {
        Err(E_NOTIMPL.into())
    }
}

/// The clipboard `IDataObject` offering the peer's files.
#[implement(IDataObject, IDataObjectAsyncCapability)]
struct VirtualFiles {
    staged: Arc<StagedItems>,
    group: GroupDescriptor,
    fmt: Formats,
    async_mode: AtomicBool,
    in_operation: AtomicBool,
    performed_effect: AtomicU32,
}

impl VirtualFiles {
    fn new(staged: Arc<StagedItems>, fmt: Formats) -> Self {
        let group = build_group_descriptor(staged.items());
        Self {
            staged,
            group,
            fmt,
            async_mode: AtomicBool::new(true),
            in_operation: AtomicBool::new(false),
            performed_effect: AtomicU32::new(0),
        }
    }
}

impl Drop for VirtualFiles {
    fn drop(&mut self) {
        // OLE released the clipboard object (replaced by another copy, or we cleared it) and
        // no paste still holds it: nothing will read the staged bytes any more.
        self.staged.cancel();
    }
}

/// An `HGLOBAL` holding a copy of `bytes`.
fn global_from_bytes(bytes: &[u8]) -> windows::core::Result<HGLOBAL> {
    // SAFETY: GlobalAlloc with a size; the result is checked by `?`.
    let h = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) }?;
    // SAFETY: `h` is a live movable block of at least `bytes.len()` bytes.
    let p = unsafe { GlobalLock(h) };
    if p.is_null() {
        // SAFETY: `h` was allocated above and is not used afterwards.
        let _ = unsafe { GlobalFree(Some(h)) };
        return Err(E_FAIL.into());
    }
    // SAFETY: `p` points to at least `bytes.len()` writable bytes (locked above) and does not
    // overlap `bytes`; the block is unlocked right after.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(h);
    }
    Ok(h)
}

fn hglobal_medium(h: HGLOBAL) -> STGMEDIUM {
    STGMEDIUM {
        tymed: TYMED_HGLOBAL.0 as u32,
        u: STGMEDIUM_0 { hGlobal: h },
        pUnkForRelease: ManuallyDrop::new(None),
    }
}

impl VirtualFiles_Impl {
    fn format_list(&self) -> [FORMATETC; 3] {
        let mk = |cf: u16, lindex: i32, tymed: i32| FORMATETC {
            cfFormat: cf,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex,
            tymed: tymed as u32,
        };
        [
            mk(self.fmt.descriptor, -1, TYMED_HGLOBAL.0),
            mk(self.fmt.contents, -1, TYMED_ISTREAM.0),
            mk(self.fmt.preferred_effect, -1, TYMED_HGLOBAL.0),
        ]
    }

    /// Whether `fe` names a format and medium we can serve.
    fn check(&self, fe: &FORMATETC) -> Result<(), HRESULT> {
        use windows::Win32::Foundation::{DV_E_FORMATETC, DV_E_LINDEX, DV_E_TYMED};
        let cf = fe.cfFormat;
        if cf == self.fmt.descriptor || cf == self.fmt.preferred_effect {
            return if fe.tymed & (TYMED_HGLOBAL.0 as u32) != 0 {
                Ok(())
            } else {
                Err(DV_E_TYMED)
            };
        }
        if cf == self.fmt.contents {
            if fe.dwAspect != DVASPECT_CONTENT.0 {
                return Err(DV_E_FORMATETC);
            }
            if fe.tymed & (TYMED_ISTREAM.0 as u32) == 0 {
                return Err(DV_E_TYMED);
            }
            let ok = usize::try_from(fe.lindex)
                .ok()
                .is_some_and(|i| i < self.group.item_index.len());
            // lindex -1 is how callers probe support; serving needs a real index.
            return if ok || fe.lindex == -1 {
                Ok(())
            } else {
                Err(DV_E_LINDEX)
            };
        }
        Err(DV_E_FORMATETC)
    }
}

impl IDataObject_Impl for VirtualFiles_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        use windows::Win32::Foundation::DV_E_LINDEX;
        // SAFETY: a non-null FORMATETC pointer from the caller is valid for reads.
        let fe = unsafe { pformatetcin.as_ref() }
            .ok_or_else(|| windows::core::Error::from(E_POINTER))?;
        self.check(fe).map_err(windows::core::Error::from)?;
        let cf = fe.cfFormat;
        if cf == self.fmt.descriptor {
            return Ok(hglobal_medium(global_from_bytes(&self.group.bytes)?));
        }
        if cf == self.fmt.preferred_effect {
            let effect = DROPEFFECT_COPY.0.to_le_bytes();
            return Ok(hglobal_medium(global_from_bytes(&effect)?));
        }
        // FILECONTENTS: one stream per file, created now, filled when Explorer reads it.
        let slot =
            usize::try_from(fe.lindex).map_err(|_| windows::core::Error::from(DV_E_LINDEX))?;
        let item_idx = *self
            .group
            .item_index
            .get(slot)
            .ok_or_else(|| windows::core::Error::from(DV_E_LINDEX))?;
        let item = self
            .staged
            .items()
            .get(item_idx)
            .ok_or_else(|| windows::core::Error::from(DV_E_LINDEX))?;
        if item.is_dir {
            // Folders have no contents; the shell creates them from the descriptor.
            return Err(DV_E_LINDEX.into());
        }
        // Start pulling everything now so the files of a multi-file paste arrive in parallel
        // with Explorer's per-file reads.
        self.staged.begin();
        let stream: IStream = VirtualStream {
            staged: self.staged.clone(),
            index: item_idx,
            size: item.size,
            pos: Cell::new(0),
            file: RefCell::new(None),
            failed: Cell::new(false),
        }
        .into();
        Ok(STGMEDIUM {
            tymed: TYMED_ISTREAM.0 as u32,
            u: STGMEDIUM_0 {
                pstm: ManuallyDrop::new(Some(stream)),
            },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }

    fn GetDataHere(&self, _f: *const FORMATETC, _m: *mut STGMEDIUM) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        // SAFETY: a non-null FORMATETC pointer from the caller is valid for reads.
        let Some(fe) = (unsafe { pformatetc.as_ref() }) else {
            return E_POINTER;
        };
        match self.check(fe) {
            Ok(()) => S_OK,
            Err(hr) => hr,
        }
    }

    fn GetCanonicalFormatEtc(&self, _i: *const FORMATETC, _o: *mut FORMATETC) -> HRESULT {
        // DATA_S_SAMEFORMATETC: the canonical format is the same.
        HRESULT(0x0004_0130)
    }

    fn SetData(
        &self,
        pformatetc: *const FORMATETC,
        pmedium: *const STGMEDIUM,
        frelease: BOOL,
    ) -> windows::core::Result<()> {
        // SAFETY: non-null pointers from the caller are valid for reads.
        let (Some(fe), Some(med)) = (unsafe { pformatetc.as_ref() }, unsafe { pmedium.as_ref() })
        else {
            return Err(E_POINTER.into());
        };
        let known =
            fe.cfFormat == self.fmt.performed_effect || fe.cfFormat == self.fmt.paste_succeeded;
        if !known {
            return Err(E_NOTIMPL.into());
        }
        if fe.cfFormat == self.fmt.performed_effect && med.tymed == TYMED_HGLOBAL.0 as u32 {
            // SAFETY: tymed says the union holds an HGLOBAL; it is locked for the read and the
            // block is at least a DWORD (the documented payload of this format).
            unsafe {
                let h = med.u.hGlobal;
                let p = GlobalLock(h);
                if !p.is_null() {
                    self.performed_effect
                        .store(p.cast::<u32>().read_unaligned(), Ordering::SeqCst);
                    let _ = GlobalUnlock(h);
                }
            }
            tracing::debug!(
                effect = self.performed_effect.load(Ordering::SeqCst),
                "performed drop effect"
            );
        }
        if frelease.as_bool() {
            // SAFETY: with fRelease the callee owns the medium. The pointer was non-null above
            // and the shell does not use the medium afterwards.
            unsafe { ReleaseStgMedium(pmedium.cast_mut()) };
        }
        Ok(())
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> windows::core::Result<IEnumFORMATETC> {
        if i32::try_from(dwdirection).ok() != Some(DATADIR_GET.0) {
            return Err(E_NOTIMPL.into());
        }
        let list = self.format_list();
        // SAFETY: the slice is valid for the call; the shell copies it.
        unsafe { SHCreateStdEnumFmtEtc(&list) }
    }

    fn DAdvise(
        &self,
        _f: *const FORMATETC,
        _a: u32,
        _s: windows::core::Ref<'_, IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(windows::Win32::Foundation::OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _c: u32) -> windows::core::Result<()> {
        Err(windows::Win32::Foundation::OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(windows::Win32::Foundation::OLE_E_ADVISENOTSUPPORTED.into())
    }
}

impl IDataObjectAsyncCapability_Impl for VirtualFiles_Impl {
    fn SetAsyncMode(&self, fdoopasync: BOOL) -> windows::core::Result<()> {
        self.async_mode
            .store(fdoopasync.as_bool(), Ordering::SeqCst);
        Ok(())
    }

    fn GetAsyncMode(&self) -> windows::core::Result<BOOL> {
        Ok(self.async_mode.load(Ordering::SeqCst).into())
    }

    fn StartOperation(&self, _ctx: windows::core::Ref<'_, IBindCtx>) -> windows::core::Result<()> {
        self.in_operation.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn InOperation(&self) -> windows::core::Result<BOOL> {
        Ok(self.in_operation.load(Ordering::SeqCst).into())
    }

    fn EndOperation(
        &self,
        hresult: HRESULT,
        _ctx: windows::core::Ref<'_, IBindCtx>,
        dweffects: u32,
    ) -> windows::core::Result<()> {
        self.in_operation.store(false, Ordering::SeqCst);
        tracing::info!(
            ok = hresult.is_ok(),
            effects = dweffects,
            "Explorer finished pasting virtual files"
        );
        Ok(())
    }
}

enum Command {
    Set(Arc<StagedItems>),
    Clear,
}

type Queue = Arc<Mutex<VecDeque<Command>>>;

/// The real clipboard port: commands go to the STA thread.
pub(super) struct WindowsPort {
    queue: Queue,
    thread_id: Mutex<Option<u32>>,
    started: Mutex<bool>,
}

impl WindowsPort {
    pub(super) fn new() -> Self {
        Self {
            queue: Arc::default(),
            thread_id: Mutex::new(None),
            started: Mutex::new(false),
        }
    }

    /// Starts the STA thread on first use; returns its id.
    fn ensure_thread(&self) -> Option<u32> {
        let mut started = lock(&self.started);
        if *started {
            return *lock(&self.thread_id);
        }
        *started = true;
        let (tx, rx) = mpsc::channel();
        let queue = self.queue.clone();
        let spawned = std::thread::Builder::new()
            .name("tlp-shellclip".into())
            .spawn(move || run(&queue, &tx));
        if let Err(e) = spawned {
            tracing::warn!(%e, "cannot start the shell clipboard thread");
            return None;
        }
        let id = rx.recv().ok().flatten();
        *lock(&self.thread_id) = id;
        id
    }

    fn send(&self, cmd: Command) {
        let Some(tid) = self.ensure_thread() else {
            return;
        };
        lock(&self.queue).push_back(cmd);
        // SAFETY: plain call with ids and no pointers; the thread's queue exists (created
        // before its id was reported).
        let posted =
            unsafe { PostThreadMessageW(tid, WM_APP, Default::default(), Default::default()) };
        if let Err(e) = posted {
            tracing::warn!(%e, "cannot reach the shell clipboard thread");
        }
    }
}

impl ClipboardPort for WindowsPort {
    fn set(&self, offer: Arc<StagedItems>) {
        self.send(Command::Set(offer));
    }

    fn clear(&self) {
        self.send(Command::Clear);
    }
}

/// The STA thread: owns the clipboard object until told otherwise.
fn run(queue: &Queue, ready: &mpsc::Sender<Option<u32>>) {
    // SAFETY: first COM call on this thread; paired with OleUninitialize at the end.
    if let Err(e) = unsafe { OleInitialize(None) } {
        tracing::warn!(%e, "OleInitialize failed; no Explorer paste");
        let _ = ready.send(None);
        return;
    }
    let Some(fmt) = Formats::register() else {
        let _ = ready.send(None);
        // SAFETY: balances the successful OleInitialize above.
        unsafe { OleUninitialize() };
        return;
    };
    // Creating the thread's message queue before reporting the id makes PostThreadMessage safe.
    let mut probe = MSG::default();
    // SAFETY: `probe` is a valid out-pointer; PM_NOREMOVE leaves the queue untouched.
    let _ = unsafe { PeekMessageW(&mut probe, None, WM_APP, WM_APP, PM_NOREMOVE) };
    // SAFETY: no arguments.
    let tid = unsafe { GetCurrentThreadId() };
    let _ = ready.send(Some(tid));

    let mut current: Option<IDataObject> = None;
    loop {
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid out-pointer.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            break;
        }
        if msg.hwnd.is_invalid() && msg.message == WM_APP {
            loop {
                let next = lock(queue).pop_front();
                let Some(cmd) = next else { break };
                apply(cmd, fmt, &mut current);
            }
            continue;
        }
        // SAFETY: `msg` was just filled in by GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    release(&mut current);
    // SAFETY: balances the successful OleInitialize above.
    unsafe { OleUninitialize() };
}

fn apply(cmd: Command, fmt: Formats, current: &mut Option<IDataObject>) {
    match cmd {
        Command::Set(staged) => {
            let obj: IDataObject = VirtualFiles::new(staged, fmt).into();
            // Another program can hold the clipboard open for a moment: retry briefly.
            let mut last = None;
            for _ in 0..10 {
                // SAFETY: `obj` is a live COM object; OLE takes its own reference.
                match unsafe { OleSetClipboard(&obj) } {
                    Ok(()) => {
                        last = None;
                        break;
                    }
                    Err(e) => last = Some(e),
                }
                pump();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if let Some(e) = last {
                tracing::warn!(%e, "cannot place virtual files on the clipboard");
                return;
            }
            *current = Some(obj);
        }
        Command::Clear => release(current),
    }
}

/// Takes our object off the clipboard, but only if it is still the current one (never wipes
/// something the user copied afterwards).
fn release(current: &mut Option<IDataObject>) {
    let Some(obj) = current.take() else {
        return;
    };
    // SAFETY: `obj` is a live COM object.
    let ours = unsafe { OleIsCurrentClipboard(&obj) }.is_ok();
    if ours {
        // SAFETY: a null data object empties the clipboard (documented behaviour).
        if let Err(e) = unsafe { OleSetClipboard(None::<&IDataObject>) } {
            tracing::debug!(%e, "cannot empty the clipboard");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shellclip::{StageState, Stager};
    use std::sync::Arc;
    use tlp_desk_proto::TransferItem;

    struct Done;
    impl Stager for Done {
        fn begin(&self) {}
        fn state(&self) -> StageState {
            StageState::Finished
        }
        fn cancel(&self) {}
    }

    /// Replaces the user's clipboard with virtual files, reads them back through OLE like
    /// Explorer does, then restores the clipboard text. Run by hand only
    /// (`cargo test -p tlp-desk-files -- --ignored real_clipboard`). Skips (passes) when the
    /// clipboard holds files or images, which could not be restored.
    #[test]
    #[ignore = "overwrites the real clipboard (restores text)"]
    fn real_clipboard_round_trip() {
        use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;
        use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
        use windows::Win32::System::Ole::OleGetClipboard;

        // CF_BITMAP, CF_TIFF, CF_DIB, CF_ENHMETAFILE, CF_HDROP, CF_DIBV5.
        // SAFETY: plain format-id queries.
        let unrestorable = [2u32, 6, 8, 14, 15, 17]
            .iter()
            .any(|&f| unsafe { IsClipboardFormatAvailable(f) }.is_ok());
        if unrestorable {
            eprintln!("skipped: the clipboard holds files or an image that cannot be restored");
            return;
        }
        let saved = arboard::Clipboard::new()
            .ok()
            .and_then(|mut c| c.get_text().ok());

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("ملف.txt"), "محتوى").unwrap();
        let items = vec![TransferItem {
            rel_path: "ملف.txt".into(),
            size: "محتوى".len() as u64,
            modified_unix_ms: 1_700_000_000_000,
            is_dir: false,
        }];
        let staged = StagedItems::new(tmp.path().to_path_buf(), items.clone(), Arc::new(Done));
        let port = WindowsPort::new();
        port.set(staged);

        // The reader side: this thread is its own apartment, like Explorer's process.
        let reader = std::thread::spawn(move || {
            // SAFETY: balanced with OleUninitialize below; all pointers are locals.
            unsafe {
                OleInitialize(None).unwrap();
                let fmt = Formats::register().unwrap();
                // The clipboard is set asynchronously by the STA thread: poll for it.
                let mut obj = None;
                for _ in 0..50 {
                    if let Ok(o) = OleGetClipboard() {
                        if o.QueryGetData(&FORMATETC {
                            cfFormat: fmt.descriptor,
                            ptd: std::ptr::null_mut(),
                            dwAspect: DVASPECT_CONTENT.0,
                            lindex: -1,
                            tymed: TYMED_HGLOBAL.0 as u32,
                        })
                        .is_ok()
                        {
                            obj = Some(o);
                            break;
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                let obj = obj.expect("virtual files never appeared on the clipboard");
                let group = build_group_descriptor(&items);

                let mut fe = FORMATETC {
                    cfFormat: fmt.descriptor,
                    ptd: std::ptr::null_mut(),
                    dwAspect: DVASPECT_CONTENT.0,
                    lindex: -1,
                    tymed: TYMED_HGLOBAL.0 as u32,
                };
                let mut med = obj.GetData(&fe).expect("descriptor");
                let h = med.u.hGlobal;
                let p = GlobalLock(h);
                let got = std::slice::from_raw_parts(p.cast::<u8>(), group.bytes.len()).to_vec();
                let _ = GlobalUnlock(h);
                ReleaseStgMedium(&mut med);
                assert_eq!(got, group.bytes);

                fe.cfFormat = fmt.contents;
                fe.lindex = 0;
                fe.tymed = TYMED_ISTREAM.0 as u32;
                let med = obj.GetData(&fe).expect("contents");
                let stream = (*med.u.pstm).clone().expect("stream");
                let mut buf = [0u8; 64];
                let mut n = 0u32;
                let hr = stream.Read(buf.as_mut_ptr().cast(), 64, Some(&mut n));
                assert!(hr.is_ok());
                assert_eq!(&buf[..n as usize], "محتوى".as_bytes());
                drop(stream);
                drop(obj);
                OleUninitialize();
            }
        });
        let result = reader.join();

        port.clear();
        std::thread::sleep(std::time::Duration::from_millis(300));
        // Restore the user's text (or leave the clipboard empty if there was none).
        if let Ok(mut c) = arboard::Clipboard::new() {
            match saved {
                Some(t) => {
                    let _ = c.set_text(t);
                }
                None => {
                    let _ = c.clear();
                }
            }
        }
        result.expect("reader thread");
    }
}
