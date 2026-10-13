use tlp_desk_proto::*;

fn samples() -> Vec<Message> {
    let mut v = vec![
        Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: 123_456_789,
            device_name: "مكتب-1 office".into(),
            role: Role::Viewer,
        }),
        Message::AuthRequest(AuthRequest::AcceptPrompt),
        Message::AuthRequest(AuthRequest::OneTimeCode {
            spake2_msg: vec![1, 2, 3],
        }),
        Message::AuthRequest(AuthRequest::Password {
            spake2_msg: vec![9; 33],
        }),
        Message::AuthRequest(AuthRequest::KeyConfirm { mac: vec![7; 32] }),
        Message::AuthResult(AuthResult::KeyConfirm { mac: vec![8; 32] }),
        Message::AuthResult(AuthResult::Spake2Reply {
            spake2_msg: vec![4, 5],
        }),
        Message::AuthResult(AuthResult::Accepted {
            permissions: Permissions::VIEW | Permissions::CONTROL,
        }),
        Message::AuthResult(AuthResult::Rejected {
            reason: RejectReason::RateLimited,
        }),
        Message::VideoFrame(VideoFrame {
            seq: 42,
            timestamp_us: 1_000_000,
            width: 1920,
            height: 1080,
            codec: Codec::H264,
            keyframe: true,
            data: (0..=255).collect(),
        }),
        Message::InputEvent(InputEvent::MouseMove { x: 0, y: u16::MAX }),
        Message::InputEvent(InputEvent::MouseButton {
            button: MouseButton::Right,
            down: true,
        }),
        Message::InputEvent(InputEvent::MouseWheel { dx: -120, dy: 240 }),
        Message::InputEvent(InputEvent::Key {
            scancode: 0x5B,
            extended: true,
            down: false,
        }),
        Message::InputEvent(InputEvent::ReleaseAll),
        Message::ClipboardData(ClipboardData::Text("مرحبا hello".into())),
        Message::ClipboardData(ClipboardData::Image {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 4],
        }),
        Message::FileChunk(FileChunk {
            transfer_id: 7,
            offset: 4096,
            data: vec![0; 10],
            last: true,
        }),
        Message::Permissions(Permissions::ALL),
        Message::Bye(Bye {
            reason: ByeReason::UserClosed,
        }),
        Message::RequestKeyframe,
        Message::HostInputPriority { local_active: true },
        Message::HostInputPriority {
            local_active: false,
        },
        Message::MonitorList {
            monitors: vec![
                MonitorDesc {
                    index: 0,
                    name: "شاشة 1 DISPLAY1".into(),
                    width: 3840,
                    height: 2400,
                    primary: true,
                    x: 0,
                    y: 0,
                },
                MonitorDesc {
                    index: 1,
                    name: String::new(),
                    width: 1920,
                    height: 1080,
                    primary: false,
                    x: -1920,
                    y: 200,
                },
            ],
            active: 1,
        },
        Message::MonitorList {
            monitors: vec![],
            active: 0,
        },
        Message::SelectMonitor { index: 1 },
    ];
    v.extend(v4_samples());
    v
}

#[test]
fn every_message_roundtrips() {
    for msg in samples() {
        let body = encode_message(&msg).unwrap();
        assert_eq!(decode_message(&body).unwrap(), msg);
    }
}

#[test]
fn frames_decode_from_byte_by_byte_stream() {
    let msgs = samples();
    let stream: Vec<u8> = msgs.iter().flat_map(|m| encode_frame(m).unwrap()).collect();
    let mut dec = FrameDecoder::new();
    let mut out = Vec::new();
    for b in stream {
        dec.push(&[b]);
        while let Some(m) = dec.next_message().unwrap() {
            out.push(m);
        }
    }
    assert_eq!(out, msgs);
}

#[test]
fn oversized_length_prefix_is_rejected() {
    let mut dec = FrameDecoder::new();
    dec.push(&((MAX_FRAME_LEN as u32) + 1).to_le_bytes());
    assert!(matches!(
        dec.next_message(),
        Err(ProtoError::FrameTooLarge(_))
    ));
}

#[test]
fn garbage_body_is_an_error_not_a_panic() {
    let mut dec = FrameDecoder::new();
    dec.push(&3u32.to_le_bytes());
    dec.push(&[0xFF, 0xFF, 0xFF]);
    assert!(dec.next_message().is_err());
}

#[test]
fn permissions_bit_ops() {
    let p = Permissions::VIEW | Permissions::CLIPBOARD;
    assert!(p.contains(Permissions::VIEW));
    assert!(!p.contains(Permissions::CONTROL));
    assert_eq!(p.difference(Permissions::VIEW), Permissions::CLIPBOARD);
    assert!(Permissions::ALL.contains(p | Permissions::PRIVACY_MODE));
}

#[test]
fn permission_bits_are_stable_and_distinct() {
    let bits = [
        Permissions::VIEW,
        Permissions::CONTROL,
        Permissions::KEYBOARD,
        Permissions::CLIPBOARD,
        Permissions::FILES,
        Permissions::AUDIO,
        Permissions::RESTART,
        Permissions::PRIVACY_MODE,
        Permissions::CHAT,
        Permissions::TUNNEL,
        Permissions::WHITEBOARD,
        Permissions::SYSTEM,
    ];
    for (i, b) in bits.iter().enumerate() {
        assert_eq!(b.0, 1 << i, "bit {i} moved");
        assert!(Permissions::ALL.contains(*b));
    }
    assert_eq!(Permissions::ALL.0, 0x0FFF);
}

