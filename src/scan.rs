//! Bounded-parallel TCP port probe — the LAN discovery scanner.

use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::Mutex;
use std::time::Duration;

/// Probe `port` on each of `ips` with `connect_timeout`, across at most
/// `workers` threads. Returns the reachable addresses, sorted. A bare TCP
/// connect — no data, no auth.
pub fn scan_port(ips: &[Ipv4Addr], port: u16, timeout: Duration, workers: usize) -> Vec<Ipv4Addr> {
    if ips.is_empty() {
        return Vec::new();
    }
    let workers = workers.clamp(1, ips.len());
    let chunk = ips.len().div_ceil(workers);
    let found = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for slice in ips.chunks(chunk) {
            let found = &found;
            s.spawn(move || {
                for &ip in slice {
                    let addr = SocketAddr::from((ip, port));
                    if TcpStream::connect_timeout(&addr, timeout).is_ok() {
                        found.lock().unwrap().push(ip);
                    }
                }
            });
        }
    });
    let mut v = found.into_inner().unwrap();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpListener};
    use std::time::Duration;

    #[test]
    fn finds_a_listening_port_and_skips_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let open = listener.local_addr().unwrap().port();
        let ips = [Ipv4Addr::LOCALHOST];
        let found = scan_port(&ips, open, Duration::from_millis(300), 4);
        assert_eq!(found, vec![Ipv4Addr::LOCALHOST]);

        // A port with nothing bound: bind then drop to get a free one.
        let tmp = TcpListener::bind("127.0.0.1:0").unwrap();
        let closed = tmp.local_addr().unwrap().port();
        drop(tmp);
        let none = scan_port(&ips, closed, Duration::from_millis(200), 4);
        assert!(
            none.is_empty(),
            "closed port should not be reported: {none:?}"
        );
    }
}
