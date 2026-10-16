//! Windows neighbour cache via `GetIpNetTable2` (IP Helper API, Microsoft Learn:
//! "GetIpNetTable2 function (netioapi.h)"). Read-only: the call returns the cached table and
//! sends nothing on the network.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use windows::Win32::Foundation::NO_ERROR;
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIpNetTable2, MIB_IPNET_ROW2, MIB_IPNET_TABLE2,
};
use windows::Win32::Networking::WinSock::{
    NlnsIncomplete, NlnsUnreachable, AF_INET, AF_INET6, AF_UNSPEC,
};

use super::{RawRow, RowState};
use crate::NetError;

/// Upper bound on rows we copy out, so a corrupt count cannot make us read without end.
const MAX_ROWS: usize = 65_536;

/// Frees the table returned by `GetIpNetTable2` on every exit path.
struct Table(*mut MIB_IPNET_TABLE2);

impl Drop for Table {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a successful GetIpNetTable2 call and is freed
            // exactly once, here, with the matching FreeMibTable.
            unsafe { FreeMibTable(self.0.cast_const().cast()) };
        }
    }
}

/// All rows of the neighbour cache, IPv4 and IPv6.
pub(super) fn table() -> Result<Vec<RawRow>, NetError> {
    let mut raw: *mut MIB_IPNET_TABLE2 = std::ptr::null_mut();
    // SAFETY: `raw` is a valid out-pointer; on success the OS stores a table it allocated,
    // which `Table` frees.
    let status = unsafe { GetIpNetTable2(AF_UNSPEC, &mut raw) };
    let guard = Table(raw);
    if status != NO_ERROR {
        return Err(NetError::Io(std::io::Error::from_raw_os_error(
            i32::try_from(status.0).unwrap_or(i32::MAX),
        )));
    }
    if guard.0.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: non-null table from a successful call; `NumEntries` is read from the header
    // the OS wrote.
    let count = unsafe { (*guard.0).NumEntries };
    let count = usize::try_from(count).unwrap_or(0).min(MAX_ROWS);
    // SAFETY: the OS allocates `NumEntries` contiguous rows starting at `Table` (a C flexible
    // array declared with length 1); `count` never exceeds `NumEntries`, and the slice lives
    // only while `guard` keeps the allocation alive.
    let rows: &[MIB_IPNET_ROW2] =
        unsafe { std::slice::from_raw_parts(std::ptr::addr_of!((*guard.0).Table).cast(), count) };
    Ok(rows.iter().filter_map(convert).collect())
}

fn convert(row: &MIB_IPNET_ROW2) -> Option<RawRow> {
    // SAFETY: `si_family` overlays the family field of both union variants, so it is always
    // initialized; it tells which variant is valid.
    let family = unsafe { row.Address.si_family };
    let ip = if family == AF_INET {
        // SAFETY: the family says the IPv4 variant is the valid one.
        let octets = unsafe { row.Address.Ipv4.sin_addr.S_un.S_un_b };
        IpAddr::V4(Ipv4Addr::new(
            octets.s_b1,
            octets.s_b2,
            octets.s_b3,
            octets.s_b4,
        ))
    } else if family == AF_INET6 {
        // SAFETY: the family says the IPv6 variant is the valid one.
        let bytes = unsafe { row.Address.Ipv6.sin6_addr.u.Byte };
        IpAddr::V6(Ipv6Addr::from(bytes))
    } else {
        return None;
    };
    let len = usize::try_from(row.PhysicalAddressLength).unwrap_or(0);
    let hw = row.PhysicalAddress.get(..len)?.to_vec();
    let state = if row.State == NlnsUnreachable || row.State == NlnsIncomplete {
        RowState::Unresolved
    } else {
        RowState::Resolved
    };
    Some(RawRow { ip, hw, state })
}
