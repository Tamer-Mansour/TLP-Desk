//! Property tests for the wire format: every [`Message`] round-trips, and the decoders never
//! panic and never buffer more than one frame on random, truncated, mutated or oversized input.
//!
//! The `*_long` tests are the same properties with many more cases (a stable-Rust stand-in for
//! a fuzzer); they are `#[ignore]`d. Run them with
//! `cargo test --release -p tlp-desk-proto --test properties -- --ignored`.
//! `PROPTEST_CASES=<n>` overrides the case count of the normal tests.

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::strategy::Union;
use tlp_desk_proto::*;

// ------------------------------------------------------------------------------------------
// Strategies
// ------------------------------------------------------------------------------------------

/// Any text, biased towards Arabic with RTL marks, joiners and digits (where bidi bugs live).
fn text() -> BoxedStrategy<String> {
    prop_oneof![
        any::<String>(),
        "[\u{0621}-\u{064A}\u{0660}-\u{0669}\u{200C}\u{200D}\u{200F} a-zA-Z0-9._-]{0,48}",
        Just(String::new()),
    ]
    .boxed()
}

fn bytes(max: usize) -> impl Strategy<Value = Vec<u8>> {
    vec(any::<u8>(), 0..=max)
}

fn permissions() -> impl Strategy<Value = Permissions> {
    any::<u16>().prop_map(Permissions)
}

fn role() -> impl Strategy<Value = Role> {
    prop_oneof![Just(Role::Host), Just(Role::Viewer)]
}

fn hello() -> impl Strategy<Value = Hello> {
    (any::<u32>(), any::<u32>(), text(), role()).prop_map(
        |(protocol_version, device_id, device_name, role)| Hello {
            protocol_version,
            device_id,
            device_name,
            role,
        },
    )
}

fn auth_request() -> impl Strategy<Value = AuthRequest> {
    prop_oneof![
        Just(AuthRequest::AcceptPrompt),
        bytes(64).prop_map(|spake2_msg| AuthRequest::OneTimeCode { spake2_msg }),
        bytes(64).prop_map(|spake2_msg| AuthRequest::Password { spake2_msg }),
        bytes(64).prop_map(|mac| AuthRequest::KeyConfirm { mac }),
        bytes(64).prop_map(|token| AuthRequest::ReconnectTicket { token }),
    ]
}

fn reject_reason() -> impl Strategy<Value = RejectReason> {
    prop_oneof![
        Just(RejectReason::UserDeclined),
        Just(RejectReason::WrongCredentials),
        Just(RejectReason::RateLimited),
        Just(RejectReason::NotAllowed),
        Just(RejectReason::VersionMismatch),
    ]
}

fn auth_result() -> impl Strategy<Value = AuthResult> {
    prop_oneof![
        bytes(64).prop_map(|spake2_msg| AuthResult::Spake2Reply { spake2_msg }),
        permissions().prop_map(|permissions| AuthResult::Accepted { permissions }),
        reject_reason().prop_map(|reason| AuthResult::Rejected { reason }),
        bytes(64).prop_map(|mac| AuthResult::KeyConfirm { mac }),
    ]
}

fn video_frame() -> impl Strategy<Value = VideoFrame> {
    (
        any::<u64>(),
        any::<u64>(),
        any::<u32>(),
        any::<u32>(),
        any::<bool>(),
        bytes(512),
    )
        .prop_map(
            |(seq, timestamp_us, width, height, keyframe, data)| VideoFrame {
                seq,
                timestamp_us,
                width,
                height,
                codec: Codec::H264,
                keyframe,
                data,
            },
        )
}

fn mouse_button() -> impl Strategy<Value = MouseButton> {
    prop_oneof![
        Just(MouseButton::Left),
        Just(MouseButton::Right),
        Just(MouseButton::Middle),
        Just(MouseButton::Back),
        Just(MouseButton::Forward),
    ]
}

fn input_event() -> impl Strategy<Value = InputEvent> {
    prop_oneof![
        (any::<u16>(), any::<u16>()).prop_map(|(x, y)| InputEvent::MouseMove { x, y }),
        (mouse_button(), any::<bool>())
            .prop_map(|(button, down)| InputEvent::MouseButton { button, down }),
        (any::<i16>(), any::<i16>()).prop_map(|(dx, dy)| InputEvent::MouseWheel { dx, dy }),
        (any::<u16>(), any::<bool>(), any::<bool>()).prop_map(|(scancode, extended, down)| {
            InputEvent::Key {
                scancode,
                extended,
                down,
            }
        }),
        Just(InputEvent::ReleaseAll),
        ".{0,40}".prop_map(|text| InputEvent::Text { text }),
    ]
}

