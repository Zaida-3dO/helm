//! Waking a PC that is off or asleep.
//!
//! The hub never knows a PC's hardware address by itself: the PC's launcher reports it (see
//! `launcher.rs`) and the hub keeps it with the machine. Waking is off until `HUB_WAKE` says how:
//!
//! | `HUB_WAKE` | what the hub does |
//! |---|---|
//! | `off` (default) | nothing: no Wake button is ever offered |
//! | `udp` | sends a magic packet by UDP broadcast to `HUB_WAKE_BROADCAST`, or else to the broadcast address the launcher reported |
//! | `ha` | asks Home Assistant to send it: `POST <HUB_WAKE_HA_URL>/api/services/wake_on_lan/send_magic_packet` with `HUB_WAKE_HA_TOKEN` as the bearer token |
//!
//! A hub in a container on a bridge network may find that a UDP broadcast never leaves the
//! container; the `ha` path does not have that problem. The token is a secret: it is never
//! logged and never part of an error.
//!
//! What a launcher reports is checked before it is kept: the hardware address must be six hex
//! pairs, and the broadcast address a private IPv4 address, so a machine cannot aim the hub at
//! an address outside the home network.

use serde_json::json;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

/// The port a magic packet is sent to.
pub const WOL_PORT: u16 = 9;

/// How long the Home Assistant call may take.
const HA_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    Udp,
    Ha,
}

/// How this hub wakes a PC, from the environment.
#[derive(Clone)]
pub struct Wake {
    mode: Mode,
    broadcast: Option<Ipv4Addr>,
    ha_url: Option<String>,
    ha_token: Option<String>,
    port: u16,
}

// Never prints the address of Home Assistant or its token.
impl std::fmt::Debug for Wake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wake")
            .field("mode", &self.mode)
            .field("broadcast", &self.broadcast.is_some())
            .field("ha_url", &self.ha_url.is_some())
            .field("ha_token", &self.ha_token.is_some())
            .finish()
    }
}

impl Default for Wake {
    fn default() -> Self {
        Wake::off()
    }
}

impl Wake {
    /// Waking switched off.
    pub fn off() -> Self {
        Wake {
            mode: Mode::Off,
            broadcast: None,
            ha_url: None,
            ha_token: None,
            port: WOL_PORT,
        }
    }

    /// Send by UDP broadcast, to `broadcast` when given, else to what the PC reported, on
    /// `port` (tests use another port than [`WOL_PORT`]).
    pub fn udp(broadcast: Option<Ipv4Addr>, port: u16) -> Self {
        Wake {
            mode: Mode::Udp,
            broadcast,
            port,
            ..Wake::off()
        }
    }

    pub fn from_env() -> Result<Wake, String> {
        Self::from_vars(&|k| std::env::var(k).ok())
    }

    pub fn from_vars(get: &dyn Fn(&str) -> Option<String>) -> Result<Wake, String> {
        let var = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let mode = match var("HUB_WAKE").as_deref() {
            None | Some("off") => return Ok(Wake::off()),
            Some("udp") => Mode::Udp,
            Some("ha") => Mode::Ha,
            Some(_) => return Err("HUB_WAKE must be off, udp or ha".to_owned()),
        };
        let broadcast = match var("HUB_WAKE_BROADCAST") {
            Some(b) => Some(
                b.parse::<Ipv4Addr>()
                    .map_err(|_| "HUB_WAKE_BROADCAST must be an IPv4 address".to_owned())?,
            ),
            None => None,
        };
        let (mut ha_url, mut ha_token) = (None, None);
        if mode == Mode::Ha {
            let url = var("HUB_WAKE_HA_URL")
                .filter(|u| u.starts_with("http://") || u.starts_with("https://"));
            match (url, var("HUB_WAKE_HA_TOKEN")) {
                (Some(u), Some(t)) => {
                    ha_url = Some(u.trim_end_matches('/').to_owned());
                    ha_token = Some(t);
                }
                _ => {
                    return Err("HUB_WAKE=ha needs HUB_WAKE_HA_URL (an http:// or https:// \
                                address) and HUB_WAKE_HA_TOKEN"
                        .to_owned())
                }
            }
        }
        Ok(Wake {
            mode,
            broadcast,
            ha_url,
            ha_token,
            port: WOL_PORT,
        })
    }

