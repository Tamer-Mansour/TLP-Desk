//! Platform-independent injector core: maps events, tracks what is held down, and hands
//! low-level actions to a sink (real `SendInput` on Windows, a recorder in tests).

// Only the Windows sink and the tests drive this today.
#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use std::collections::BTreeSet;

use crate::keys::{normalize_scancode, ScanKey};
use crate::{InputError, InputEvent, InputInjector, MonitorRect, MouseButton};
use tlp_desk_proto::MAX_INPUT_TEXT_CHARS;

/// One low-level action for the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawInput {
    /// Move to this physical virtual-desktop pixel.
    MoveTo {
        x: i32,
        y: i32,
    },
    Button {
        button: MouseButton,
        down: bool,
    },
    /// Wheel delta in 1/120 notch units; `horizontal` selects the tilt wheel.
    Wheel {
        horizontal: bool,
        delta: i32,
    },
    Key {
        key: ScanKey,
        down: bool,
    },
    /// One UTF-16 code unit typed independent of the keyboard layout (`KEYEVENTF_UNICODE`).
    /// Characters outside the BMP are two units (a surrogate pair) sent back to back.
    Unicode {
        unit: u16,
        down: bool,
    },
}

/// Turns `text` into the key-down/key-up pairs that type it, or `None` if the text must be
/// dropped (empty, longer than [`MAX_INPUT_TEXT_CHARS`], or containing control characters other
/// than tab and newline). A newline becomes carriage return, which is what Enter produces.
pub(crate) fn text_to_raw(text: &str) -> Option<Vec<RawInput>> {
    if text.is_empty() || text.chars().count() > MAX_INPUT_TEXT_CHARS {
        return None;
    }
    let mut batch = Vec::with_capacity(text.len().saturating_mul(2));
    let mut units = [0u16; 2];
    for c in text.chars() {
        if c.is_control() && !matches!(c, '\t' | '\n') {
            return None;
        }
        let c = if c == '\n' { '\r' } else { c };
        for &unit in c.encode_utf16(&mut units).iter() {
            batch.push(RawInput::Unicode { unit, down: true });
            batch.push(RawInput::Unicode { unit, down: false });
        }
    }
    Some(batch)
}

/// Delivers a batch of actions to the OS atomically (one `SendInput` call on Windows).
pub(crate) trait InputSink: Send {
    fn send(&mut self, batch: &[RawInput]) -> Result<(), InputError>;
}

const ALL_BUTTONS: [MouseButton; 5] = [
    MouseButton::Left,
    MouseButton::Right,
    MouseButton::Middle,
    MouseButton::Back,
    MouseButton::Forward,
];

fn button_index(b: MouseButton) -> usize {
    match b {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::Back => 3,
        MouseButton::Forward => 4,
    }
}

/// Injector core over any sink. Releases everything it pressed on [`InputEvent::ReleaseAll`]
/// and on drop.
pub(crate) struct TrackingInjector<S: InputSink> {
    sink: S,
    monitor: MonitorRect,
    keys: BTreeSet<ScanKey>,
    buttons: [bool; 5],
}

impl<S: InputSink> TrackingInjector<S> {
    pub(crate) fn new(sink: S, monitor: MonitorRect) -> Self {
        Self {
            sink,
            monitor,
            keys: BTreeSet::new(),
            buttons: [false; 5],
        }
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn monitor(&self) -> MonitorRect {
        self.monitor
    }

    pub(crate) fn set_monitor(&mut self, monitor: MonitorRect) {
        self.monitor = monitor;
    }

    #[cfg(test)]
    pub(crate) fn sink(&self) -> &S {
        &self.sink
    }

    #[cfg(test)]
    pub(crate) fn pressed_keys(&self) -> impl Iterator<Item = ScanKey> + '_ {
        self.keys.iter().copied()
    }

