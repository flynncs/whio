use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderValue;

pub struct ClientIpPolicy {
    pub trusted_proxies: Vec<IpAddr>,
}

pub fn client_ip(
    peer: SocketAddr,
    forwarded_for: Option<&HeaderValue>,
    policy: &ClientIpPolicy,
) -> IpAddr {
    if !policy.trusted_proxies.contains(&peer.ip()) {
        return peer.ip();
    }
    let Some(header) = forwarded_for.and_then(|value| value.to_str().ok()) else {
        return peer.ip();
    };
    let entries = header
        .split(',')
        .map(|entry| entry.trim().parse::<IpAddr>())
        .collect::<Result<Vec<_>, _>>();
    let Ok(entries) = entries else {
        return peer.ip();
    };
    entries
        .iter()
        .rev()
        .find(|address| !policy.trusted_proxies.contains(address))
        .copied()
        .or_else(|| entries.first().copied())
        .unwrap_or_else(|| peer.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer() -> SocketAddr {
        "10.0.0.1:3000".parse().unwrap()
    }

    fn policy() -> ClientIpPolicy {
        ClientIpPolicy {
            trusted_proxies: vec!["10.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap()],
        }
    }

    #[test]
    fn untrusted_peer_ignores_forwarded_header() {
        let policy = ClientIpPolicy {
            trusted_proxies: vec![],
        };
        let header = HeaderValue::from_static("192.0.2.9");
        assert_eq!(client_ip(peer(), Some(&header), &policy), peer().ip());
    }

    #[test]
    fn trusted_peer_takes_rightmost_untrusted_entry() {
        let header = HeaderValue::from_static("198.51.100.1, 10.0.0.2, 192.0.2.7");
        assert_eq!(
            client_ip(peer(), Some(&header), &policy()),
            "192.0.2.7".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn all_trusted_chain_takes_leftmost_entry() {
        let header = HeaderValue::from_static("10.0.0.2, 10.0.0.1");
        assert_eq!(
            client_ip(peer(), Some(&header), &policy()),
            "10.0.0.2".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn malformed_forwarded_header_uses_peer() {
        let header = HeaderValue::from_static("198.51.100.1, invalid");
        assert_eq!(client_ip(peer(), Some(&header), &policy()), peer().ip());
    }
}