fn clipboard_data() -> impl Strategy<Value = ClipboardData> {
    prop_oneof![
        text().prop_map(ClipboardData::Text),
        (any::<u32>(), any::<u32>(), bytes(256)).prop_map(|(width, height, rgba)| {
            ClipboardData::Image {
                width,
                height,
                rgba,
            }
        }),
        // v5: compressed image.
        (any::<u32>(), any::<u32>(), bytes(256))
            .prop_map(|(width, height, png)| { ClipboardData::Png { width, height, png } }),
    ]
}

/// Every `ClipboardData` variant is produced (the match below stops compiling when one is
/// appended without a strategy).
#[test]
fn clipboard_strategy_covers_every_variant() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::{Config, TestRng, TestRunner};
    let mut runner = TestRunner::new_with_rng(
        Config::default(),
        TestRng::deterministic_rng(proptest::test_runner::RngAlgorithm::ChaCha),
    );
    let strategy = clipboard_data();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..2_000 {
        seen.insert(match strategy.new_tree(&mut runner).unwrap().current() {
            ClipboardData::Text(_) => "Text",
            ClipboardData::Image { .. } => "Image",
            ClipboardData::Png { .. } => "Png",
        });
    }
    assert_eq!(seen.len(), 3, "only saw {seen:?}");
}

fn file_chunk() -> impl Strategy<Value = FileChunk> {
    (any::<u64>(), any::<u64>(), bytes(256), any::<bool>()).prop_map(
        |(transfer_id, offset, data, last)| FileChunk {
            transfer_id,
            offset,
            data,
            last,
        },
    )
}

fn bye() -> impl Strategy<Value = Bye> {
    prop_oneof![
        Just(ByeReason::UserClosed),
        Just(ByeReason::PermissionRevoked),
        Just(ByeReason::Timeout),
        Just(ByeReason::Error),
    ]
    .prop_map(|reason| Bye { reason })
}

fn monitor_desc() -> impl Strategy<Value = MonitorDesc> {
    (
        any::<u32>(),
        text(),
        any::<u32>(),
        any::<u32>(),
        any::<bool>(),
        any::<i32>(),
        any::<i32>(),
    )
        .prop_map(|(index, name, width, height, primary, x, y)| MonitorDesc {
            index,
            name,
            width,
            height,
            primary,
            x,
            y,
        })
}

fn fs_op() -> impl Strategy<Value = FsOp> {
    prop_oneof![
        text().prop_map(|path| FsOp::ListDir { path }),
        text().prop_map(|path| FsOp::Stat { path }),
        Just(FsOp::Roots),
        text().prop_map(|path| FsOp::MakeDir { path }),
        (text(), text()).prop_map(|(from, to)| FsOp::Rename { from, to }),
        (text(), any::<bool>()).prop_map(|(path, recursive)| FsOp::Delete { path, recursive }),
    ]
}

fn fs_entry() -> impl Strategy<Value = FsEntry> {
    (
        text(),
        any::<bool>(),
        any::<u64>(),
        any::<i64>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(name, is_dir, size, modified_unix_ms, readonly, hidden)| FsEntry {
                name,
                is_dir,
                size,
                modified_unix_ms,
                readonly,
                hidden,
            },
        )
}

fn fs_root_kind() -> impl Strategy<Value = FsRootKind> {
    prop_oneof![
        Just(FsRootKind::Drive),
        Just(FsRootKind::Home),
        Just(FsRootKind::Desktop),
        Just(FsRootKind::Documents),
        Just(FsRootKind::Downloads),
        Just(FsRootKind::Pictures),
        Just(FsRootKind::Music),
        Just(FsRootKind::Videos),
        Just(FsRootKind::Other),
    ]
}

fn fs_root() -> impl Strategy<Value = FsRoot> {
    (
        text(),
        text(),
        fs_root_kind(),
        any::<Option<u64>>(),
        any::<Option<u64>>(),
    )
        .prop_map(|(path, label, kind, total_bytes, free_bytes)| FsRoot {
            path,
            label,
            kind,
            total_bytes,
            free_bytes,
        })
}

fn fs_error() -> impl Strategy<Value = FsError> {
    prop_oneof![
        Just(FsError::NotFound),
        Just(FsError::PermissionDenied),
        Just(FsError::AlreadyExists),
        Just(FsError::NotEmpty),
        Just(FsError::InvalidPath),
        Just(FsError::NoSpace),
        Just(FsError::Busy),
        Just(FsError::Declined),
        Just(FsError::Cancelled),
        Just(FsError::HashMismatch),
        text().prop_map(FsError::Other),
    ]
}

