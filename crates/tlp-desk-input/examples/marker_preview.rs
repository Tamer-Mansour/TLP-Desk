//! Shows the green remote-pointer marker at the current cursor position for a few seconds,
//! *without* capture exclusion, so it can be screenshotted for documentation. Does not move
//! the mouse. Usage: `cargo run -p tlp-desk-input --example marker_preview -- [label] [secs]`
fn main() {
    let mut args = std::env::args().skip(1);
    let label = args.next().unwrap_or_else(|| "viewer-pc".into());
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(4);
    if let Err(e) = tlp_desk_input::ensure_dpi_aware() {
        eprintln!("DPI awareness: {e}");
    }
    match tlp_desk_input::RemotePointerMarker::start_with(&label, false) {
        Ok(m) => {
            m.set_visible(true);
            println!("marker visible for {secs} s");
            std::thread::sleep(std::time::Duration::from_secs(secs));
            m.set_visible(false);
        }
        Err(e) => eprintln!("cannot show marker: {e}"),
    }
}
