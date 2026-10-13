//! Real `SendInput` test: moves the actual cursor. Run manually on an unlocked desktop:
//! `cargo test -p tlp-desk-input --test real_injection -- --ignored --nocapture`
#![cfg(windows)]

use tlp_desk_input::{ensure_dpi_aware, InputEvent, InputInjector, MonitorRect, WindowsInjector};
use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

unsafe extern "system" fn collect(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `m` is the monitor handle passed by EnumDisplayMonitors; `info` is writable
    // with cbSize set; `data` is the &mut Vec we passed to EnumDisplayMonitors.
    unsafe {
        if GetMonitorInfoW(m, &mut info).as_bool() {
            let r = info.rcMonitor;
            let out = &mut *(data.0 as *mut Vec<MonitorRect>);
            out.push(MonitorRect::new(
                r.left,
                r.top,
                (r.right - r.left) as u32,
                (r.bottom - r.top) as u32,
            ));
        }
    }
    BOOL(1)
}

fn monitors() -> Vec<MonitorRect> {
    let mut out: Vec<MonitorRect> = Vec::new();
    // SAFETY: the callback only runs during this call, while `out` is alive.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut out as *mut Vec<MonitorRect> as isize),
        );
    }
    out
}

fn cursor() -> (i32, i32) {
    let mut p = POINT::default();
    // SAFETY: `p` is a valid, writable POINT.
    unsafe { GetCursorPos(&mut p) }.expect("GetCursorPos");
    (p.x, p.y)
}

#[test]
#[ignore = "moves the real mouse cursor; needs an interactive, unlocked desktop"]
fn real_mouse_move_lands_on_exact_pixel() {
    ensure_dpi_aware().expect("per-monitor DPI awareness");
    let original = cursor();
    let mons = monitors();
    assert!(!mons.is_empty());
    println!("monitors: {mons:?}");
    println!(
        "virtual desktop: {:?}",
        tlp_desk_input::virtual_desktop_rect()
    );

    let mut failures = Vec::new();
    let samples: [(u16, u16); 7] = [
        (0, 0),
        (u16::MAX, u16::MAX),
        (u16::MAX, 0),
        (0, u16::MAX),
        (u16::MAX / 2, u16::MAX / 2),
        (12345, 54321),
        (1, 65534),
    ];
    for mon in &mons {
        let mut inj = WindowsInjector::new(*mon).unwrap();
        for &(x, y) in &samples {
            inj.inject(&InputEvent::MouseMove { x, y }).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(30));
            let expected = mon.normalized_to_pixel(x, y);
            let got = cursor();
            println!("{mon:?} ({x},{y}) expected {expected:?} got {got:?}");
            if got != expected {
                failures.push((*mon, (x, y), expected, got));
            }
        }
    }
    // SAFETY: plain coordinates.
    let _ = unsafe { SetCursorPos(original.0, original.1) };
    assert!(failures.is_empty(), "mismatches: {failures:?}");
}