fn fs_reply() -> impl Strategy<Value = FsReply> {
    prop_oneof![
        vec(fs_entry(), 0..4).prop_map(FsReply::Entries),
        fs_entry().prop_map(FsReply::Entry),
        vec(fs_root(), 0..4).prop_map(FsReply::Roots),
        Just(FsReply::Done),
    ]
}

fn transfer_item() -> impl Strategy<Value = TransferItem> {
    (text(), any::<u64>(), any::<i64>(), any::<bool>()).prop_map(
        |(rel_path, size, modified_unix_ms, is_dir)| TransferItem {
            rel_path,
            size,
            modified_unix_ms,
            is_dir,
        },
    )
}

fn resume() -> impl Strategy<Value = Vec<(u32, u64)>> {
    vec((any::<u32>(), any::<u64>()), 0..4)
}

fn transfer_offer() -> impl Strategy<Value = TransferOffer> {
    (
        any::<u64>(),
        prop_oneof![
            Just(TransferDirection::Upload),
            Just(TransferDirection::Download)
        ],
        text(),
        text(),
        vec(transfer_item(), 0..4),
        any::<u64>(),
        resume(),
    )
        .prop_map(
            |(id, direction, source_dir, dest_dir, items, total_bytes, resume)| TransferOffer {
                id,
                direction,
                source_dir,
                dest_dir,
                items,
                total_bytes,
                resume,
            },
        )
}

fn quality() -> impl Strategy<Value = QualitySettings> {
    (
        prop_oneof![
            Just(QualityPreset::Speed),
            Just(QualityPreset::Balanced),
            Just(QualityPreset::Quality),
            Just(QualityPreset::Custom),
        ],
        any::<u32>(),
        any::<u32>(),
        any::<u32>(),
        any::<Option<u32>>(),
    )
        .prop_map(
            |(preset, max_width, max_height, fps, bitrate_kbps)| QualitySettings {
                preset,
                max_width,
                max_height,
                fps,
                bitrate_kbps,
            },
        )
}

fn device_info() -> impl Strategy<Value = DeviceInfo> {
    let disk = (text(), text(), any::<u64>(), any::<u64>()).prop_map(
        |(mount, label, total_bytes, free_bytes)| DiskInfo {
            mount,
            label,
            total_bytes,
            free_bytes,
        },
    );
    (
        (text(), text(), text()),
        any::<u32>(),
        any::<u64>(),
        vec(disk, 0..4),
        any::<u64>(),
        text(),
    )
        .prop_map(
            |((os, version, cpu), cores, ram_bytes, disks, uptime_s, hostname)| DeviceInfo {
                os,
                version,
                cpu,
                cores,
                ram_bytes,
                disks,
                uptime_s,
                hostname,
            },
        )
}

