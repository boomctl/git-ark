//! Client-side LAN facts: the client's own IPv4 and the hosts of its /24.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

/// The client's primary IPv4, via the routing table — bind a UDP socket and
/// "connect" it to a public address (no packet is sent; this just selects the
/// default-route interface), then read the local address. `None` if there is no
/// usable route (e.g. an isolated host); the caller then needs `--subnet`.
pub fn local_ipv4() -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()? {
        SocketAddr::V4(a) => Some(*a.ip()),
        SocketAddr::V6(_) => None,
    }
}

/// The 254 usable host addresses of `base`'s /24 (`.1`–`.254`), network and
/// broadcast excluded.
pub fn hosts_in_slash24(base: Ipv4Addr) -> Vec<Ipv4Addr> {
    let o = base.octets();
    (1u8..=254)
        .map(|last| Ipv4Addr::new(o[0], o[1], o[2], last))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn slash24_has_254_hosts_first_and_last() {
        let h = hosts_in_slash24(Ipv4Addr::new(192, 168, 1, 42));
        assert_eq!(h.len(), 254);
        assert_eq!(h[0], Ipv4Addr::new(192, 168, 1, 1));
        assert_eq!(h[253], Ipv4Addr::new(192, 168, 1, 254));
        // network (.0) and broadcast (.255) are excluded
        assert!(!h.contains(&Ipv4Addr::new(192, 168, 1, 0)));
        assert!(!h.contains(&Ipv4Addr::new(192, 168, 1, 255)));
    }
}
