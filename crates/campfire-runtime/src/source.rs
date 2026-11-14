//! Request source identification for per-source admission limits.
use std::{
    collections::HashMap,
    fmt,
    net::{IpAddr, Ipv6Addr},
    str::FromStr,
    sync::{Arc, Mutex},
};

/// A reverse proxy network whose `X-Forwarded-For` header is trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrustedProxy {
    network: IpAddr,
    prefix: u8,
}
impl TrustedProxy {
    pub fn contains(&self, address: IpAddr) -> bool {
        match (self.network, canonical(address)) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                mask(u128::from(u32::from(address)), self.prefix, 32)
                    == u128::from(u32::from(network))
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                mask(u128::from(address), self.prefix, 128) == u128::from(network)
            }
            _ => false,
        }
    }
}
impl FromStr for TrustedProxy {
    type Err = String;
    /// Parses `ADDRESS` or `ADDRESS/PREFIX`; host bits below the prefix are ignored.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("invalid trusted proxy network: {value}");
        let (address, prefix) = match value.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (value, None),
        };
        let address = canonical(address.parse().map_err(|_| invalid())?);
        let width = if address.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(prefix) => prefix.parse::<u8>().map_err(|_| invalid())?,
            None => width,
        };
        if prefix > width {
            return Err(invalid());
        }
        let network = match address {
            IpAddr::V4(address) => {
                IpAddr::V4((mask(u128::from(u32::from(address)), prefix, 32) as u32).into())
            }
            IpAddr::V6(address) => IpAddr::V6(mask(u128::from(address), prefix, 128).into()),
        };
        Ok(Self { network, prefix })
    }
}
impl fmt::Display for TrustedProxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

fn mask(value: u128, prefix: u8, width: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        value & (u128::MAX << (width - prefix)) & (u128::MAX >> (128 - width))
    }
}

fn canonical(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address, IpAddr::V4),
        v4 => v4,
    }
}

/// Resolves the client address for a request.
///
/// Forwarded entries are honoured only while the hop that reported them is a trusted proxy,
/// walking from the nearest hop outwards. A malformed entry ends the walk at the last verified hop.
pub(crate) fn client_address<'a>(
    peer: IpAddr,
    forwarded: impl DoubleEndedIterator<Item = &'a str>,
    trusted: &[TrustedProxy],
) -> IpAddr {
    let is_trusted = |address: IpAddr| trusted.iter().any(|proxy| proxy.contains(address));
    let mut client = canonical(peer);
    for entry in forwarded.rev() {
        if !is_trusted(client) {
            break;
        }
        let Ok(address) = entry.trim().parse::<IpAddr>() else {
            break;
        };
        client = canonical(address);
    }
    client
}

/// Splits one `X-Forwarded-For` header value into its non-empty entries.
pub(crate) fn forwarded_entries(value: &str) -> impl DoubleEndedIterator<Item = &str> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
}

/// The admission key for an address: IPv4 hosts individually, IPv6 by /64 allocation.
pub(crate) fn source_key(address: IpAddr) -> String {
    match canonical(address) {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            format!("{}/64", Ipv6Addr::from(mask(u128::from(v6), 64, 128)))
        }
    }
}