/// Every [`Message`] variant, with arbitrary contents. [`variant_name`] (an exhaustive match)
/// fails to compile when a variant is appended, and [`strategy_covers_every_variant`] fails if
/// it is not added here too.
fn message() -> impl Strategy<Value = Message> {
    use Message as M;
    Union::new(vec![
        hello().prop_map(M::Hello).boxed(),
        auth_request().prop_map(M::AuthRequest).boxed(),
        auth_result().prop_map(M::AuthResult).boxed(),
        video_frame().prop_map(M::VideoFrame).boxed(),
        input_event().prop_map(M::InputEvent).boxed(),
        clipboard_data().prop_map(M::ClipboardData).boxed(),
        file_chunk().prop_map(M::FileChunk).boxed(),
        permissions().prop_map(M::Permissions).boxed(),
        bye().prop_map(M::Bye).boxed(),
        Just(M::RequestKeyframe).boxed(),
        any::<bool>()
            .prop_map(|local_active| M::HostInputPriority { local_active })
            .boxed(),
        (vec(monitor_desc(), 0..4), any::<u32>())
            .prop_map(|(monitors, active)| M::MonitorList { monitors, active })
            .boxed(),
        any::<u32>()
            .prop_map(|index| M::SelectMonitor { index })
            .boxed(),
        (any::<u64>(), fs_op())
            .prop_map(|(req_id, op)| M::FsRequest { req_id, op })
            .boxed(),
        (
            any::<u64>(),
            prop_oneof![fs_reply().prop_map(Ok), fs_error().prop_map(Err)],
        )
            .prop_map(|(req_id, result)| M::FsResponse { req_id, result })
            .boxed(),
        transfer_offer().prop_map(M::TransferOffer).boxed(),
        (any::<u64>(), resume())
            .prop_map(|(id, resume)| M::TransferAccept { id, resume })
            .boxed(),
        (any::<u64>(), fs_error())
            .prop_map(|(id, error)| M::TransferReject { id, error })
            .boxed(),
        (any::<u64>(), any::<u64>())
            .prop_map(|(id, bytes_done)| M::TransferProgress { id, bytes_done })
            .boxed(),
        any::<u64>().prop_map(|id| M::TransferCancel { id }).boxed(),
        (any::<u64>(), any::<u32>(), any::<[u8; 32]>())
            .prop_map(|(id, item, blake3)| M::TransferDone { id, item, blake3 })
            .boxed(),
        (any::<u64>(), any::<Option<u32>>(), fs_error())
            .prop_map(|(id, item, error)| M::TransferError { id, item, error })
            .boxed(),
        (any::<u64>(), vec(transfer_item(), 0..4))
            .prop_map(|(transfer_id, items)| M::ClipboardFiles { transfer_id, items })
            .boxed(),
        (text(), text())
            .prop_map(|(html, alt_text)| M::ClipboardHtml { html, alt_text })
            .boxed(),
        (any::<u64>(), text(), any::<u64>())
            .prop_map(|(id, text, sent_unix_ms)| M::ChatMessage {
                id,
                text,
                sent_unix_ms,
            })
            .boxed(),
        (any::<bool>(), any::<u32>(), any::<u8>())
            .prop_map(|(enabled, sample_rate, channels)| M::AudioControl {
                enabled,
                sample_rate,
                channels,
            })
            .boxed(),
        Just(M::SendCtrlAltDel).boxed(),
        any::<bool>()
            .prop_map(|reconnect| M::RestartRemote { reconnect })
            .boxed(),
        (bytes(64), any::<u64>())
            .prop_map(|(token, expires_unix_ms)| M::ReconnectTicket {
                token,
                expires_unix_ms,
            })
            .boxed(),
        any::<bool>()
            .prop_map(|enabled| M::PrivacyMode { enabled })
            .boxed(),
        (any::<bool>(), any::<bool>())
            .prop_map(|(secure_desktop, locked)| M::DesktopState {
                secure_desktop,
                locked,
            })
            .boxed(),
        Just(M::LockWorkstation).boxed(),
        quality().prop_map(M::SetQuality).boxed(),
        (any::<u32>(), any::<u32>(), any::<u32>())
            .prop_map(|(decode_us, frames_dropped, rtt_us)| M::ViewerFeedback {
                decode_us,
                frames_dropped,
                rtt_us,
            })
            .boxed(),
        any::<u64>()
            .prop_map(|t_viewer_us| M::LatencyProbe { t_viewer_us })
            .boxed(),
        (any::<u64>(), any::<u64>())
            .prop_map(|(t_viewer_us, t_host_capture_us)| M::LatencyEcho {
                t_viewer_us,
                t_host_capture_us,
            })
            .boxed(),
        Just(M::SelectAllMonitors).boxed(),
        (any::<u64>(), any::<u16>(), proptest::option::of(text()))
            .prop_map(|(id, target_port, target_host)| M::TunnelOpen {
                id,
                target_port,
                target_host,
            })
            .boxed(),
        any::<u64>().prop_map(|id| M::TunnelOpened { id }).boxed(),
        (any::<u64>(), proptest::option::of(text()))
            .prop_map(|(id, error)| M::TunnelClose { id, error })
            .boxed(),
        (
            any::<u64>(),
            vec((any::<u16>(), any::<u16>()), 0..16),
            any::<u32>(),
            any::<u16>(),
        )
            .prop_map(|(id, points, color_rgba, width)| M::WhiteboardStroke {
                id,
                points,
                color_rgba,
                width,
            })
            .boxed(),
        Just(M::WhiteboardClear).boxed(),
        Just(M::DeviceInfoRequest).boxed(),
        device_info().prop_map(M::DeviceInfo).boxed(),
    ])
}

