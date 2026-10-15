//! Viewer side: send [`FsOp`]s to the host and wait for the answers.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tlp_desk_net::Session;
use tlp_desk_proto::{FsEntry, FsError, FsOp, FsReply, FsRoot, Message, Permissions};
use tokio::sync::{oneshot, watch};

/// How long [`FsClient::request`] waits for an answer. Generous: listing a huge folder or
/// deleting a deep tree on a slow disk can take a while.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a remote file-system request failed on the viewer side.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FsClientError {
    /// The host answered with an error (not found, access denied, ...).
    #[error("the remote device answered: {0:?}")]
    Remote(FsError),
    /// The session does not (or no longer) hold the `FILES` permission. Not sent: the host
    /// would drop the request.
    #[error("the host does not allow file access in this session")]
    NoPermission,
    /// No answer within the timeout.
    #[error("the remote device did not answer in time")]
    Timeout,
    /// The session ended.
    #[error("the session has ended")]
    Closed,
    /// The request could not be sent.
    #[error("cannot send the request: {0}")]
    Send(String),
    /// The host answered with a reply of the wrong kind (a buggy or hostile peer).
    #[error("the remote device sent an unexpected answer")]
    UnexpectedReply,
}

type Pending = HashMap<u64, oneshot::Sender<Result<FsReply, FsError>>>;

/// Handle for one viewer session's remote file system. Cheap to clone; every method may be
/// called from any task. Obtain it from [`super::FsFeature::client`].
#[derive(Clone)]
pub struct FsClient {
    inner: Arc<Inner>,
}

struct Inner {
    session: Arc<Session>,
    permissions: watch::Receiver<Permissions>,
    next_id: AtomicU64,
    pending: Mutex<Pending>,
    closed: AtomicBool,
}

impl std::fmt::Debug for FsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsClient")
            .field("pending", &self.pending().len())
            .field("closed", &self.inner.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

/// Removes a request from the pending map when its future ends (answer, timeout or drop).
struct PendingGuard<'a> {
    client: &'a FsClient,
    id: u64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.client.pending().remove(&self.id);
    }
}

impl FsClient {
    pub(crate) fn new(session: Arc<Session>, permissions: watch::Receiver<Permissions>) -> Self {
        Self {
            inner: Arc::new(Inner {
                session,
                permissions,
                next_id: AtomicU64::new(1),
                pending: Mutex::new(HashMap::new()),
                closed: AtomicBool::new(false),
            }),
        }
    }

    fn pending(&self) -> MutexGuard<'_, Pending> {
        self.inner
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn allowed(&self) -> bool {
        self.inner.permissions.borrow().contains(Permissions::FILES)
    }

    /// Whether the session currently holds the `FILES` permission.
    pub fn has_permission(&self) -> bool {
        self.allowed()
    }

    /// Runs `op` on the host with [`DEFAULT_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// See [`FsClientError`].
    pub async fn request(&self, op: FsOp) -> Result<FsReply, FsClientError> {
        self.request_timeout(op, DEFAULT_TIMEOUT).await
    }

    /// Runs `op` on the host and waits at most `timeout` for the answer. Fails at once with
    /// [`FsClientError::NoPermission`] if the session lacks `FILES`, and while waiting if the
    /// host revokes it.
    ///
    /// # Errors
    ///
    /// See [`FsClientError`].
    pub async fn request_timeout(
        &self,
        op: FsOp,
        timeout: Duration,
    ) -> Result<FsReply, FsClientError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(FsClientError::Closed);
        }
        if !self.allowed() {
            return Err(FsClientError::NoPermission);
        }
        let req_id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending().insert(req_id, tx);
        let _guard = PendingGuard {
            client: self,
            id: req_id,
        };
        self.inner
            .session
            .send(&Message::FsRequest { req_id, op })
            .await
            .map_err(|e| FsClientError::Send(e.to_string()))?;
        let mut perms = self.inner.permissions.clone();
        let revoked = async move {
            // Resolves only when FILES is gone. If the permission feed ends (session over),
            // the answer channel closes too and decides the outcome.
            if perms
                .wait_for(|p| !p.contains(Permissions::FILES))
                .await
                .is_err()
            {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            answer = tokio::time::timeout(timeout, rx) => match answer {
                Err(_) => Err(FsClientError::Timeout),
                Ok(Err(_)) => Err(FsClientError::Closed),
                Ok(Ok(result)) => result.map_err(FsClientError::Remote),
            },
            () = revoked => Err(FsClientError::NoPermission),
        }
    }

    /// The host's drives and known folders.
    ///
    /// # Errors
    ///
    /// See [`FsClientError`].
    pub async fn roots(&self) -> Result<Vec<FsRoot>, FsClientError> {
        match self.request(FsOp::Roots).await? {
            FsReply::Roots(r) => Ok(r),
            _ => Err(FsClientError::UnexpectedReply),
        }
    }

    /// Entries of a host directory, folders first.
    ///
    /// # Errors
    ///
    /// See [`FsClientError`].
    pub async fn list_dir(&self, path: &str) -> Result<Vec<FsEntry>, FsClientError> {
        match self.request(FsOp::ListDir { path: path.into() }).await? {
            FsReply::Entries(e) => Ok(e),
            _ => Err(FsClientError::UnexpectedReply),
        }
    }

    /// One host file or folder.
    ///
    /// # Errors
    ///
    /// See [`FsClientError`].
    pub async fn stat(&self, path: &str) -> Result<FsEntry, FsClientError> {
        match self.request(FsOp::Stat { path: path.into() }).await? {
            FsReply::Entry(e) => Ok(e),
            _ => Err(FsClientError::UnexpectedReply),
        }
    }

    /// Creates a host folder (and missing parents); fails if it exists.
    ///
    /// # Errors
    ///
    /// See [`FsClientError`].
    pub async fn make_dir(&self, path: &str) -> Result<(), FsClientError> {
        self.done(FsOp::MakeDir { path: path.into() }).await
    }

    /// Renames or moves within one host volume; never overwrites.
    ///
    /// # Errors
    ///
    /// See [`FsClientError`].
    pub async fn rename(&self, from: &str, to: &str) -> Result<(), FsClientError> {
        self.done(FsOp::Rename {
            from: from.into(),
            to: to.into(),
        })
        .await
    }

    /// Deletes a host file or folder (permanently; `recursive` for a non-empty folder).
    ///
    /// # Errors
    ///
    /// See [`FsClientError`].
    pub async fn delete(&self, path: &str, recursive: bool) -> Result<(), FsClientError> {
        self.done(FsOp::Delete {
            path: path.into(),
            recursive,
        })
        .await
    }

    async fn done(&self, op: FsOp) -> Result<(), FsClientError> {
        match self.request(op).await? {
            FsReply::Done => Ok(()),
            _ => Err(FsClientError::UnexpectedReply),
        }
    }

    /// Delivers the host's answer to the waiting request (unknown or late ids are ignored).
    pub(crate) fn resolve(&self, req_id: u64, result: Result<FsReply, FsError>) {
        match self.pending().remove(&req_id) {
            Some(tx) => {
                let _ = tx.send(result);
            }
            None => tracing::debug!(req_id, "answer for an unknown or expired fs request"),
        }
    }

    /// The session ended: fail every waiting request and refuse new ones.
    pub(crate) fn close(&self) {
        self.inner.closed.store(true, Ordering::Release);
        // Dropping the senders wakes every waiter with `Closed`.
        self.pending().clear();
    }
}
