//! The app messages between the service and the user's GUI (carried as
//! `GuiEvent::App` / `GuiCommand::App` / `GuiReply::App` postcard bytes on the GUI pipe).
//!
//! Session ids are the **service's** host session ids; the GUI's mirror maps them to its own.
//! Settings and connection-log entries travel as JSON text (the same form as their files).

use serde::{Deserialize, Serialize};

use crate::backend::chat::ChatLine;
use crate::backend::files::{
    ConflictPrompt, FsError, TransferDir, TransferId, TransferStatus, TransferView,
};

/// Largest app message decoded (the pipe caps headers at 256 KB anyway).
pub const MAX_APP_MESSAGE: usize = 200 * 1024;

/// Service -> GUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UiEvent {
    /// A transfer of a host session was created or changed.
    Transfer { session: u64, view: WireTransfer },
    /// A viewer's upload waits for "replace or keep both?".
    Conflict { session: u64, prompt: WireConflict },
    /// That question was answered, timed out or its session ended.
    ConflictClosed { id: u64 },
    /// A chat message from a viewer.
    Chat { session: u64, line: WireChatLine },
    /// Chat was turned on or off for a session.
    ChatAvailability { session: u64, allowed: bool },
    /// This PC's screen is (no longer) blanked for a viewer.
    Privacy { session: u64, active: bool },
}

/// GUI -> service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UiCommand {
    /// Send a chat message to the viewer of a host session.
    SendChat { session: u64, text: String },
    /// Answer an upload conflict (`None` declines the upload).
    ResolveConflict {
        id: u64,
        answer: Option<WireResolution>,
    },
    /// Cancel a transfer.
    CancelTransfer { id: u64 },
    /// Validate, apply live and save the machine-wide host settings (JSON of
    /// `HostSettings`).
    ApplySettings { json: String },
    /// The machine-wide host settings in effect (JSON).
    GetSettings,
}

/// Service -> GUI answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UiReply {
    Ok,
    /// The settings in effect, as JSON.
    Settings(String),
    /// Refused or failed; the text is for the log.
    Error(String),
}

/// [`TransferView`] on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireTransfer {
    pub id: u64,
    pub sending: bool,
    pub label: String,
    pub status: WireStatus,
    pub bytes_done: u64,
    pub total_bytes: u64,
    pub files_done: u32,
    pub total_files: u32,
    pub failed_files: u32,
    pub bytes_per_sec: u64,
    pub local_dest: Option<String>,
    pub local: bool,
    pub started_unix_ms: u64,
}

/// [`TransferStatus`] on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireStatus {
    Preparing,
    Running,
    Paused,
    Completed,
    Cancelled,
    Failed(FsError),
    Disconnected,
}

impl From<&TransferView> for WireTransfer {
    fn from(v: &TransferView) -> Self {
        Self {
            id: v.id.0,
            sending: v.dir == TransferDir::Send,
            label: v.label.clone(),
            status: match &v.status {
                TransferStatus::Preparing => WireStatus::Preparing,
                TransferStatus::Running => WireStatus::Running,
                TransferStatus::Paused => WireStatus::Paused,
                TransferStatus::Completed => WireStatus::Completed,
                TransferStatus::Cancelled => WireStatus::Cancelled,
                TransferStatus::Failed(e) => WireStatus::Failed(e.clone()),
                TransferStatus::Disconnected => WireStatus::Disconnected,
            },
            bytes_done: v.bytes_done,
            total_bytes: v.total_bytes,
            files_done: v.files_done,
            total_files: v.total_files,
            failed_files: v.failed_files,
            bytes_per_sec: v.bytes_per_sec,
            local_dest: v
                .local_dest
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            local: v.local,
            started_unix_ms: v.started_unix_ms,
        }
    }
}