/// Name of the variant. Exhaustive on purpose: a new variant breaks the build here until the
/// strategy above learns it.
fn variant_name(m: &Message) -> &'static str {
    use Message as M;
    match m {
        M::Hello(_) => "Hello",
        M::AuthRequest(_) => "AuthRequest",
        M::AuthResult(_) => "AuthResult",
        M::VideoFrame(_) => "VideoFrame",
        M::InputEvent(_) => "InputEvent",
        M::ClipboardData(_) => "ClipboardData",
        M::FileChunk(_) => "FileChunk",
        M::Permissions(_) => "Permissions",
        M::Bye(_) => "Bye",
        M::RequestKeyframe => "RequestKeyframe",
        M::HostInputPriority { .. } => "HostInputPriority",
        M::MonitorList { .. } => "MonitorList",
        M::SelectMonitor { .. } => "SelectMonitor",
        M::FsRequest { .. } => "FsRequest",
        M::FsResponse { .. } => "FsResponse",
        M::TransferOffer(_) => "TransferOffer",
        M::TransferAccept { .. } => "TransferAccept",
        M::TransferReject { .. } => "TransferReject",
        M::TransferProgress { .. } => "TransferProgress",
        M::TransferCancel { .. } => "TransferCancel",
        M::TransferDone { .. } => "TransferDone",
        M::TransferError { .. } => "TransferError",
        M::ClipboardFiles { .. } => "ClipboardFiles",
        M::ClipboardHtml { .. } => "ClipboardHtml",
        M::ChatMessage { .. } => "ChatMessage",
        M::AudioControl { .. } => "AudioControl",
        M::SendCtrlAltDel => "SendCtrlAltDel",
        M::RestartRemote { .. } => "RestartRemote",
        M::ReconnectTicket { .. } => "ReconnectTicket",
        M::PrivacyMode { .. } => "PrivacyMode",
        M::DesktopState { .. } => "DesktopState",
        M::LockWorkstation => "LockWorkstation",
        M::SetQuality(_) => "SetQuality",
        M::ViewerFeedback { .. } => "ViewerFeedback",
        M::LatencyProbe { .. } => "LatencyProbe",
        M::LatencyEcho { .. } => "LatencyEcho",
        M::SelectAllMonitors => "SelectAllMonitors",
        M::TunnelOpen { .. } => "TunnelOpen",
        M::TunnelOpened { .. } => "TunnelOpened",
        M::TunnelClose { .. } => "TunnelClose",
        M::WhiteboardStroke { .. } => "WhiteboardStroke",
        M::WhiteboardClear => "WhiteboardClear",
        M::DeviceInfoRequest => "DeviceInfoRequest",
        M::DeviceInfo(_) => "DeviceInfo",
    }
}

/// Number of [`Message`] variants (= arms of [`variant_name`]).
const VARIANTS: usize = 44;

// ------------------------------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------------------------------

/// Feeds `stream` to a fresh decoder in chunks of the given sizes (cycled), draining after
/// every push like `tlp-desk-net` does. Returns the messages before the first error and
/// whether an error ended decoding. Asserts the buffering bound after every step.
fn feed(stream: &[u8], chunks: &[usize]) -> (Vec<Message>, bool) {
    let mut dec = FrameDecoder::new();
    let mut out = Vec::new();
    let mut rest = stream;
    let mut sizes = chunks.iter().copied().cycle();
    while !rest.is_empty() {
        let n = sizes.next().unwrap_or(1).clamp(1, rest.len());
        let (chunk, tail) = rest.split_at(n);
        rest = tail;
        dec.push(chunk);
        loop {
            match dec.next_message() {
                Ok(Some(m)) => out.push(m),
                Ok(None) => break,
                Err(_) => return (out, true),
            }
        }
        assert!(
            dec.buffered_len() <= MAX_FRAME_LEN + 4,
            "decoder holds {} bytes after draining",
            dec.buffered_len()
        );
    }
    (out, false)
}

fn frames(msgs: &[Message]) -> Vec<u8> {
    msgs.iter().flat_map(|m| encode_frame(m).unwrap()).collect()
}

fn config(cases: u32) -> ProptestConfig {
    ProptestConfig {
        cases,
        // Each case is cheap; shrinking a failure must not take forever either.
        max_shrink_iters: 4096,
        ..ProptestConfig::default()
    }
}

/// Default case count; `PROPTEST_CASES` overrides it.
fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(512)
}

/// Case count of the ignored long runs.
const LONG: u32 = 200_000;

// ------------------------------------------------------------------------------------------
// Properties
// ------------------------------------------------------------------------------------------

fn check_roundtrip(msg: &Message) -> Result<(), TestCaseError> {
    let body = encode_message(msg).unwrap();
    prop_assert_eq!(&decode_message(&body).unwrap(), msg);
    let frame = encode_frame(msg).unwrap();
    prop_assert_eq!(frame.len(), body.len() + 4);
    prop_assert_eq!(&frame[..4], &(body.len() as u32).to_le_bytes()[..]);
    // The variant index is the first body byte (all indices are < 128, one varint byte).
    prop_assert!(usize::from(body[0]) < VARIANTS, "{}", variant_name(msg));
    Ok(())
}

