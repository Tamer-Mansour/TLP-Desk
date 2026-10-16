//! Console setup: make a Windows console print UTF-8, so Arabic output (`--lang ar`, Arabic
//! device names) is not turned into `?`. Elsewhere this is a no-op.

/// Switches the console output code page to UTF-8 (65001). Failure (no console, output
/// redirected to a file or pipe) is ignored: bytes are UTF-8 either way.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn enable_utf8_output() {
    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleOutputCP(code_page: u32) -> i32;
    }
    // SAFETY: `SetConsoleOutputCP` takes a plain integer and has no memory-safety
    // preconditions; the return value only reports success.
    let _ = unsafe { SetConsoleOutputCP(65001) };
}

/// No-op outside Windows (terminals there are already UTF-8).
#[cfg(not(windows))]
pub fn enable_utf8_output() {}
