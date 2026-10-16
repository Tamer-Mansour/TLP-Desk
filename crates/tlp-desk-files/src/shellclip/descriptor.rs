//! The bytes of a `FILEGROUPDESCRIPTORW` (clipboard format `CFSTR_FILEDESCRIPTORW`), built by
//! hand so the layout is testable on every platform.
//!
//! Layout (Microsoft Learn, "FILEGROUPDESCRIPTORW" / "FILEDESCRIPTORW"): a `UINT cItems`
//! followed by `cItems` descriptors of 592 bytes each. A descriptor is `dwFlags`, a CLSID,
//! `sizel`, `pointl`, `dwFileAttributes`, three `FILETIME`s, `nFileSizeHigh`, `nFileSizeLow`
//! and a `WCHAR cFileName[260]`. All integers are little-endian.

use tlp_desk_proto::TransferItem;

/// `FD_ATTRIBUTES`: `dwFileAttributes` is valid.
pub const FD_ATTRIBUTES: u32 = 0x0000_0004;
/// `FD_WRITESTIME`: `ftLastWriteTime` is valid.
pub const FD_WRITESTIME: u32 = 0x0000_0020;
/// `FD_FILESIZE`: `nFileSizeHigh` / `nFileSizeLow` are valid.
pub const FD_FILESIZE: u32 = 0x0000_0040;
/// `FD_PROGRESSUI`: the shell shows its progress dialog for the copy.
pub const FD_PROGRESSUI: u32 = 0x0000_4000;

/// `FILE_ATTRIBUTE_DIRECTORY`.
const ATTR_DIRECTORY: u32 = 0x10;
/// `FILE_ATTRIBUTE_NORMAL`.
const ATTR_NORMAL: u32 = 0x80;

/// Size of one `FILEDESCRIPTORW`.
pub const DESCRIPTOR_SIZE: usize = 592;
/// `MAX_PATH`: units of `cFileName`, including the terminating NUL.
pub const NAME_UNITS: usize = 260;

/// Milliseconds between 1601-01-01 (FILETIME epoch) and 1970-01-01.
const EPOCH_DIFF_MS: i64 = 11_644_473_600_000;

/// The result of [`build_group_descriptor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDescriptor {
    /// The `FILEGROUPDESCRIPTORW` bytes.
    pub bytes: Vec<u8>,
    /// For each descriptor (index = `lindex` of `CFSTR_FILECONTENTS`): the index of the source
    /// [`TransferItem`] it describes. Items whose name does not fit `MAX_PATH` are left out,
    /// so the two indexes can differ.
    pub item_index: Vec<usize>,
}

/// Converts Unix milliseconds to a Windows `FILETIME` (100 ns ticks since 1601). Times before
/// 1601 (and the unknown time 0) become 0, which the shell shows as "no date".
pub fn filetime_from_unix_ms(ms: i64) -> u64 {
    if ms <= 0 {
        return 0;
    }
    ms.checked_add(EPOCH_DIFF_MS)
        .and_then(|m| u64::try_from(m).ok())
        .and_then(|m| m.checked_mul(10_000))
        .unwrap_or(0)
}

/// Builds the descriptor group for `items` (folders before their contents, relative
/// `/`-separated paths, as announced on the wire). Names become `\`-separated UTF-16 exactly
/// as sent, so Arabic names survive. Sizes above 4 GiB use `nFileSizeHigh`. Items whose path
/// needs `MAX_PATH` or more UTF-16 units are skipped (the shell cannot take them).
pub fn build_group_descriptor(items: &[TransferItem]) -> GroupDescriptor {
    let mut body = Vec::with_capacity(items.len().saturating_mul(DESCRIPTOR_SIZE));
    let mut item_index = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let name: Vec<u16> = item.rel_path.replace('/', "\\").encode_utf16().collect();
        if name.is_empty() || name.len() >= NAME_UNITS {
            continue;
        }
        push_descriptor(&mut body, item, &name);
        item_index.push(i);
    }
    let count = u32::try_from(item_index.len()).unwrap_or(u32::MAX);
    let mut bytes = Vec::with_capacity(4 + body.len());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&body);
    GroupDescriptor { bytes, item_index }
}

