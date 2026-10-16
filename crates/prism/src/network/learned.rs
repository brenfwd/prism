//! The IPv6 addresses guests are seen using, for guests whose address is not the EUI-64 one
//! (stable-privacy addresses, temporary addresses, a static address).
//!
//! Two sources feed it per NIC: the guest agent, which reports the guest's own address list,
//! and the host's neighbour table on the bridge, which holds every address the guest sent
//! neighbour discovery from. Addresses seen on the bridge stay for [`RETAIN`] after the
//! neighbour entry goes away.

use std::collections::HashMap;
use std::net::Ipv6Addr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use prism_net::MacAddr;

/// How long an address stays known after it was last seen.
pub const RETAIN: Duration = Duration::from_secs(30 * 60);
/// Most addresses kept per NIC, oldest first. Bounds the firewall rules a guest can cause.
pub const MAX_PER_NIC: usize = 8;

#[derive(Debug, Default)]
struct Entry {
    /// What the guest agent last reported, in its order.
    agent: Vec<Ipv6Addr>,
    /// Addresses seen in the neighbour table, in the order first seen.
    seen: Vec<(Ipv6Addr, Instant)>,
}

/// Addresses learned per NIC MAC. Shared between the guest agent poller and the network
/// reconciler.
#[derive(Debug, Default)]
pub struct GuestAddrs {
    inner: Mutex<HashMap<MacAddr, Entry>>,
}

impl GuestAddrs {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<MacAddr, Entry>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Replaces what the guest agent reports for `mac`.
    pub fn set_agent(&self, mac: MacAddr, addrs: Vec<Ipv6Addr>) {
        self.lock().entry(mac).or_default().agent = addrs;
    }

    /// Records the neighbour table entries `(address, mac)` seen at `now`. An address already
    /// held by another MAC stays with the first one until it expires.
    pub fn observe(&self, entries: &[(Ipv6Addr, MacAddr)], now: Instant) {
        let mut map = self.lock();
        for (addr, mac) in entries {
            let held_elsewhere = map
                .iter()
                .any(|(m, e)| m != mac && e.seen.iter().any(|(a, t)| a == addr && fresh(*t, now)));
            if held_elsewhere {
                continue;
            }
            let e = map.entry(*mac).or_default();
            match e.seen.iter_mut().find(|(a, _)| a == addr) {
                Some((_, t)) => *t = now,
                None => e.seen.push((*addr, now)),
            }
        }
    }

    /// Forgets expired addresses and returns what is known per MAC: the agent's addresses
    /// first, then the neighbour-table ones, at most [`MAX_PER_NIC`].
    pub fn snapshot(&self, now: Instant) -> HashMap<MacAddr, Vec<Ipv6Addr>> {
        let mut map = self.lock();
        map.retain(|_, e| {
            e.seen.retain(|(_, t)| fresh(*t, now));
            !e.agent.is_empty() || !e.seen.is_empty()
        });
        map.iter()
            .map(|(m, e)| {
                let mut out: Vec<Ipv6Addr> = Vec::new();
                for a in e.agent.iter().chain(e.seen.iter().map(|(a, _)| a)) {
                    if !out.contains(a) {
                        out.push(*a);
                    }
                }
                out.truncate(MAX_PER_NIC);
                (*m, out)
            })
            .collect()
    }
}

fn fresh(seen: Instant, now: Instant) -> bool {
    now.saturating_duration_since(seen) < RETAIN
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac(n: u8) -> MacAddr {
        format!("02:00:00:00:00:{n:02x}").parse().unwrap()
    }

    fn ip(s: &str) -> Ipv6Addr {
        s.parse().unwrap()
    }

    #[test]
    fn agent_addresses_come_before_seen_ones_without_repeats() {
        let g = GuestAddrs::default();
        let t = Instant::now();
        g.observe(&[(ip("fd00::2"), mac(1)), (ip("fd00::1"), mac(1))], t);
        g.set_agent(mac(1), vec![ip("fd00::1"), ip("fd00::9")]);
        assert_eq!(
            g.snapshot(t)[&mac(1)],
            vec![ip("fd00::1"), ip("fd00::9"), ip("fd00::2")]
        );
    }

    #[test]
    fn seen_addresses_expire() {
        let g = GuestAddrs::default();
        let t = Instant::now();
        g.observe(&[(ip("fd00::2"), mac(1))], t);
        assert!(g.snapshot(t + (RETAIN / 2)).contains_key(&mac(1)));
        assert!(g.snapshot(t + RETAIN).is_empty());
    }

    #[test]
    fn seeing_an_address_again_keeps_it() {
        let g = GuestAddrs::default();
        let t = Instant::now();
        g.observe(&[(ip("fd00::2"), mac(1))], t);
        g.observe(&[(ip("fd00::2"), mac(1))], t + RETAIN / 2 * 3);
        assert_eq!(g.snapshot(t + RETAIN * 2)[&mac(1)], vec![ip("fd00::2")]);
    }

    #[test]
    fn the_first_mac_keeps_an_address() {
        let g = GuestAddrs::default();
        let t = Instant::now();
        g.observe(&[(ip("fd00::2"), mac(1))], t);
        g.observe(&[(ip("fd00::2"), mac(2))], t);
        let s = g.snapshot(t);
        assert_eq!(s[&mac(1)], vec![ip("fd00::2")]);
        assert!(!s.contains_key(&mac(2)));
    }

    #[test]
    fn a_nic_keeps_at_most_eight_addresses() {
        let g = GuestAddrs::default();
        let t = Instant::now();
        let many: Vec<_> = (1..=20u16)
            .map(|i| (Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, i), mac(1)))
            .collect();
        g.observe(&many, t);
        let s = g.snapshot(t);
        assert_eq!(s[&mac(1)].len(), MAX_PER_NIC);
        assert_eq!(s[&mac(1)][0], ip("fd00::1"));
    }
}
