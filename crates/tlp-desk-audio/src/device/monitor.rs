//! Picking the "monitor of the speakers" capture device on Linux.
//!
//! PulseAudio and PipeWire expose what a sound card plays as a *monitor source* (a capture
//! device named `<sink>.monitor`); ALSA has no loopback of its own. The host must never fall
//! back to the default input, which is the microphone, so a device is only used when its name
//! says it is a monitor or the user named one explicitly.

// Only the Linux device code and the tests use it.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

/// Environment variable naming (a substring of) the capture device to use as loopback.
pub(crate) const SOURCE_ENV: &str = "TLP_DESK_AUDIO_SOURCE";

/// Index of the device to capture "what you hear" from, among capture device `names`.
///
/// 1. `hint` (the user's override): the first name containing it, ignoring case. A hint that
///    matches nothing selects nothing; it is never silently ignored.
/// 2. otherwise the first name containing `monitor` (ignoring case).
pub(crate) fn pick_monitor(names: &[String], hint: Option<&str>) -> Option<usize> {
    let find = |needle: &str| {
        let needle = needle.to_lowercase();
        names
            .iter()
            .position(|n| n.to_lowercase().contains(&needle))
    };
    match hint.map(str::trim).filter(|h| !h.is_empty()) {
        Some(h) => find(h),
        None => find("monitor"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn picks_the_monitor_not_the_microphone() {
        let n = names(&[
            "default",
            "alsa_input.usb-mic.analog-stereo",
            "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor",
        ]);
        assert_eq!(pick_monitor(&n, None), Some(2));
    }

    #[test]
    fn no_monitor_means_no_device_never_the_default_input() {
        let n = names(&["default", "pulse", "sysdefault:CARD=PCH"]);
        assert_eq!(pick_monitor(&n, None), None);
        assert_eq!(pick_monitor(&[], None), None);
    }

    #[test]
    fn hint_overrides_and_is_case_insensitive() {
        let n = names(&["Monitor of Speakers", "Monitor of HDMI Output"]);
        assert_eq!(pick_monitor(&n, Some("hdmi")), Some(1));
        assert_eq!(pick_monitor(&n, Some("  ")), Some(0));
        assert_eq!(pick_monitor(&n, Some("bluetooth")), None);
    }

    #[test]
    fn arabic_device_names_do_not_confuse_matching() {
        let n = names(&["سماعات الرأس", "مراقب Monitor سماعات"]);
        assert_eq!(pick_monitor(&n, None), Some(1));
        assert_eq!(pick_monitor(&n, Some("سماعات الرأس")), Some(0));
    }
}