    pub(crate) fn pressed_buttons(&self) -> impl Iterator<Item = MouseButton> + '_ {
        ALL_BUTTONS
            .into_iter()
            .filter(|b| self.buttons[button_index(*b)])
    }

    fn release_everything(&mut self) -> Result<(), InputError> {
        let keys = self
            .keys
            .iter()
            .rev()
            .map(|&key| RawInput::Key { key, down: false });
        let buttons = self.pressed_buttons().map(|button| RawInput::Button {
            button,
            down: false,
        });
        let batch: Vec<RawInput> = keys.chain(buttons).collect();
        if batch.is_empty() {
            return Ok(());
        }
        // Keep the state on failure so a later ReleaseAll (or drop) retries.
        self.sink.send(&batch)?;
        self.keys.clear();
        self.buttons = [false; 5];
        Ok(())
    }
}

impl<S: InputSink> InputInjector for TrackingInjector<S> {
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        match *event {
            InputEvent::MouseMove { x, y } => {
                let (x, y) = self.monitor.normalized_to_pixel(x, y);
                self.sink.send(&[RawInput::MoveTo { x, y }])
            }
            InputEvent::MouseButton { button, down } => {
                let idx = button_index(button);
                if !down {
                    self.buttons[idx] = false;
                }
                self.sink.send(&[RawInput::Button { button, down }])?;
                if down {
                    self.buttons[idx] = true;
                }
                Ok(())
            }
            InputEvent::MouseWheel { dx, dy } => {
                let mut batch = Vec::with_capacity(2);
                if dy != 0 {
                    batch.push(RawInput::Wheel {
                        horizontal: false,
                        delta: i32::from(dy),
                    });
                }
                if dx != 0 {
                    batch.push(RawInput::Wheel {
                        horizontal: true,
                        delta: i32::from(dx),
                    });
                }
                if batch.is_empty() {
                    return Ok(());
                }
                self.sink.send(&batch)
            }
            InputEvent::Key {
                scancode,
                extended,
                down,
            } => {
                // Unrepresentable keys (Pause, scancode 0) are ignored rather than failing
                // the whole session.
                let Some(key) = normalize_scancode(scancode, extended) else {
                    return Ok(());
                };
                if !down {
                    self.keys.remove(&key);
                }
                self.sink.send(&[RawInput::Key { key, down }])?;
                if down {
                    self.keys.insert(key);
                }
                Ok(())
            }
            InputEvent::ReleaseAll => self.release_everything(),
            InputEvent::Text { ref text } => match text_to_raw(text) {
                // One atomic batch so a surrogate pair is never split by other input.
                Some(batch) => self.sink.send(&batch),
                None => Ok(()),
            },
        }
    }
}