impl WireTransfer {
    /// The UI view, with the transfer id `id` (the mirror's own id space).
    pub fn view(&self, id: TransferId) -> TransferView {
        TransferView {
            id,
            dir: if self.sending {
                TransferDir::Send
            } else {
                TransferDir::Receive
            },
            label: self.label.clone(),
            status: match &self.status {
                WireStatus::Preparing => TransferStatus::Preparing,
                WireStatus::Running => TransferStatus::Running,
                WireStatus::Paused => TransferStatus::Paused,
                WireStatus::Completed => TransferStatus::Completed,
                WireStatus::Cancelled => TransferStatus::Cancelled,
                WireStatus::Failed(e) => TransferStatus::Failed(e.clone()),
                WireStatus::Disconnected => TransferStatus::Disconnected,
            },
            bytes_done: self.bytes_done,
            total_bytes: self.total_bytes,
            files_done: self.files_done,
            total_files: self.total_files,
            failed_files: self.failed_files,
            bytes_per_sec: self.bytes_per_sec,
            local_dest: self.local_dest.as_ref().map(std::path::PathBuf::from),
            local: self.local,
            started_unix_ms: self.started_unix_ms,
        }
    }
}

/// One clashing file of a [`WireConflict`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFileConflict {
    pub index: u32,
    pub rel_path: String,
    pub existing_size: u64,
    pub existing_modified_unix_ms: i64,
    pub incoming_size: u64,
    pub incoming_modified_unix_ms: i64,
    pub partial_bytes: Option<u64>,
}

/// [`ConflictPrompt`] on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireConflict {
    pub id: u64,
    pub label: String,
    pub dest_dir: String,
    pub total: u32,
    pub files: Vec<WireFileConflict>,
}

impl From<&ConflictPrompt> for WireConflict {
    fn from(p: &ConflictPrompt) -> Self {
        Self {
            id: p.id.0,
            label: p.label.clone(),
            dest_dir: p.dest_dir.clone(),
            total: p.total,
            files: p
                .files
                .iter()
                .map(|f| WireFileConflict {
                    index: f.index,
                    rel_path: f.rel_path.clone(),
                    existing_size: f.existing_size,
                    existing_modified_unix_ms: f.existing_modified_unix_ms,
                    incoming_size: f.incoming_size,
                    incoming_modified_unix_ms: f.incoming_modified_unix_ms,
                    partial_bytes: f.partial_bytes,
                })
                .collect(),
        }
    }
}

impl WireConflict {
    /// The UI prompt, with the transfer id `id` (the mirror's own id space).
    pub fn prompt(&self, id: TransferId) -> ConflictPrompt {
        ConflictPrompt {
            id,
            local: false,
            label: self.label.clone(),
            dest_dir: self.dest_dir.clone(),
            total: self.total,
            files: self
                .files
                .iter()
                .map(|f| tlp_desk_files::FileConflict {
                    index: f.index,
                    rel_path: f.rel_path.clone(),
                    existing_size: f.existing_size,
                    existing_modified_unix_ms: f.existing_modified_unix_ms,
                    incoming_size: f.incoming_size,
                    incoming_modified_unix_ms: f.incoming_modified_unix_ms,
                    partial_bytes: f.partial_bytes,
                })
                .collect(),
        }
    }
}

/// A conflict choice on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireChoice {
    KeepBoth,
    Overwrite,
    Skip,
    Resume,
}

impl From<tlp_desk_files::ConflictChoice> for WireChoice {
    fn from(c: tlp_desk_files::ConflictChoice) -> Self {
        use tlp_desk_files::ConflictChoice as C;
        match c {
            C::KeepBoth => Self::KeepBoth,
            C::Overwrite => Self::Overwrite,
            C::Skip => Self::Skip,
            C::Resume => Self::Resume,
        }
    }
}

impl From<WireChoice> for tlp_desk_files::ConflictChoice {
    fn from(c: WireChoice) -> Self {
        match c {
            WireChoice::KeepBoth => Self::KeepBoth,
            WireChoice::Overwrite => Self::Overwrite,
            WireChoice::Skip => Self::Skip,
            WireChoice::Resume => Self::Resume,
        }
    }
}

/// A conflict answer on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireResolution {
    pub default: WireChoice,
    pub per_file: Vec<(u32, WireChoice)>,
}

impl From<&tlp_desk_files::ConflictResolution> for WireResolution {
    fn from(r: &tlp_desk_files::ConflictResolution) -> Self {
        Self {
            default: r.default.into(),
            per_file: r.per_file.iter().map(|(i, c)| (*i, (*c).into())).collect(),
        }
    }
}