/// Counts unauthenticated connections per source; entries exist only while a slot is held.
#[derive(Default)]
pub(crate) struct SourceSlots {
    held: Mutex<HashMap<String, usize>>,
}
impl SourceSlots {
    pub fn acquire(self: &Arc<Self>, source: &str, limit: usize) -> Option<SourceSlot> {
        let mut held = self.held.lock().unwrap_or_else(|error| error.into_inner());
        let count = held.get(source).copied().unwrap_or(0);
        if count >= limit {
            return None;
        }
        held.insert(source.to_owned(), count + 1);
        Some(SourceSlot {
            slots: self.clone(),
            source: source.to_owned(),
        })
    }
    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.held.lock().unwrap().len()
    }
}
pub(crate) struct SourceSlot {
    slots: Arc<SourceSlots>,
    source: String,
}
impl Drop for SourceSlot {
    fn drop(&mut self) {
        let mut held = self
            .slots
            .held
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match held.get_mut(&self.source) {
            Some(count) if *count > 1 => *count -= 1,
            _ => {
                held.remove(&self.source);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }
    fn proxies(values: &[&str]) -> Vec<TrustedProxy> {
        values.iter().map(|value| value.parse().unwrap()).collect()
    }
    fn resolve(peer: &str, forwarded: &str, trusted: &[&str]) -> String {
        client_address(ip(peer), forwarded_entries(forwarded), &proxies(trusted)).to_string()
    }
    #[test]
    fn parses_and_matches_networks() {
        let v4: TrustedProxy = "10.1.2.3/8".parse().unwrap();
        assert_eq!(v4.to_string(), "10.0.0.0/8");
        assert!(v4.contains(ip("10.255.0.1")));
        assert!(!v4.contains(ip("11.0.0.1")));
        assert!(v4.contains(ip("::ffff:10.0.0.9")));
        let host: TrustedProxy = "127.0.0.1".parse().unwrap();
        assert!(host.contains(ip("127.0.0.1")) && !host.contains(ip("127.0.0.2")));
        let v6: TrustedProxy = "fd00::/8".parse().unwrap();
        assert!(v6.contains(ip("fd12::1")) && !v6.contains(ip("fe80::1")));
        assert!(!v6.contains(ip("10.0.0.1")));
        let any: TrustedProxy = "0.0.0.0/0".parse().unwrap();
        assert!(any.contains(ip("203.0.113.9")) && !any.contains(ip("2001:db8::1")));
        for invalid in [
            "",
            "10.0.0.0/33",
            "::/129",
            "10.0.0.0/-1",
            "proxy",
            "10.0.0.0/",
        ] {
            assert!(invalid.parse::<TrustedProxy>().is_err(), "{invalid}");
        }
    }
    #[test]
    fn forwarded_header_is_ignored_from_untrusted_peers() {
        assert_eq!(resolve("203.0.113.9", "198.51.100.1", &[]), "203.0.113.9");
        assert_eq!(
            resolve("203.0.113.9", "198.51.100.1", &["10.0.0.0/8"]),
            "203.0.113.9"
        );
    }
    #[test]
    fn nearest_untrusted_hop_is_the_client() {
        let trusted = ["10.0.0.0/8"];
        assert_eq!(
            resolve("10.0.0.1", "198.51.100.1", &trusted),
            "198.51.100.1"
        );
        // A client-supplied leading entry cannot displace the hop the proxy observed.
        assert_eq!(
            resolve("10.0.0.1", "1.2.3.4, 198.51.100.1", &trusted),
            "198.51.100.1"
        );
        assert_eq!(
            resolve("10.0.0.1", "198.51.100.1, 10.0.0.2", &trusted),
            "198.51.100.1"
        );
        assert_eq!(
            resolve("10.0.0.1", "10.0.0.3, 10.0.0.2", &trusted),
            "10.0.0.3"
        );
        assert_eq!(resolve("10.0.0.1", "", &trusted), "10.0.0.1");
        // Empty entries, such as a trailing comma, are not hops.
        assert_eq!(
            resolve("10.0.0.1", "198.51.100.1, ,", &trusted),
            "198.51.100.1"
        );
    }
    #[test]
    fn malformed_entry_stops_at_last_verified_hop() {
        let trusted = ["10.0.0.0/8"];
        assert_eq!(resolve("10.0.0.1", "unknown", &trusted), "10.0.0.1");
        assert_eq!(
            resolve("10.0.0.1", "198.51.100.1, garbage, 10.0.0.2", &trusted),
            "10.0.0.2"
        );
    }
    #[test]
    fn keys_group_ipv6_allocations_and_unmap_ipv4() {
        assert_eq!(source_key(ip("198.51.100.7")), "198.51.100.7");
        assert_eq!(source_key(ip("::ffff:198.51.100.7")), "198.51.100.7");
        assert_eq!(
            source_key(ip("2001:db8:1:2:aaaa::1")),
            source_key(ip("2001:db8:1:2:bbbb::2"))
        );
        assert_eq!(source_key(ip("2001:db8:1:2::1")), "2001:db8:1:2::/64");
        assert_ne!(
            source_key(ip("2001:db8:1:2::1")),
            source_key(ip("2001:db8:1:3::1"))
        );
    }
    #[test]
    fn slots_enforce_the_limit_and_forget_released_sources() {
        let slots = Arc::new(SourceSlots::default());
        let first = slots.acquire("a", 2).unwrap();
        let second = slots.acquire("a", 2).unwrap();
        assert!(slots.acquire("a", 2).is_none());
        let other = slots.acquire("b", 2).unwrap();
        drop(first);
        let third = slots.acquire("a", 2).unwrap();
        drop((second, third, other));
        assert_eq!(slots.tracked(), 0);
    }
}
