//! GUI side of the popup: one `TLP-Desk.exe --accept-prompt` child per incoming request,
//! answered without blocking the UI thread.
//!
//! [`AcceptPopups`] is what the app holds. It owns a [`PromptSpawner`] (the real one starts
//! processes; tests plug in a fake) and the open [`PromptHandle`]s keyed by request.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use super::protocol::{decode, encode, read_line, PromptDecision, PromptRequest, CANCEL_LINE};
use super::run::ACCEPT_PROMPT_ARG;
use crate::backend::{RepaintFn, RequestId};

/// The state of one popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptPoll {
    /// Still on screen.
    Pending,
    /// The user (or the timeout) answered.
    Done(PromptDecision),
    /// The popup went away without an answer (crashed, could not open a window).
    Failed,
}

/// One running popup.
pub trait PromptHandle: Send {
    /// Non-blocking: has it answered?
    fn poll(&mut self) -> PromptPoll;
    /// Close it without an answer (the viewer gave up). No answer is reported afterwards.
    fn cancel(&mut self);
}

/// Starts popups.
pub trait PromptSpawner: Send {
    /// Show `request` in a new popup.
    fn spawn(&self, request: &PromptRequest) -> std::io::Result<Box<dyn PromptHandle>>;
}

/// What became of a popup, reported by [`AcceptPopups::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupResult {
    Decided(PromptDecision),
    /// Fall back to the in-app dialog.
    Failed,
}

/// The popups the app currently shows, one per incoming request.
#[derive(Default)]
pub struct AcceptPopups {
    spawner: Option<Box<dyn PromptSpawner>>,
    open: HashMap<RequestId, Box<dyn PromptHandle>>,
}

impl std::fmt::Debug for AcceptPopups {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcceptPopups")
            .field("enabled", &self.spawner.is_some())
            .field("open", &self.open.len())
            .finish()
    }
}

impl AcceptPopups {
    /// Popups switched on, started by `spawner`.
    pub fn new(spawner: Box<dyn PromptSpawner>) -> Self {
        Self {
            spawner: Some(spawner),
            open: HashMap::new(),
        }
    }

    /// Whether popups can be shown at all (tests and screenshots run without).
    pub fn is_enabled(&self) -> bool {
        self.spawner.is_some()
    }

    /// Show `prompt` for `request`. `false` when popups are off or the popup could not start
    /// (the caller then uses the in-app dialog).
    pub fn open(&mut self, request: RequestId, prompt: &PromptRequest) -> bool {
        let Some(spawner) = &self.spawner else {
            return false;
        };
        match spawner.spawn(prompt) {
            Ok(handle) => {
                if let Some(mut old) = self.open.insert(request, handle) {
                    old.cancel();
                }
                true
            }
            Err(e) => {
                tracing::warn!(%e, "could not start the accept popup; using the in-app dialog");
                false
            }
        }
    }

    /// Whether `request` is on screen in a popup.
    pub fn showing(&self, request: RequestId) -> bool {
        self.open.contains_key(&request)
    }

    /// Close the popup of `request` (viewer gave up, or answered elsewhere).
    pub fn cancel(&mut self, request: RequestId) {
        if let Some(mut h) = self.open.remove(&request) {
            h.cancel();
        }
    }

    /// Collect finished popups.
    pub fn poll(&mut self) -> Vec<(RequestId, PopupResult)> {
        let mut out = Vec::new();
        self.open.retain(|id, h| match h.poll() {
            PromptPoll::Pending => true,
            PromptPoll::Done(d) => {
                out.push((*id, PopupResult::Decided(d)));
                false
            }
            PromptPoll::Failed => {
                out.push((*id, PopupResult::Failed));
                false
            }
        });
        out
    }
}

impl Drop for AcceptPopups {
    fn drop(&mut self) {
        for (_, mut h) in self.open.drain() {
            h.cancel();
        }
    }
}

/// Starts `TLP-Desk.exe --accept-prompt` children.
pub struct ProcessSpawner {
    exe: PathBuf,
    repaint: RepaintFn,
}

impl ProcessSpawner {
    /// Popups run this same executable. `repaint` wakes the UI when an answer arrives.
    pub fn current(repaint: RepaintFn) -> std::io::Result<Self> {
        Ok(Self {
            exe: std::env::current_exe()?,
            repaint,
        })
    }
}

impl PromptSpawner for ProcessSpawner {
    fn spawn(&self, request: &PromptRequest) -> std::io::Result<Box<dyn PromptHandle>> {
        let line = encode(request).map_err(std::io::Error::other)?;
        let mut cmd = Command::new(&self.exe);
        cmd.arg(ACCEPT_PROMPT_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            // CREATE_NO_WINDOW: never flash a console (debug builds are console programs).
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn()?;
        super::platform::allow_foreground(child.id());
        let mut stdin = child.stdin.take();
        if let Some(s) = stdin.as_mut() {
            writeln!(s, "{line}")?;
            s.flush()?;
        }
        let (tx, rx) = mpsc::channel();
        if let Some(stdout) = child.stdout.take() {
            let repaint = self.repaint.clone();
            std::thread::Builder::new()
                .name("tlp-desk-accept-popup".into())
                .spawn(move || {
                    let mut r = std::io::BufReader::new(stdout);
                    if let Ok(Some(line)) = read_line(&mut r) {
                        match decode::<PromptDecision>(&line) {
                            Ok(d) => {
                                let _ = tx.send(d);
                            }
                            Err(e) => tracing::warn!(%e, "unreadable accept popup answer"),
                        }
                    }
                    drop(tx);
                    repaint();
                })?;
        }
        Ok(Box::new(ProcessHandle {
            child: Some(child),
            stdin,
            rx,
        }))
    }
}

struct ProcessHandle {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    rx: mpsc::Receiver<PromptDecision>,
}

impl PromptHandle for ProcessHandle {
    fn poll(&mut self) -> PromptPoll {
        match self.rx.try_recv() {
            Ok(d) => PromptPoll::Done(d),
            Err(mpsc::TryRecvError::Empty) => PromptPoll::Pending,
            Err(mpsc::TryRecvError::Disconnected) => PromptPoll::Failed,
        }
    }

