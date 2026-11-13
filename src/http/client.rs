//! Where a request really came from. Behind a reverse proxy every connection
//! arrives from the proxy, so the client address has to come from a header,
//! and that header is believed only when the connection itself comes from a
//! trusted proxy.

use std::net::{IpAddr, SocketAddr};

use axum::http::{HeaderMap, HeaderName};
use ipnet::IpNet;

/// Resolves the client address of a request.
#[derive(Debug, Clone)]
pub struct ClientAddr {
    trusted: Vec<IpNet>,
    header: HeaderName,
}

impl ClientAddr {
    pub fn new(trusted: Vec<IpNet>, header: HeaderName) -> Self {
        Self { trusted, header }
    }

    fn is_trusted(&self, ip: IpAddr) -> bool {
        self.trusted.iter().any(|net| net.contains(&ip))
    }

    /// The client address: the peer itself, unless the peer is a trusted
    /// proxy. Then `X-Forwarded-For` is walked from the right and the first
    /// hop that is not a trusted proxy wins; any other configured header
    /// (for example `CF-Connecting-IP`) is taken as one address.
    pub fn resolve(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        let peer_ip = peer.ip().to_canonical();
        if !self.is_trusted(peer_ip) {
            return peer_ip;
        }
        let mut values = headers
            .get_all(&self.header)
            .iter()
            .filter_map(|v| v.to_str().ok());
        if self.header == "x-forwarded-for" {
            let hops: Vec<IpAddr> = values
                .flat_map(|v| v.split(','))
                .filter_map(|hop| hop.trim().parse::<IpAddr>().ok())
                .map(|ip| ip.to_canonical())
                .collect();
            hops.iter()
                .rev()
                .find(|ip| !self.is_trusted(**ip))
                .or(hops.first())
                .copied()
                .unwrap_or(peer_ip)
        } else {
            values
                .next_back()
                .and_then(|v| v.trim().parse::<IpAddr>().ok())
                .map_or(peer_ip, |ip| ip.to_canonical())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(trusted: &[&str], header: &'static str) -> ClientAddr {
        ClientAddr::new(
            trusted.iter().map(|n| n.parse().unwrap()).collect(),
            HeaderName::from_static(header),
        )
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.append(*k, v.parse().unwrap());
        }
        map
    }

    fn peer(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 4000)
    }

    #[test]
    fn an_untrusted_peer_cannot_claim_another_address() {
        let a = addr(&["127.0.0.0/8", "172.16.0.0/12"], "x-forwarded-for");
        let h = headers(&[("x-forwarded-for", "203.0.113.7")]);
        assert_eq!(
            a.resolve(peer("198.51.100.2"), &h).to_string(),
            "198.51.100.2"
        );
    }

    #[test]
    fn a_docker_gateway_proxy_is_believed() {
        let a = addr(&["127.0.0.0/8", "172.16.0.0/12"], "x-forwarded-for");
        let h = headers(&[("x-forwarded-for", "203.0.113.7")]);
        assert_eq!(a.resolve(peer("172.25.0.1"), &h).to_string(), "203.0.113.7");
        assert_eq!(a.resolve(peer("127.0.0.1"), &h).to_string(), "203.0.113.7");
    }

    #[test]
    fn forwarded_for_skips_trusted_hops_from_the_right() {
        let a = addr(&["172.16.0.0/12", "10.0.0.0/8"], "x-forwarded-for");
        // The client forged the first entry; the proxies appended the rest.
        let h = headers(&[("x-forwarded-for", "1.1.1.1, 203.0.113.7, 10.0.0.5")]);
        assert_eq!(a.resolve(peer("172.25.0.1"), &h).to_string(), "203.0.113.7");
    }

    #[test]
    fn a_custom_header_is_taken_from_a_trusted_proxy_only() {
        let a = addr(&["172.16.0.0/12"], "cf-connecting-ip");
        let h = headers(&[
            ("cf-connecting-ip", "2001:db8::7"),
            ("x-forwarded-for", "9.9.9.9"),
        ]);
        assert_eq!(a.resolve(peer("172.25.0.1"), &h).to_string(), "2001:db8::7");
        assert_eq!(
            a.resolve(peer("198.51.100.2"), &h).to_string(),
            "198.51.100.2"
        );
    }

    #[test]
    fn garbage_falls_back_to_the_peer() {
        let a = addr(&["172.16.0.0/12"], "x-forwarded-for");
        let h = headers(&[("x-forwarded-for", "not-an-ip")]);
        assert_eq!(a.resolve(peer("172.25.0.1"), &h).to_string(), "172.25.0.1");
        assert_eq!(
            a.resolve(peer("172.25.0.1"), &HeaderMap::new()).to_string(),
            "172.25.0.1"
        );
    }

    #[test]
    fn mapped_ipv4_is_canonical() {
        let a = addr(&["127.0.0.0/8"], "x-forwarded-for");
        let h = HeaderMap::new();
        assert_eq!(
            a.resolve(peer("::ffff:198.51.100.2"), &h).to_string(),
            "198.51.100.2"
        );
    }
}