    /// Whether the hub can wake a PC at all. Without it no Wake button is shown.
    pub fn enabled(&self) -> bool {
        self.mode != Mode::Off
    }

    /// A short line for the start-up log.
    pub fn describe(&self) -> &'static str {
        match self.mode {
            Mode::Off => "off",
            Mode::Udp => "udp",
            Mode::Ha => "home assistant",
        }
    }

    /// Wake the PC. The error is a plain sentence and never holds a secret.
    pub async fn send(&self, info: &WakeInfo) -> Result<(), String> {
        match self.mode {
            Mode::Off => Err("waking is off on this hub".to_owned()),
            Mode::Udp => {
                let target = self.broadcast.or(info.broadcast).ok_or_else(|| {
                    "the hub does not know this PC's broadcast address; set HUB_WAKE_BROADCAST"
                        .to_owned()
                })?;
                let (packet, port) = (magic_packet(info.mac), self.port);
                crate::stt::blocking(move || send_udp(&packet, target, port)).await
            }
            Mode::Ha => {
                let (Some(url), Some(token)) = (self.ha_url.clone(), self.ha_token.clone()) else {
                    return Err("Home Assistant is not configured".to_owned());
                };
                let mut body = json!({ "mac": info.mac_text() });
                if let Some(b) = self.broadcast.or(info.broadcast) {
                    body["broadcast_address"] = json!(b.to_string());
                }
                crate::stt::blocking(move || call_home_assistant(&url, &token, &body)).await
            }
        }
    }
}

fn send_udp(packet: &[u8], target: Ipv4Addr, port: u16) -> Result<(), String> {
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|_| "could not open a socket".to_owned())?;
    socket
        .set_broadcast(true)
        .map_err(|_| "could not turn broadcast on".to_owned())?;
    socket
        .send_to(packet, SocketAddr::from((target, port)))
        .map_err(|_| "the magic packet could not be sent".to_owned())?;
    Ok(())
}

fn call_home_assistant(url: &str, token: &str, body: &serde_json::Value) -> Result<(), String> {
    let agent = crate::stt::agent_with(HA_TIMEOUT);
    let endpoint = format!("{url}/api/services/wake_on_lan/send_magic_packet");
    // The error of the request is left out on purpose: it can name the address, and this call
    // carries a secret in its headers.
    let resp = agent
        .post(&endpoint)
        .header("Authorization", format!("Bearer {token}"))
        .send_json(body)
        .map_err(|_| "Home Assistant could not be reached".to_owned())?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(format!("Home Assistant answered HTTP {status}"))
    }
}

/// What a PC's launcher said about how to wake it, already checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakeInfo {
    pub mac: [u8; 6],
    pub broadcast: Option<Ipv4Addr>,
}

impl WakeInfo {
    /// A hardware address (six hex pairs) and, if it is a private IPv4 address, a broadcast
    /// address. A bad broadcast address is dropped and the rest kept; a bad hardware address
    /// leaves nothing.
    pub fn parse(mac: &str, broadcast: Option<&str>) -> Option<WakeInfo> {
        Some(WakeInfo {
            mac: parse_mac(mac)?,
            broadcast: broadcast.and_then(private_ipv4),
        })
    }

    /// `AA:BB:CC:DD:EE:FF`.
    pub fn mac_text(&self) -> String {
        mac_text(self.mac)
    }
}