    fn cancel(&mut self) {
        if let Some(mut s) = self.stdin.take() {
            let _ = writeln!(s, "{CANCEL_LINE}");
            let _ = s.flush();
        }
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // Closing stdin also cancels; then reap the child, killing it if it hangs.
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            let _ = std::thread::Builder::new()
                .name("tlp-desk-accept-popup-reap".into())
                .spawn(move || {
                    for _ in 0..30 {
                        if !matches!(child.try_wait(), Ok(None)) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                });
        }
    }
}

/// A scripted popup for tests: answers with whatever the shared slot holds.
#[cfg(test)]
pub(crate) mod fake {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Shared view of every fake popup: requests seen, cancels, and the next answer.
    #[derive(Default)]
    pub struct FakeLog {
        pub requests: Vec<PromptRequest>,
        pub cancelled: usize,
        /// What open popups report on their next poll.
        pub answer: Option<PromptPoll>,
        /// Make `spawn` fail.
        pub fail_spawn: bool,
    }

    pub struct FakeSpawner(pub Arc<Mutex<FakeLog>>);

    struct FakeHandle(Arc<Mutex<FakeLog>>);

    impl PromptSpawner for FakeSpawner {
        fn spawn(&self, request: &PromptRequest) -> std::io::Result<Box<dyn PromptHandle>> {
            let mut log = self.0.lock().unwrap();
            if log.fail_spawn {
                return Err(std::io::Error::other("no popup"));
            }
            log.requests.push(request.clone());
            Ok(Box::new(FakeHandle(Arc::clone(&self.0))))
        }
    }

    impl PromptHandle for FakeHandle {
        fn poll(&mut self) -> PromptPoll {
            self.0
                .lock()
                .unwrap()
                .answer
                .clone()
                .unwrap_or(PromptPoll::Pending)
        }
        fn cancel(&mut self) {
            self.0.lock().unwrap().cancelled += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::fake::{FakeLog, FakeSpawner};
    use super::*;
    use crate::prompt::protocol::Outcome;

    fn popups() -> (AcceptPopups, Arc<Mutex<FakeLog>>) {
        let log = Arc::new(Mutex::new(FakeLog::default()));
        (
            AcceptPopups::new(Box::new(FakeSpawner(Arc::clone(&log)))),
            log,
        )
    }

    #[test]
    fn disabled_popups_never_open() {
        let mut p = AcceptPopups::default();
        assert!(!p.is_enabled());
        assert!(!p.open(RequestId(1), &PromptRequest::default()));
        assert!(p.poll().is_empty());
    }

    #[test]
    fn decision_is_reported_once_and_closes_the_popup() {
        let (mut p, log) = popups();
        assert!(p.open(RequestId(7), &PromptRequest::default()));
        assert!(p.showing(RequestId(7)));
        assert!(p.poll().is_empty());
        let d = PromptDecision {
            outcome: Outcome::Accept,
            permissions: Default::default(),
            trust: false,
        };
        log.lock().unwrap().answer = Some(PromptPoll::Done(d.clone()));
        assert_eq!(p.poll(), vec![(RequestId(7), PopupResult::Decided(d))]);
        assert!(!p.showing(RequestId(7)));
        assert!(p.poll().is_empty());
    }

    #[test]
    fn cancel_closes_without_answer() {
        let (mut p, log) = popups();
        p.open(RequestId(1), &PromptRequest::default());
        p.cancel(RequestId(1));
        assert_eq!(log.lock().unwrap().cancelled, 1);
        assert!(!p.showing(RequestId(1)));
        // A late answer is never reported for a cancelled popup.
        log.lock().unwrap().answer = Some(PromptPoll::Failed);
        assert!(p.poll().is_empty());
    }

    #[test]
    fn spawn_failure_and_crash_fall_back() {
        let (mut p, log) = popups();
        log.lock().unwrap().fail_spawn = true;
        assert!(!p.open(RequestId(1), &PromptRequest::default()));
        log.lock().unwrap().fail_spawn = false;
        assert!(p.open(RequestId(2), &PromptRequest::default()));
        log.lock().unwrap().answer = Some(PromptPoll::Failed);
        assert_eq!(p.poll(), vec![(RequestId(2), PopupResult::Failed)]);
    }

    #[test]
    fn dropping_cancels_open_popups() {
        let (mut p, log) = popups();
        p.open(RequestId(1), &PromptRequest::default());
        p.open(RequestId(2), &PromptRequest::default());
        drop(p);
        assert_eq!(log.lock().unwrap().cancelled, 2);
    }
}
