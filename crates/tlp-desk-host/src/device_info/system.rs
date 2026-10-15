//! [`SystemInfoSource`]: the real machine, read through the `sysinfo` crate (MIT; it wraps
//! the Win32 / NT APIs, so this crate stays free of `unsafe`). Only the CPU list, memory and
//! disks are refreshed; no process, network or user enumeration happens.

use sysinfo::{CpuRefreshKind, Disks, MemoryRefreshKind, RefreshKind, System};
use tlp_desk_proto::{DeviceInfo, DiskInfo};

use super::DeviceInfoSource;

/// Reads this computer's OS, CPU, RAM, disks, uptime and host name.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemInfoSource;

impl DeviceInfoSource for SystemInfoSource {
    fn collect(&self) -> DeviceInfo {
        let sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing())
                .with_memory(MemoryRefreshKind::nothing().with_ram()),
        );
        let cpu = sys
            .cpus()
            .first()
            .map(|c| c.brand().trim().to_owned())
            .unwrap_or_default();
        let cores = u32::try_from(sys.cpus().len()).unwrap_or(u32::MAX);
        let disks = Disks::new_with_refreshed_list()
            .list()
            .iter()
            .map(|d| DiskInfo {
                mount: d.mount_point().to_string_lossy().into_owned(),
                label: d.name().to_string_lossy().into_owned(),
                total_bytes: d.total_space(),
                free_bytes: d.available_space(),
            })
            .collect();
        DeviceInfo {
            os: System::long_os_version()
                .or_else(System::name)
                .unwrap_or_default(),
            version: os_version(),
            cpu,
            cores,
            ram_bytes: sys.total_memory(),
            disks,
            uptime_s: System::uptime(),
            hostname: System::host_name().unwrap_or_default(),
        }
    }
}

/// "10.0.<build>" on Windows (10 and 11 both report NT 10.0; sysinfo gives the build number),
/// the kernel version elsewhere.
fn os_version() -> String {
    let kernel = System::kernel_version().unwrap_or_default();
    version_string(cfg!(windows), &kernel)
}

fn version_string(windows: bool, kernel: &str) -> String {
    if windows && !kernel.is_empty() && kernel.bytes().all(|b| b.is_ascii_digit()) {
        format!("10.0.{kernel}")
    } else {
        kernel.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_build_becomes_nt_version() {
        assert_eq!(version_string(true, "26100"), "10.0.26100");
        assert_eq!(version_string(true, ""), "");
        assert_eq!(version_string(false, "6.8.0-45"), "6.8.0-45");
    }

    /// Reads only; touches nothing. Sanity-checks what this machine reports.
    #[test]
    fn real_machine_reports_plausible_values() {
        let i = SystemInfoSource.collect();
        assert!(!i.os.is_empty(), "{i:?}");
        assert!(i.cores >= 1, "{i:?}");
        assert!(i.ram_bytes > 0, "{i:?}");
        assert!(
            i.disks.iter().all(|d| d.free_bytes <= d.total_bytes),
            "{i:?}"
        );
    }
}