fn check_stream(msgs: &[Message], chunks: &[usize]) -> Result<(), TestCaseError> {
    let (out, failed) = feed(&frames(msgs), chunks);
    prop_assert!(!failed);
    prop_assert_eq!(out.as_slice(), msgs);
    Ok(())
}

/// Random bytes: decoding returns `Ok` or `Err`, never panics; anything accepted re-encodes to
/// a message that decodes to itself.
fn check_random_body(body: &[u8]) -> Result<(), TestCaseError> {
    if let Ok(m) = decode_message(body) {
        let again = encode_message(&m).unwrap();
        prop_assert_eq!(decode_message(&again).unwrap(), m);
    }
    Ok(())
}

fn check_random_stream(stream: &[u8], chunks: &[usize]) -> Result<(), TestCaseError> {
    let _ = feed(stream, chunks);
    Ok(())
}

/// A valid frame with some bytes overwritten (header and body): no panic, bounded buffer.
fn check_mutated(
    msgs: &[Message],
    edits: &[(usize, u8)],
    chunks: &[usize],
) -> Result<(), TestCaseError> {
    let mut stream = frames(msgs);
    if stream.is_empty() {
        return Ok(());
    }
    let len = stream.len();
    for (at, b) in edits {
        stream[at % len] = *b;
    }
    let _ = feed(&stream, chunks);
    Ok(())
}

/// Every strict prefix of a frame is "need more bytes", never a message or an error.
fn check_truncated(msg: &Message, cut: usize) -> Result<(), TestCaseError> {
    let frame = encode_frame(msg).unwrap();
    let cut = cut % frame.len();
    let mut dec = FrameDecoder::new();
    dec.push(&frame[..cut]);
    prop_assert!(matches!(dec.next_message(), Ok(None)));
    prop_assert_eq!(dec.buffered_len(), cut);
    dec.push(&frame[cut..]);
    prop_assert_eq!(&dec.next_message().unwrap().unwrap(), msg);
    prop_assert_eq!(dec.buffered_len(), 0);
    Ok(())
}

/// A length prefix above the limit is refused at once, before the body arrives, whatever
/// follows it.
fn check_oversized(len: u32, tail: &[u8]) -> Result<(), TestCaseError> {
    let mut dec = FrameDecoder::new();
    dec.push(&len.to_le_bytes());
    let r = dec.next_message();
    prop_assert!(
        matches!(r, Err(ProtoError::FrameTooLarge(n)) if n == len as usize),
        "{:?}",
        r
    );
    dec.push(tail);
    prop_assert!(dec.next_message().is_err());
    Ok(())
}

fn check_stream_header(b: [u8; STREAM_HEADER_LEN]) -> Result<(), TestCaseError> {
    match StreamHeader::decode(&b) {
        Ok(h) => {
            prop_assert!(b[0] < 5);
            prop_assert_eq!(h.encode(), b);
        }
        Err(ProtoError::UnknownStreamKind(k)) => {
            prop_assert_eq!(k, b[0]);
            prop_assert!(StreamKind::from_u8(k).is_none());
        }
        Err(e) => prop_assert!(false, "unexpected error {e}"),
    }
    Ok(())
}

fn check_file_data_header(b: [u8; FILE_DATA_HEADER_LEN]) -> Result<(), TestCaseError> {
    let h = FileDataHeader::decode(&b);
    prop_assert_eq!(h.encode(), b);
    Ok(())
}

fn chunk_sizes() -> impl Strategy<Value = Vec<usize>> {
    vec(1usize..64, 1..8)
}