impl From<&WireResolution> for tlp_desk_files::ConflictResolution {
    fn from(r: &WireResolution) -> Self {
        Self {
            default: r.default.into(),
            per_file: r.per_file.iter().map(|(i, c)| (*i, (*c).into())).collect(),
        }
    }
}

/// [`ChatLine`] on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireChatLine {
    pub id: u64,
    pub mine: bool,
    pub text: String,
    pub at_unix_ms: u64,
}

impl From<&ChatLine> for WireChatLine {
    fn from(l: &ChatLine) -> Self {
        Self {
            id: l.id,
            mine: l.mine,
            text: l.text.clone(),
            at_unix_ms: l.at_unix_ms,
        }
    }
}

impl From<&WireChatLine> for ChatLine {
    fn from(l: &WireChatLine) -> Self {
        Self {
            id: l.id,
            mine: l.mine,
            text: l.text.clone(),
            at_unix_ms: l.at_unix_ms,
        }
    }
}

/// Encodes an app message (an encoding failure gives an empty message, which decodes to
/// nothing).
pub fn encode<T: Serialize>(msg: &T) -> Vec<u8> {
    postcard::to_stdvec(msg).unwrap_or_default()
}

/// Decodes an app message; `None` when it is malformed or too large.
pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    if bytes.len() > MAX_APP_MESSAGE {
        return None;
    }
    postcard::from_bytes(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(m: T) {
        assert_eq!(decode::<T>(&encode(&m)), Some(m));
    }

    #[test]
    fn app_messages_roundtrip_with_arabic() {
        roundtrip(UiEvent::Transfer {
            session: 4,
            view: WireTransfer {
                id: 9,
                sending: false,
                label: "تقارير الربع الأول".into(),
                status: WireStatus::Failed(FsError::PermissionDenied),
                bytes_done: 10,
                total_bytes: 20,
                files_done: 1,
                total_files: 2,
                failed_files: 1,
                bytes_per_sec: 5,
                local_dest: Some(r"C:\Users\تامر\Desktop".into()),
                local: false,
                started_unix_ms: 1,
            },
        });
        roundtrip(UiEvent::Conflict {
            session: 4,
            prompt: WireConflict {
                id: 9,
                label: "صور".into(),
                dest_dir: r"C:\Users\تامر\Desktop".into(),
                total: 3,
                files: vec![WireFileConflict {
                    index: 0,
                    rel_path: "صور/عطلة.jpg".into(),
                    existing_size: 1,
                    existing_modified_unix_ms: -1,
                    incoming_size: 2,
                    incoming_modified_unix_ms: 3,
                    partial_bytes: Some(1),
                }],
            },
        });
        roundtrip(UiEvent::Chat {
            session: 4,
            line: WireChatLine {
                id: 1,
                mine: false,
                text: "مرحبا\u{200f} PC-01".into(),
                at_unix_ms: 7,
            },
        });
        roundtrip(UiCommand::SendChat {
            session: 4,
            text: "أهلاً".into(),
        });
        roundtrip(UiCommand::ResolveConflict {
            id: 9,
            answer: Some(WireResolution {
                default: WireChoice::KeepBoth,
                per_file: vec![(0, WireChoice::Overwrite)],
            }),
        });
        roundtrip(UiReply::Settings("{\"allow_one_time_code\":false}".into()));
    }

    #[test]
    fn malformed_or_huge_messages_decode_to_nothing() {
        assert_eq!(decode::<UiCommand>(&[200, 1, 2]), None);
        assert_eq!(decode::<UiCommand>(&vec![0; MAX_APP_MESSAGE + 1]), None);
        assert_eq!(decode::<UiEvent>(&[]), None);
    }

    #[test]
    fn views_survive_the_wire() {
        let v = TransferView {
            id: TransferId(3),
            dir: TransferDir::Receive,
            label: "ملف.txt".into(),
            status: TransferStatus::Running,
            bytes_done: 1,
            total_bytes: 2,
            files_done: 0,
            total_files: 1,
            failed_files: 0,
            bytes_per_sec: 9,
            local_dest: Some(std::path::PathBuf::from(r"C:\Users\سارة\Desktop")),
            local: false,
            started_unix_ms: 5,
        };
        let back = WireTransfer::from(&v).view(TransferId(3));
        assert_eq!(back, v);
    }
}