#[test]
fn v1_variant_indices_are_stable() {
    // v2 only appended variants; existing encodings must not change so an older peer can still
    // decode `Rejected { VersionMismatch }`.
    let rej = encode_message(&Message::AuthResult(AuthResult::Rejected {
        reason: RejectReason::VersionMismatch,
    }))
    .unwrap();
    assert_eq!(rej, vec![2, 2, 4]);
    let accept = encode_message(&Message::AuthRequest(AuthRequest::AcceptPrompt)).unwrap();
    assert_eq!(accept, vec![1, 0]);
}

#[test]
fn v3_variants_are_appended() {
    // v3 appended three variants after `RequestKeyframe` (index 9).
    assert_eq!(encode_message(&Message::RequestKeyframe).unwrap(), vec![9]);
    assert_eq!(
        encode_message(&Message::HostInputPriority { local_active: true }).unwrap(),
        vec![10, 1]
    );
    assert_eq!(
        encode_message(&Message::SelectMonitor { index: 2 }).unwrap(),
        vec![12, 2]
    );
}

/// Samples of every protocol-v4 message (Arabic text and paths on purpose).
fn v4_samples() -> Vec<Message> {
    let entry = FsEntry {
        name: "تقرير الربع الأول.docx".into(),
        is_dir: false,
        size: 123_456,
        modified_unix_ms: 1_760_000_000_000,
        readonly: false,
        hidden: true,
    };
    vec![
        // --- remote file system ---
        Message::FsRequest {
            req_id: 1,
            op: FsOp::ListDir {
                path: r"C:\Users\مستخدم\سطح المكتب".into(),
            },
        },
        Message::FsRequest {
            req_id: 2,
            op: FsOp::Stat {
                path: r"D:\a.txt".into(),
            },
        },
        Message::FsRequest {
            req_id: 3,
            op: FsOp::Roots,
        },
        Message::FsRequest {
            req_id: 4,
            op: FsOp::MakeDir {
                path: r"C:\جديد".into(),
            },
        },
        Message::FsRequest {
            req_id: 5,
            op: FsOp::Rename {
                from: r"C:\a".into(),
                to: r"C:\ب".into(),
            },
        },
        Message::FsRequest {
            req_id: u64::MAX,
            op: FsOp::Delete {
                path: r"C:\tmp".into(),
                recursive: true,
            },
        },
        Message::FsResponse {
            req_id: 1,
            result: Ok(FsReply::Entries(vec![entry.clone()])),
        },
        Message::FsResponse {
            req_id: 2,
            result: Ok(FsReply::Entry(FsEntry {
                modified_unix_ms: -5,
                ..entry
            })),
        },
        Message::FsResponse {
            req_id: 3,
            result: Ok(FsReply::Roots(vec![
                FsRoot {
                    path: r"C:\".into(),
                    label: "Windows".into(),
                    kind: FsRootKind::Drive,
                    total_bytes: Some(1 << 40),
                    free_bytes: Some(1 << 30),
                },
                FsRoot {
                    path: r"C:\Users\u\Desktop".into(),
                    label: "سطح المكتب".into(),
                    kind: FsRootKind::Desktop,
                    total_bytes: None,
                    free_bytes: None,
                },
            ])),
        },
        Message::FsResponse {
            req_id: 4,
            result: Ok(FsReply::Done),
        },
        Message::FsResponse {
            req_id: 5,
            result: Err(FsError::Other("الوصول مرفوض".into())),
        },
        Message::FsResponse {
            req_id: 6,
            result: Err(FsError::NotEmpty),
        },
    ]
}

/// Postcard variant index of a message (its first byte; every index is < 128).
fn index(m: &Message) -> u8 {
    encode_message(m).unwrap()[0]
}

#[test]
fn v4_variants_are_appended_in_order() {
    // v4 appends after `SelectMonitor` (index 12). These indices are wire format: never change.
    let expected: &[(Message, u8)] = &[
        (
            Message::FsRequest {
                req_id: 0,
                op: FsOp::Roots,
            },
            13,
        ),
        (
            Message::FsResponse {
                req_id: 0,
                result: Ok(FsReply::Done),
            },
            14,
        ),
    ];
    for (m, i) in expected {
        assert_eq!(index(m), *i, "{m:?}");
    }
}

#[test]
fn v4_permissions_gate_feature_messages() {
    let fs = Message::FsRequest {
        req_id: 1,
        op: FsOp::Roots,
    };
    assert_eq!(fs.required_permissions(), Permissions::FILES);
    assert_eq!(
        Message::RequestKeyframe.required_permissions(),
        Permissions::VIEW
    );
    assert_eq!(
        Message::Bye(Bye {
            reason: ByeReason::Error
        })
        .required_permissions(),
        Permissions::NONE
    );
}

#[test]
fn protocol_version_is_4() {
    assert_eq!(PROTOCOL_VERSION, 4);
}
