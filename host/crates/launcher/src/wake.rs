//! What the hub needs to wake this PC: the MAC of the adapter the launcher reaches the hub
//! through, and that network's directed broadcast address.
//!
//! The adapter is the one holding the line's local IPv4 address. Only a private LAN address
//! (10/8, 172.16/12, 192.168/16) counts: a line over loopback or a VPN has no LAN to wake on.

use crate::frames::Wake;
use std::net::Ipv4Addr;

/// The directed broadcast of `addr`'s network: every host bit set. `None` for a prefix that
/// leaves no host bits (/31, /32) or is not a prefix at all.
pub fn broadcast(addr: Ipv4Addr, prefix: u8) -> Option<Ipv4Addr> {
    if prefix > 30 {
        return None;
    }
    let host_bits = u32::MAX.checked_shr(u32::from(prefix)).unwrap_or(0);
    Some(Ipv4Addr::from(u32::from(addr) | host_bits))
}

/// `AA:BB:CC:DD:EE:FF`, for a six-byte address that is not all zeros.
pub fn mac_text(bytes: &[u8]) -> Option<String> {
    if bytes.len() != 6 || bytes.iter().all(|b| *b == 0) {
        return None;
    }
    Some(
        bytes
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

/// The wake details for the adapter holding `local`, when it is a LAN address.
pub fn for_local_address(local: Ipv4Addr) -> Option<Wake> {
    if !local.is_private() {
        return None;
    }
    let (mac, prefix) = adapter_of(local)?;
    Some(Wake {
        mac: mac_text(&mac)?,
        broadcast: broadcast(local, prefix)?.to_string(),
    })
}

/// The MAC and on-link prefix length of the adapter that holds `local`.
#[cfg(windows)]
fn adapter_of(local: Ipv4Addr) -> Option<(Vec<u8>, u8)> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
        GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::Networking::WinSock::AF_INET;
    const ERROR_BUFFER_OVERFLOW: u32 = 111;
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 16 * 1024;
    for _ in 0..3 {
        // u64 words keep the buffer aligned for the structures the call writes into it.
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let first = buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        // SAFETY: the buffer is `size` bytes (rounded up), aligned, and lives until the walk ends.
        let rc = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_INET),
                flags,
                std::ptr::null(),
                first,
                &mut size,
            )
        };
        if rc == ERROR_BUFFER_OVERFLOW {
            continue;
        }
        if rc != 0 {
            return None;
        }
        let mut adapter = first;
        // SAFETY: a linked list the call built inside `buf`, read only while `buf` lives.
        unsafe {
            while !adapter.is_null() {
                let a = &*adapter;
                let mut uni = a.FirstUnicastAddress;
                while !uni.is_null() {
                    let u = &*uni;
                    let sa = u.Address.lpSockaddr;
                    if !sa.is_null() && (*sa).sa_family == AF_INET {
                        // sockaddr_in: the port's two bytes, then the address's four.
                        let d = (*sa).sa_data;
                        let ip = Ipv4Addr::new(d[2] as u8, d[3] as u8, d[4] as u8, d[5] as u8);
                        if ip == local {
                            let len = (a.PhysicalAddressLength as usize).min(8);
                            return Some((a.PhysicalAddress[..len].to_vec(), u.OnLinkPrefixLength));
                        }
                    }
                    uni = u.Next;
                }
                adapter = a.Next;
            }
        }
        return None;
    }
    None
}

#[cfg(not(windows))]
fn adapter_of(_local: Ipv4Addr) -> Option<(Vec<u8>, u8)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_broadcast_and_mac_are_computed_from_the_adapter() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        #[rustfmt::skip]
        let rows: Vec<(&str, u8, Option<&str>)> = vec![
            ("192.168.1.23", 24, Some("192.168.1.255")),
            ("192.168.1.23", 16, Some("192.168.255.255")),
            ("10.1.2.3",      8, Some("10.255.255.255")),
            ("172.16.5.4",   20, Some("172.16.15.255")),
            ("192.168.1.23", 30, Some("192.168.1.23")),
            ("192.168.1.20", 30, Some("192.168.1.23")),
            ("192.168.1.23",  0, Some("255.255.255.255")),
            ("192.168.1.23", 31, None),
            ("192.168.1.23", 32, None),
            ("192.168.1.23", 64, None),
        ];
        for (addr, prefix, want) in rows {
            assert_eq!(broadcast(ip(addr), prefix), want.map(ip), "{addr}/{prefix}");
        }
        assert_eq!(
            mac_text(&[0xaa, 0xbb, 0x0c, 0xdd, 0xee, 0x01]).as_deref(),
            Some("AA:BB:0C:DD:EE:01")
        );
        assert_eq!(mac_text(&[0; 6]), None, "no address");
        assert_eq!(mac_text(&[1, 2, 3, 4, 5, 6, 7, 8]), None, "not Ethernet");
        // Loopback and VPN addresses are never a LAN to wake on.
        assert_eq!(for_local_address(ip("127.0.0.1")), None);
        assert_eq!(for_local_address(ip("100.100.1.2")), None);
    }
}
