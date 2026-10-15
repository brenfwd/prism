//! Address management: subnet suggestions, gateway and guest leases, VNIs.
//! Pure and deterministic, so the state machine can call it.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use ipnet::Ipv4Net;

use super::{VNI_MAX, VNI_MIN};

/// Carrier-grade NAT space, which Tailscale uses; never handed to guests.
pub const CGNAT: Ipv4Net = Ipv4Net::new_assert(Ipv4Addr::new(100, 64, 0, 0), 10);

/// The `/24`s Prism suggests, in order: `10.88.0.0/24` first, then the rest
/// of `10.88.0.0/16` and onwards, then `172.30.0.0/16` and
/// `192.168.200.0/21`.
fn candidates() -> impl Iterator<Item = Ipv4Net> {
    let ten = (88..=99u8).flat_map(|b| (0..=255u8).map(move |c| Ipv4Addr::new(10, b, c, 0)));
    let one72 = (0..=255u8).map(|c| Ipv4Addr::new(172, 30, c, 0));
    let one92 = (200..=207u8).map(|c| Ipv4Addr::new(192, 168, c, 0));
    ten.chain(one72)
        .chain(one92)
        .map(|a| Ipv4Net::new_assert(a, 24))
}

fn overlaps(a: Ipv4Net, b: Ipv4Net) -> bool {
    a.contains(&b.network()) || b.contains(&a.network())
}

/// The first suggested `/24` that overlaps none of `taken` (other networks,
/// host routes and addresses).
pub fn pick_subnet(taken: &[Ipv4Net]) -> Option<Ipv4Net> {
    candidates().find(|c| !taken.iter().any(|t| overlaps(*c, *t)))
}

/// Whether `net` overlaps any of `others`.
pub fn conflicts(net: Ipv4Net, others: &[Ipv4Net]) -> Option<Ipv4Net> {
    others.iter().copied().find(|o| overlaps(net, *o))
}

/// The lowest host address in `subnet` that is not in `used`.
pub fn first_free(subnet: Ipv4Net, used: &BTreeSet<Ipv4Addr>) -> Option<Ipv4Addr> {
    subnet.hosts().find(|a| !used.contains(a))
}

/// Whether `ip` is a host address of `subnet` (not its network or broadcast
/// address).
pub fn is_host(subnet: Ipv4Net, ip: Ipv4Addr) -> bool {
    subnet.contains(&ip) && ip != subnet.network() && ip != subnet.broadcast()
}

/// Where `ip` of `old` lands in `new` when the networks have the same size:
/// the same host offset. `None` when the sizes differ or it does not fit.
pub fn translate(old: Ipv4Net, new: Ipv4Net, ip: Ipv4Addr) -> Option<Ipv4Addr> {
    if old.prefix_len() != new.prefix_len() || !old.contains(&ip) {
        return None;
    }
    let off = u32::from(ip) - u32::from(old.network());
    let out = Ipv4Addr::from(u32::from(new.network()) + off);
    is_host(new, out).then_some(out)
}

/// The lowest VNI in [`VNI_MIN`]..=[`VNI_MAX`] not in `unavailable`.
pub fn alloc_vni(unavailable: &BTreeSet<u32>) -> Option<u32> {
    (VNI_MIN..=VNI_MAX).find(|v| !unavailable.contains(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(s: &str) -> Ipv4Net {
        s.parse().unwrap()
    }

    #[test]
    fn subnets_avoid_conflicts() {
        assert_eq!(pick_subnet(&[]), Some(net("10.88.0.0/24")));
        assert_eq!(
            pick_subnet(&[net("10.88.0.0/24"), net("10.88.1.0/24")]),
            Some(net("10.88.2.0/24"))
        );
        // A host route covering everything in 10/8 pushes past it.
        assert_eq!(
            pick_subnet(&[net("10.0.0.0/8")]),
            Some(net("172.30.0.0/24"))
        );
        // A narrower host route inside a candidate also counts.
        assert_eq!(
            pick_subnet(&[net("10.88.0.128/25")]),
            Some(net("10.88.1.0/24"))
        );
        assert_eq!(
            conflicts(net("10.88.0.0/16"), &[net("10.88.3.0/24")]),
            Some(net("10.88.3.0/24"))
        );
        assert_eq!(conflicts(net("10.89.0.0/24"), &[net("10.88.0.0/16")]), None);
    }

    #[test]
    fn leases() {
        let s = net("10.88.0.0/29");
        let mut used = BTreeSet::new();
        for want in 1..=6 {
            let a = first_free(s, &used).unwrap();
            assert_eq!(a, Ipv4Addr::new(10, 88, 0, want));
            used.insert(a);
        }
        assert_eq!(
            first_free(s, &used),
            None,
            "the broadcast address is never leased"
        );
        used.remove(&Ipv4Addr::new(10, 88, 0, 3));
        assert_eq!(first_free(s, &used), Some(Ipv4Addr::new(10, 88, 0, 3)));
        assert!(is_host(s, Ipv4Addr::new(10, 88, 0, 6)));
        assert!(!is_host(s, Ipv4Addr::new(10, 88, 0, 7)));
        assert!(!is_host(s, Ipv4Addr::new(10, 88, 0, 0)));
        assert!(!is_host(s, Ipv4Addr::new(10, 88, 1, 1)));
    }

    #[test]
    fn translation_keeps_the_host_offset() {
        let (a, b) = (net("10.88.0.0/24"), net("10.90.4.0/24"));
        assert_eq!(
            translate(a, b, Ipv4Addr::new(10, 88, 0, 7)),
            Some(Ipv4Addr::new(10, 90, 4, 7))
        );
        assert_eq!(
            translate(a, net("10.90.0.0/23"), Ipv4Addr::new(10, 88, 0, 7)),
            None
        );
        assert_eq!(translate(a, b, Ipv4Addr::new(10, 88, 1, 7)), None);
    }

    #[test]
    fn vnis() {
        assert_eq!(alloc_vni(&BTreeSet::new()), Some(VNI_MIN));
        assert_eq!(
            alloc_vni(&BTreeSet::from([VNI_MIN, VNI_MIN + 1])),
            Some(VNI_MIN + 2)
        );
    }
}