fn push_descriptor(out: &mut Vec<u8>, item: &TransferItem, name: &[u16]) {
    let start = out.len();
    let flags =
        FD_ATTRIBUTES | FD_WRITESTIME | FD_PROGRESSUI | if item.is_dir { 0 } else { FD_FILESIZE };
    out.extend_from_slice(&flags.to_le_bytes()); // dwFlags
    out.extend_from_slice(&[0u8; 16]); // clsid
    out.extend_from_slice(&[0u8; 8]); // sizel
    out.extend_from_slice(&[0u8; 8]); // pointl
    let attrs = if item.is_dir {
        ATTR_DIRECTORY
    } else {
        ATTR_NORMAL
    };
    out.extend_from_slice(&attrs.to_le_bytes()); // dwFileAttributes
    out.extend_from_slice(&[0u8; 8]); // ftCreationTime
    out.extend_from_slice(&[0u8; 8]); // ftLastAccessTime
    let ft = filetime_from_unix_ms(item.modified_unix_ms);
    out.extend_from_slice(&ft.to_le_bytes()); // ftLastWriteTime
    let size = if item.is_dir { 0 } else { item.size };
    out.extend_from_slice(&split_u64(size).0.to_le_bytes()); // nFileSizeHigh
    out.extend_from_slice(&split_u64(size).1.to_le_bytes()); // nFileSizeLow
    for unit in name {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    // cFileName is zero padded up to MAX_PATH units.
    out.resize(start + DESCRIPTOR_SIZE, 0);
}

/// `(high, low)` dwords of `v`.
pub(crate) fn split_u64(v: u64) -> (u32, u32) {
    (
        u32::try_from(v >> 32).unwrap_or(u32::MAX),
        u32::try_from(v & 0xFFFF_FFFF).unwrap_or(u32::MAX),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(rel: &str, size: u64, ms: i64, dir: bool) -> TransferItem {
        TransferItem {
            rel_path: rel.into(),
            size,
            modified_unix_ms: ms,
            is_dir: dir,
        }
    }

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }

    fn name_at(b: &[u8], desc: usize) -> String {
        let base = 4 + desc * DESCRIPTOR_SIZE + 72;
        let units: Vec<u16> = b[base..base + 520]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn layout_matches_filegroupdescriptorw() {
        let g = build_group_descriptor(&[item("a.txt", 5, 0, false)]);
        assert_eq!(g.bytes.len(), 4 + 592);
        assert_eq!(u32_at(&g.bytes, 0), 1);
        let d = 4;
        assert_eq!(u32_at(&g.bytes, d + 36), 0x80); // dwFileAttributes
        assert_eq!(u32_at(&g.bytes, d + 64), 0); // nFileSizeHigh
        assert_eq!(u32_at(&g.bytes, d + 68), 5); // nFileSizeLow
        assert_eq!(name_at(&g.bytes, 0), "a.txt");
        assert_eq!(
            u32_at(&g.bytes, d),
            FD_ATTRIBUTES | FD_WRITESTIME | FD_FILESIZE | FD_PROGRESSUI
        );
    }

    #[test]
    fn arabic_names_and_folders_round_trip() {
        let items = [
            item("مجلد", 0, 0, true),
            item("مجلد/ملف عربي.txt", 12, 0, false),
            item("مجلد/فرعي", 0, 0, true),
            item("مجلد/فرعي/😀.bin", 1, 0, false),
        ];
        let g = build_group_descriptor(&items);
        assert_eq!(g.item_index, vec![0, 1, 2, 3]);
        assert_eq!(name_at(&g.bytes, 0), "مجلد");
        assert_eq!(name_at(&g.bytes, 1), "مجلد\\ملف عربي.txt");
        assert_eq!(name_at(&g.bytes, 3), "مجلد\\فرعي\\😀.bin");
        // A folder is flagged as such and carries no size.
        assert_eq!(u32_at(&g.bytes, 4 + 36), 0x10);
        assert_eq!(u32_at(&g.bytes, 4) & FD_FILESIZE, 0);
    }

    #[test]
    fn sizes_above_4_gib_use_the_high_dword() {
        let size = (3u64 << 32) | 0x1234_5678;
        let g = build_group_descriptor(&[item("big.iso", size, 0, false)]);
        assert_eq!(u32_at(&g.bytes, 4 + 64), 3);
        assert_eq!(u32_at(&g.bytes, 4 + 68), 0x1234_5678);
    }

    #[test]
    fn times_become_windows_file_times() {
        // 1970-01-01 00:00:01 UTC.
        assert_eq!(filetime_from_unix_ms(1000), 116_444_736_010_000_000);
        assert_eq!(filetime_from_unix_ms(0), 0);
        assert_eq!(filetime_from_unix_ms(-5), 0);
        assert_eq!(filetime_from_unix_ms(i64::MAX), 0);
        let g = build_group_descriptor(&[item("t", 0, 1000, false)]);
        let b = &g.bytes[4 + 56..4 + 64];
        assert_eq!(
            u64::from_le_bytes(b.try_into().unwrap()),
            116_444_736_010_000_000
        );
    }

    #[test]
    fn over_long_names_are_skipped_and_indexes_kept() {
        let long = "ا".repeat(260);
        let items = [
            item("one", 1, 0, false),
            item(&long, 1, 0, false),
            item("two", 1, 0, false),
        ];
        let g = build_group_descriptor(&items);
        assert_eq!(g.item_index, vec![0, 2]);
        assert_eq!(u32_at(&g.bytes, 0), 2);
        assert_eq!(name_at(&g.bytes, 1), "two");
        let max = "ب".repeat(259);
        assert_eq!(
            build_group_descriptor(&[item(&max, 1, 0, false)]).item_index,
            vec![0]
        );
    }

    #[test]
    fn empty_input_is_an_empty_group() {
        let g = build_group_descriptor(&[]);
        assert_eq!(g.bytes, vec![0, 0, 0, 0]);
    }
}