impl<S: InputSink> Drop for TrackingInjector<S> {
    fn drop(&mut self) {
        let _ = self.release_everything();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Records every batch; can be told to fail.
    #[derive(Clone, Default)]
    pub(crate) struct RecordingSink {
        pub batches: Arc<Mutex<Vec<Vec<RawInput>>>>,
        pub fail: Arc<Mutex<bool>>,
    }

    impl InputSink for RecordingSink {
        fn send(&mut self, batch: &[RawInput]) -> Result<(), InputError> {
            if *self.fail.lock().unwrap() {
                return Err(InputError::Blocked("test".into()));
            }
            self.batches.lock().unwrap().push(batch.to_vec());
            Ok(())
        }
    }

    impl RecordingSink {
        fn take(&self) -> Vec<Vec<RawInput>> {
            std::mem::take(&mut *self.batches.lock().unwrap())
        }
    }

    fn unicode_units(batch: &[RawInput]) -> Vec<(u16, bool)> {
        batch
            .iter()
            .map(|r| match r {
                RawInput::Unicode { unit, down } => (*unit, *down),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[test]
    fn text_is_one_batch_of_down_up_pairs() {
        let (mut inj, sink) = injector();
        inj.inject(&InputEvent::Text { text: "بa".into() }).unwrap();
        let batches = sink.take();
        assert_eq!(batches.len(), 1);
        assert_eq!(
            unicode_units(&batches[0]),
            vec![(0x0628, true), (0x0628, false), (0x61, true), (0x61, false)]
        );
        // Typing text never leaves anything held.
        assert_eq!(inj.pressed_keys().count(), 0);
    }

    #[test]
    fn text_outside_the_bmp_is_a_surrogate_pair() {
        let (mut inj, sink) = injector();
        inj.inject(&InputEvent::Text {
            text: "😀".into()
        })
        .unwrap();
        let batches = sink.take();
        assert_eq!(
            unicode_units(&batches[0]),
            vec![
                (0xD83D, true),
                (0xD83D, false),
                (0xDE00, true),
                (0xDE00, false)
            ]
        );
    }

    #[test]
    fn newline_becomes_carriage_return_and_tab_is_kept() {
        let raw = text_to_raw("\n\t").unwrap();
        assert_eq!(
            unicode_units(&raw),
            vec![(0x0D, true), (0x0D, false), (0x09, true), (0x09, false)]
        );
    }

    #[test]
    fn hostile_text_is_dropped() {
        let (mut inj, sink) = injector();
        for text in [
            String::new(),
            "a\u{1b}b".into(),
            "x\0".into(),
            "\u{7f}".into(),
            "ب".repeat(MAX_INPUT_TEXT_CHARS + 1),
        ] {
            inj.inject(&InputEvent::Text { text }).unwrap();
        }
        assert!(sink.take().is_empty());
        // The cap itself is allowed.
        let max = "ب".repeat(MAX_INPUT_TEXT_CHARS);
        inj.inject(&InputEvent::Text { text: max }).unwrap();
        assert_eq!(sink.take()[0].len(), MAX_INPUT_TEXT_CHARS * 2);
    }

    #[test]
    fn failed_text_send_is_reported() {
        let (mut inj, sink) = injector();
        *sink.fail.lock().unwrap() = true;
        assert!(inj.inject(&InputEvent::Text { text: "a".into() }).is_err());
    }

    fn key(scancode: u16, extended: bool, down: bool) -> InputEvent {
        InputEvent::Key {
            scancode,
            extended,
            down,
        }
    }

    fn injector() -> (TrackingInjector<RecordingSink>, RecordingSink) {
        let sink = RecordingSink::default();
        let inj = TrackingInjector::new(sink.clone(), MonitorRect::new(-1920, 200, 1920, 1080));
        (inj, sink)
    }

    #[test]
    fn mouse_move_maps_to_monitor_pixels() {
        let (mut inj, sink) = injector();
        inj.inject(&InputEvent::MouseMove { x: 0, y: 0 }).unwrap();
        inj.inject(&InputEvent::MouseMove {
            x: u16::MAX,
            y: u16::MAX,
        })
        .unwrap();
        assert_eq!(
            sink.take(),
            vec![
                vec![RawInput::MoveTo { x: -1920, y: 200 }],
                vec![RawInput::MoveTo { x: -1, y: 1279 }]
            ]
        );
        inj.set_monitor(MonitorRect::new(0, 0, 100, 100));
        assert_eq!(inj.monitor(), MonitorRect::new(0, 0, 100, 100));
        inj.inject(&InputEvent::MouseMove { x: 0, y: u16::MAX })
            .unwrap();
        assert_eq!(sink.take(), vec![vec![RawInput::MoveTo { x: 0, y: 99 }]]);
    }

    #[test]
    fn wheel_splits_axes_and_skips_zero() {
        let (mut inj, sink) = injector();
        inj.inject(&InputEvent::MouseWheel { dx: 0, dy: 0 })
            .unwrap();
        inj.inject(&InputEvent::MouseWheel { dx: -120, dy: 240 })
            .unwrap();
        assert_eq!(
            sink.take(),
            vec![vec![
                RawInput::Wheel {
                    horizontal: false,
                    delta: 240
                },
                RawInput::Wheel {
                    horizontal: true,
                    delta: -120
                }
            ]]
        );
    }

    #[test]
    fn release_all_releases_exactly_what_is_held() {
        let (mut inj, sink) = injector();
        // Ctrl+Alt held, A pressed and released, Win held (via E0 prefix form), X1 held.
        inj.inject(&key(0x1D, false, true)).unwrap();
        inj.inject(&key(0x38, false, true)).unwrap();
        inj.inject(&key(0x1E, false, true)).unwrap();
        inj.inject(&key(0x1E, false, true)).unwrap(); // auto-repeat
        inj.inject(&key(0x1E, false, false)).unwrap();
        inj.inject(&key(0xE05B, false, true)).unwrap();
        inj.inject(&InputEvent::MouseButton {
            button: MouseButton::Back,
            down: true,
        })
        .unwrap();
        inj.inject(&InputEvent::MouseButton {
            button: MouseButton::Left,
            down: true,
        })
        .unwrap();
        inj.inject(&InputEvent::MouseButton {
            button: MouseButton::Left,
            down: false,
        })
        .unwrap();
        let mut held: Vec<_> = inj.pressed_keys().collect();
        held.sort();
        assert_eq!(
            held,
            vec![ScanKey::LEFT_CTRL, ScanKey::LEFT_ALT, ScanKey::LEFT_WIN]
        );
        assert_eq!(
            inj.pressed_buttons().collect::<Vec<_>>(),
            vec![MouseButton::Back]
        );
        sink.take();

        inj.release_all().unwrap();
        let batches = sink.take();
        assert_eq!(batches.len(), 1, "ReleaseAll must be one atomic batch");
        let batch = &batches[0];
        assert_eq!(batch.len(), 4);
        for k in [ScanKey::LEFT_CTRL, ScanKey::LEFT_ALT, ScanKey::LEFT_WIN] {
            assert!(batch.contains(&RawInput::Key {
                key: k,
                down: false
            }));
        }
        assert_eq!(
            batch[3],
            RawInput::Button {
                button: MouseButton::Back,
                down: false
            }
        );
        // Nothing held any more: a second ReleaseAll sends nothing.
        inj.release_all().unwrap();
        assert!(sink.take().is_empty());
    }

    #[test]
    fn drop_releases_held_keys() {
        let (mut inj, sink) = injector();
        inj.inject(&key(0x2A, false, true)).unwrap();
        sink.take();
        drop(inj);
        assert_eq!(
            sink.take(),
            vec![vec![RawInput::Key {
                key: ScanKey::LEFT_SHIFT,
                down: false
            }]]
        );
    }

    #[test]
    fn failed_release_is_retried_later() {
        let (mut inj, sink) = injector();
        inj.inject(&key(0x1D, true, true)).unwrap();
        *sink.fail.lock().unwrap() = true;
        assert!(inj.release_all().is_err());
        *sink.fail.lock().unwrap() = false;
        sink.take();
        inj.release_all().unwrap();
        assert_eq!(
            sink.take(),
            vec![vec![RawInput::Key {
                key: ScanKey::RIGHT_CTRL,
                down: false
            }]]
        );
    }

    #[test]
    fn failed_down_is_not_tracked() {
        let (mut inj, sink) = injector();
        *sink.fail.lock().unwrap() = true;
        assert!(inj.inject(&key(0x1E, false, true)).is_err());
        *sink.fail.lock().unwrap() = false;
        assert_eq!(inj.pressed_keys().count(), 0);
    }

    #[test]
    fn unrepresentable_keys_are_ignored() {
        let (mut inj, sink) = injector();
        inj.inject(&key(0, false, true)).unwrap();
        inj.inject(&key(0xE11D, false, true)).unwrap();
        assert!(sink.take().is_empty());
        assert_eq!(inj.pressed_keys().count(), 0);
        let _ = inj.sink();
    }

    #[test]
    fn extended_flag_distinguishes_keys() {
        let (mut inj, sink) = injector();
        inj.inject(&key(0x1D, false, true)).unwrap();
        inj.inject(&key(0x1D, true, true)).unwrap();
        inj.inject(&key(0x1D, false, false)).unwrap();
        assert_eq!(
            inj.pressed_keys().collect::<Vec<_>>(),
            vec![ScanKey::RIGHT_CTRL]
        );
        sink.take();
    }
}