proptest! {
    #![proptest_config(config(cases()))]

    #[test]
    fn every_message_roundtrips(msg in message()) {
        check_roundtrip(&msg)?;
    }

    #[test]
    fn frame_streams_decode_in_any_chunking(msgs in vec(message(), 0..8), chunks in chunk_sizes()) {
        check_stream(&msgs, &chunks)?;
    }

    #[test]
    fn random_bodies_never_panic(body in bytes(256)) {
        check_random_body(&body)?;
    }

    #[test]
    fn random_streams_never_panic(stream in bytes(1024), chunks in chunk_sizes()) {
        check_random_stream(&stream, &chunks)?;
    }

    #[test]
    fn mutated_frames_never_panic(
        msgs in vec(message(), 1..4),
        edits in vec((any::<usize>(), any::<u8>()), 1..8),
        chunks in chunk_sizes(),
    ) {
        check_mutated(&msgs, &edits, &chunks)?;
    }

    #[test]
    fn truncated_frames_wait_for_more(msg in message(), cut in any::<usize>()) {
        check_truncated(&msg, cut)?;
    }

    #[test]
    fn oversized_prefix_is_refused_immediately(
        len in (MAX_FRAME_LEN as u32 + 1)..=u32::MAX,
        tail in bytes(64),
    ) {
        check_oversized(len, &tail)?;
    }

    #[test]
    fn stream_headers_decode_or_refuse(b in any::<[u8; STREAM_HEADER_LEN]>()) {
        check_stream_header(b)?;
    }

    #[test]
    fn file_data_headers_roundtrip(b in any::<[u8; FILE_DATA_HEADER_LEN]>()) {
        check_file_data_header(b)?;
    }

    /// A decoder with a smaller limit accepts frames up to it and refuses longer prefixes
    /// before buffering their body.
    #[test]
    fn custom_limit_is_enforced(msg in message(), slack in 0usize..64) {
        let frame = encode_frame(&msg).unwrap();
        let body = frame.len() - 4;
        let mut dec = FrameDecoder::with_max_len(body + slack);
        dec.push(&frame);
        prop_assert_eq!(&dec.next_message().unwrap().unwrap(), &msg);
        if body > 0 {
            let mut dec = FrameDecoder::with_max_len(body - 1);
            dec.push(&frame[..4]);
            prop_assert!(matches!(dec.next_message(), Err(ProtoError::FrameTooLarge(n)) if n == body));
        }
    }
}

/// Limits above `MAX_FRAME_LEN` are clamped; a limit can be raised after the handshake.
#[test]
fn decoder_limit_is_clamped_and_adjustable() {
    assert_eq!(FrameDecoder::new().max_len(), MAX_FRAME_LEN);
    assert_eq!(
        FrameDecoder::with_max_len(usize::MAX).max_len(),
        MAX_FRAME_LEN
    );
    let mut dec = FrameDecoder::with_max_len(8);
    let big = encode_frame(&Message::ChatMessage {
        id: 1,
        text: "رسالة طويلة نوعا ما".into(),
        sent_unix_ms: 0,
    })
    .unwrap();
    dec.push(&big[..4]);
    assert!(dec.next_message().is_err());
    let mut dec = FrameDecoder::with_max_len(8);
    dec.set_max_len(MAX_FRAME_LEN);
    dec.push(&big);
    assert!(matches!(
        dec.next_message(),
        Ok(Some(Message::ChatMessage { .. }))
    ));
}

proptest! {
    #![proptest_config(config(LONG))]

    #[test]
    #[ignore = "long fuzz-style run; see the file header"]
    fn every_message_roundtrips_long(msg in message()) {
        check_roundtrip(&msg)?;
    }

    #[test]
    #[ignore = "long fuzz-style run; see the file header"]
    fn random_bodies_never_panic_long(body in bytes(4096)) {
        check_random_body(&body)?;
    }

    #[test]
    #[ignore = "long fuzz-style run; see the file header"]
    fn random_streams_never_panic_long(stream in bytes(8192), chunks in chunk_sizes()) {
        check_random_stream(&stream, &chunks)?;
    }

    #[test]
    #[ignore = "long fuzz-style run; see the file header"]
    fn mutated_frames_never_panic_long(
        msgs in vec(message(), 1..8),
        edits in vec((any::<usize>(), any::<u8>()), 1..16),
        chunks in chunk_sizes(),
    ) {
        check_mutated(&msgs, &edits, &chunks)?;
    }
}

// ------------------------------------------------------------------------------------------
// Targeted hostile inputs
// ------------------------------------------------------------------------------------------

/// The strategy produces every variant (guards against forgetting a newly appended one).
#[test]
fn strategy_covers_every_variant() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::{Config, TestRng, TestRunner};
    let mut runner = TestRunner::new_with_rng(
        Config::default(),
        TestRng::deterministic_rng(proptest::test_runner::RngAlgorithm::ChaCha),
    );
    let strategy = message();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..20_000 {
        let m = strategy.new_tree(&mut runner).unwrap().current();
        seen.insert(variant_name(&m));
        if seen.len() == VARIANTS {
            break;
        }
    }
    assert_eq!(seen.len(), VARIANTS, "only saw {seen:?}");
}