/// `AA:BB:CC:DD:EE:FF` for six bytes.
pub fn mac_text(mac: [u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Six hex pairs, separated by `:` or `-`. The all-zero and all-ones addresses are not a
/// machine's.
pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.trim().split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (slot, part) in mac.iter_mut().zip(parts) {
        if part.len() != 2 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        *slot = u8::from_str_radix(part, 16).ok()?;
    }
    (mac != [0; 6] && mac != [0xff; 6]).then_some(mac)
}

/// An IPv4 address in 10.0.0.0/8, 172.16.0.0/12 or 192.168.0.0/16.
pub fn private_ipv4(s: &str) -> Option<Ipv4Addr> {
    let ip: Ipv4Addr = s.trim().parse().ok()?;
    ip.is_private().then_some(ip)
}

/// The magic packet: six bytes of `FF`, then the hardware address sixteen times.
pub fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut packet = [0xffu8; 102];
    for i in 0..16 {
        packet[6 + i * 6..12 + i * 6].copy_from_slice(&mac);
    }
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_magic_packet_and_the_addresses_a_pc_may_report() {
        let packet = magic_packet([1, 2, 3, 4, 5, 6]);
        assert_eq!(packet.len(), 102);
        assert_eq!(packet[..6], [0xff; 6]);
        assert!(packet[6..].chunks(6).all(|c| c == [1, 2, 3, 4, 5, 6]));

        let macs = [
            ("aa:bb:cc:dd:ee:0f", Some("AA:BB:CC:DD:EE:0F")),
            ("AA-BB-CC-DD-EE-0F", Some("AA:BB:CC:DD:EE:0F")),
            (" 01:02:03:04:05:06 ", Some("01:02:03:04:05:06")),
            ("00:00:00:00:00:00", None),
            ("ff:ff:ff:ff:ff:ff", None),
            ("aa:bb:cc:dd:ee", None),
            ("aa:bb:cc:dd:ee:ff:00", None),
            ("aa:bb:cc:dd:ee:gg", None),
            ("a:bb:cc:dd:ee:ff", None),
            ("aabbccddeeff", None),
            ("", None),
        ];
        for (input, want) in macs {
            assert_eq!(
                parse_mac(input).map(mac_text).as_deref(),
                want,
                "mac {input:?}"
            );
        }

        let ips = [
            ("192.168.1.255", true),
            ("10.0.0.255", true),
            ("172.16.5.255", true),
            ("172.31.255.255", true),
            ("172.32.0.255", false),
            ("8.8.8.8", false),
            ("100.64.0.1", false),
            ("127.0.0.1", false),
            ("255.255.255.255", false),
            ("192.168.1", false),
            ("::1", false),
            ("example.com", false),
        ];
        for (input, ok) in ips {
            assert_eq!(private_ipv4(input).is_some(), ok, "address {input:?}");
        }

        // A bad broadcast is dropped and the hardware address kept; a bad hardware address
        // leaves nothing.
        let info = WakeInfo::parse("aa:bb:cc:dd:ee:ff", Some("8.8.8.8")).unwrap();
        assert_eq!(info.broadcast, None);
        let info = WakeInfo::parse("aa:bb:cc:dd:ee:ff", Some("192.168.0.255")).unwrap();
        assert_eq!(info.broadcast, Some(Ipv4Addr::new(192, 168, 0, 255)));
        assert!(WakeInfo::parse("nonsense", Some("192.168.0.255")).is_none());
    }

    #[test]
    fn the_settings_choose_how_to_wake_and_refuse_half_a_setup() {
        let from = |pairs: &[(&str, &str)]| {
            let pairs: Vec<(String, String)> = pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect();
            Wake::from_vars(&move |k| {
                pairs
                    .iter()
                    .find(|(name, _)| name == k)
                    .map(|(_, v)| v.clone())
            })
        };
        assert!(!from(&[]).unwrap().enabled());
        assert!(!from(&[("HUB_WAKE", "off")]).unwrap().enabled());
        assert!(from(&[("HUB_WAKE", "udp")]).unwrap().enabled());
        assert!(from(&[("HUB_WAKE", "sometimes")]).is_err());
        assert!(from(&[("HUB_WAKE", "udp"), ("HUB_WAKE_BROADCAST", "lan")]).is_err());
        assert!(from(&[("HUB_WAKE", "ha")]).is_err());
        assert!(from(&[("HUB_WAKE", "ha"), ("HUB_WAKE_HA_URL", "http://ha:8123")]).is_err());
        let ha = from(&[
            ("HUB_WAKE", "ha"),
            ("HUB_WAKE_HA_URL", "http://ha:8123/"),
            ("HUB_WAKE_HA_TOKEN", "secret-value"),
        ])
        .unwrap();
        assert!(ha.enabled());
        assert!(
            !format!("{ha:?}").contains("secret-value"),
            "the token must not show in a debug print"
        );
    }
}