/// Collections that claim billions of elements in a tiny body are rejected without allocating
/// for the claimed size (postcard checks the remaining input; serde caps pre-allocation).
#[test]
fn huge_declared_lengths_fail_fast() {
    // varint(u32::MAX) = ff ff ff ff 0f; varint(u64::MAX) = 9 x ff + 01.
    let huge32 = [0xFF, 0xFF, 0xFF, 0xFF, 0x0F];
    let huge64 = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01];
    let video = encode_message(&Message::VideoFrame(VideoFrame {
        seq: 1,
        timestamp_us: 2,
        width: 3,
        height: 4,
        codec: Codec::H264,
        keyframe: true,
        data: Vec::new(),
    }))
    .unwrap();
    for huge in [&huge32[..], &huge64[..]] {
        // VideoFrame with a huge `data` length and 3 bytes of data.
        let mut body = video[..video.len() - 1].to_vec();
        body.extend_from_slice(huge);
        body.extend_from_slice(&[1, 2, 3]);
        assert!(decode_message(&body).is_err());
        // MonitorList with a huge element count.
        let mut body = vec![11];
        body.extend_from_slice(huge);
        body.extend_from_slice(&[0; 8]);
        assert!(decode_message(&body).is_err());
        // Chat text with a huge byte length.
        let mut body = encode_message(&Message::ChatMessage {
            id: 1,
            text: String::new(),
            sent_unix_ms: 0,
        })
        .unwrap();
        body.truncate(2);
        body.extend_from_slice(huge);
        body.extend_from_slice("مرحبا".as_bytes());
        assert!(decode_message(&body).is_err());
        // Whiteboard stroke with a huge point count.
        let mut body = encode_message(&Message::WhiteboardStroke {
            id: 1,
            points: Vec::new(),
            color_rgba: 0,
            width: 0,
        })
        .unwrap();
        body.truncate(2);
        body.extend_from_slice(huge);
        assert!(decode_message(&body).is_err());
    }
}

/// Unknown variant indices (a newer peer, or garbage) are errors.
#[test]
fn unknown_variant_index_is_an_error() {
    for idx in [VARIANTS as u8, 100, 127] {
        assert!(decode_message(&[idx]).is_err(), "index {idx}");
        assert!(decode_message(&[idx, 0, 0, 0]).is_err(), "index {idx}");
    }
    assert!(decode_message(&[]).is_err());
}

/// Invalid UTF-8 in a string field is an error, not a lossy decode.
#[test]
fn invalid_utf8_text_is_an_error() {
    let mut body = encode_message(&Message::ChatMessage {
        id: 7,
        text: "ab".into(),
        sent_unix_ms: 0,
    })
    .unwrap();
    // [variant, id, len=2, 'a', 'b', ts]
    let a = body.iter().position(|b| *b == b'a').unwrap();
    body[a] = 0xFF;
    assert!(decode_message(&body).is_err());
}

/// A body with trailing bytes after a complete message is accepted (postcard ignores the
/// rest) but the next frame still starts at the declared length: framing never desyncs.
#[test]
fn trailing_bytes_inside_a_frame_do_not_desync() {
    let mut body = encode_message(&Message::RequestKeyframe).unwrap();
    body.extend_from_slice(&[0xAA; 7]);
    let mut stream = (body.len() as u32).to_le_bytes().to_vec();
    stream.extend_from_slice(&body);
    stream.extend_from_slice(&encode_frame(&Message::SelectAllMonitors).unwrap());
    let (out, failed) = feed(&stream, &[3]);
    assert!(!failed);
    assert_eq!(out.last(), Some(&Message::SelectAllMonitors));
}

/// A frame at exactly the limit is accepted; one byte more is refused.
#[test]
fn frame_limit_is_inclusive() {
    let mut dec = FrameDecoder::new();
    dec.push(&(MAX_FRAME_LEN as u32).to_le_bytes());
    assert!(matches!(dec.next_message(), Ok(None)));
    let mut dec = FrameDecoder::new();
    dec.push(&(MAX_FRAME_LEN as u32 + 1).to_le_bytes());
    assert!(matches!(
        dec.next_message(),
        Err(ProtoError::FrameTooLarge(_))
    ));
    // Encoding refuses bodies over the limit too.
    let big = Message::VideoFrame(VideoFrame {
        seq: 0,
        timestamp_us: 0,
        width: 0,
        height: 0,
        codec: Codec::H264,
        keyframe: false,
        data: vec![0; MAX_FRAME_LEN + 1],
    });
    assert!(matches!(
        encode_frame(&big),
        Err(ProtoError::FrameTooLarge(_))
    ));
}
