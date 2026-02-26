use axum::{
    extract::{ConnectInfo, Extension, Request},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use ipnet::IpNet;
use std::net::{IpAddr, SocketAddr};

#[derive(Debug, Clone)]
pub struct ClientIp(pub IpAddr);

pub struct ClientIpExtractor;

impl ClientIpExtractor {
    pub async fn middleware(
        ConnectInfo(addr): ConnectInfo<SocketAddr>,
        Extension(trusted_proxies): Extension<Vec<String>>,
        mut request: Request,
        next: Next,
    ) -> Response {
        let client_ip = Self::extract_client_ip(request.headers(), addr.ip(), &trusted_proxies);
        log::debug!(
            "extracted client IP: {} (connection IP: {})",
            client_ip,
            addr.ip()
        );
        request.extensions_mut().insert(ClientIp(client_ip));
        next.run(request).await
    }

    fn extract_client_ip(
        headers: &HeaderMap,
        connection_ip: IpAddr,
        trusted_proxies: &[String],
    ) -> IpAddr {
        let should_trust_headers = if trusted_proxies.is_empty() {
            log::debug!("trusted-proxies is empty, ignoring proxy headers for security");
            false
        } else {
            Self::is_trusted_proxy(&connection_ip, trusted_proxies)
        };

        if !should_trust_headers {
            log::debug!(
                "connection IP {} is not a trusted proxy, using connection IP",
                connection_ip
            );
            return connection_ip;
        }

        if let Some(forwarded_for) = headers.get("x-forwarded-for")
            && let Ok(header_value) = forwarded_for.to_str()
        {
            log::debug!("found X-Forwarded-For header: {}", header_value);
            if let Some(first_ip) = Self::parse_forwarded_for(header_value) {
                log::debug!("using IP from X-Forwarded-For: {}", first_ip);
                return first_ip;
            } else {
                log::debug!(
                    "failed to parse valid IP from X-Forwarded-For header, trying X-Real-IP"
                );
            }
        }

        if let Some(real_ip) = headers.get("x-real-ip")
            && let Ok(header_value) = real_ip.to_str()
        {
            log::debug!("found X-Real-IP header: {}", header_value);
            if let Ok(ip) = header_value.trim().parse::<IpAddr>() {
                log::debug!("using IP from X-Real-IP: {}", ip);
                return ip;
            } else {
                log::debug!("failed to parse IP from X-Real-IP header, using connection IP");
            }
        }

        log::trace!(
            "no valid proxy headers found, using connection IP: {}",
            connection_ip
        );
        connection_ip
    }

    fn is_trusted_proxy(connection_ip: &IpAddr, trusted_proxies: &[String]) -> bool {
        for trusted in trusted_proxies {
            if Self::matches_ip_or_cidr(connection_ip, trusted) {
                log::debug!(
                    "connection IP {} matched trusted proxy {}",
                    connection_ip,
                    trusted
                );
                return true;
            }
        }
        log::debug!(
            "connection IP {} not in trusted proxies list",
            connection_ip
        );
        false
    }

    fn matches_ip_or_cidr(ip: &IpAddr, pattern: &str) -> bool {
        if let Ok(pattern_ip) = pattern.parse::<IpAddr>() {
            return ip == &pattern_ip;
        }

        if let Ok(network) = pattern.parse::<IpNet>() {
            return network.contains(ip);
        }

        false
    }

    fn parse_forwarded_for(header_value: &str) -> Option<IpAddr> {
        header_value.split(',').next().and_then(|ip_str| {
            let trimmed = ip_str.trim();
            if Self::is_valid_ip_format(trimmed) {
                trimmed.parse::<IpAddr>().ok()
            } else {
                None
            }
        })
    }

    fn is_valid_ip_format(ip_str: &str) -> bool {
        if ip_str.is_empty() || ip_str.len() > 45 {
            return false;
        }

        ip_str.parse::<IpAddr>().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn test_extract_client_ip_from_connection_no_trusted_proxies() {
        let headers = HeaderMap::new();
        let connection_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));

        let result = ClientIpExtractor::extract_client_ip(&headers, connection_ip, &[]);
        assert_eq!(result, connection_ip);
    }

    #[test]
    fn test_extract_client_ip_from_x_forwarded_for_trusted_proxy() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "203.0.113.195, 198.51.100.178".parse().unwrap(),
        );
        let connection_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

        let result = ClientIpExtractor::extract_client_ip(
            &headers,
            connection_ip,
            &["10.0.0.1".to_string()],
        );
        assert_eq!(result, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 195)));
    }

    #[test]
    fn test_extract_client_ip_from_x_real_ip_trusted_proxy() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "203.0.113.195".parse().unwrap());
        let connection_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

        let result = ClientIpExtractor::extract_client_ip(
            &headers,
            connection_ip,
            &["10.0.0.1".to_string()],
        );
        assert_eq!(result, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 195)));
    }

    #[test]
    fn test_x_forwarded_for_takes_precedence_over_x_real_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.195".parse().unwrap());
        headers.insert("x-real-ip", "198.51.100.178".parse().unwrap());
        let connection_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

        let result = ClientIpExtractor::extract_client_ip(
            &headers,
            connection_ip,
            &["10.0.0.1".to_string()],
        );
        assert_eq!(result, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 195)));
    }

    #[test]
    fn test_invalid_x_forwarded_for_falls_back_to_x_real_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "invalid-ip".parse().unwrap());
        headers.insert("x-real-ip", "203.0.113.195".parse().unwrap());
        let connection_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

        let result = ClientIpExtractor::extract_client_ip(
            &headers,
            connection_ip,
            &["10.0.0.1".to_string()],
        );
        assert_eq!(result, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 195)));
    }

    #[test]
    fn test_invalid_headers_fall_back_to_connection_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "invalid-ip".parse().unwrap());
        headers.insert("x-real-ip", "also-invalid".parse().unwrap());
        let connection_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

        let result = ClientIpExtractor::extract_client_ip(
            &headers,
            connection_ip,
            &["10.0.0.1".to_string()],
        );
        assert_eq!(result, connection_ip);
    }

    #[test]
    fn test_untrusted_proxy_ignores_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.195".parse().unwrap());
        let connection_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));

        let result = ClientIpExtractor::extract_client_ip(
            &headers,
            connection_ip,
            &["10.0.0.1".to_string()],
        );
        assert_eq!(result, connection_ip);
    }

    #[test]
    fn test_empty_trusted_proxies_ignores_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.195".parse().unwrap());
        let connection_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));

        let result = ClientIpExtractor::extract_client_ip(&headers, connection_ip, &[]);
        assert_eq!(result, connection_ip);
    }

    #[test]
    fn test_trusted_proxy_cidr_match() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.195".parse().unwrap());
        let connection_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 50));

        let result = ClientIpExtractor::extract_client_ip(
            &headers,
            connection_ip,
            &["10.0.0.0/8".to_string()],
        );
        assert_eq!(result, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 195)));
    }

    #[test]
    fn test_ipv6_addresses() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "2001:db8::1".parse().unwrap());
        let connection_ip = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x2));

        let result = ClientIpExtractor::extract_client_ip(
            &headers,
            connection_ip,
            &["2001:db8::2".to_string()],
        );
        assert_eq!(
            result,
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x1))
        );
    }

    #[test]
    fn test_parse_forwarded_for_multiple_ips() {
        let result =
            ClientIpExtractor::parse_forwarded_for("203.0.113.195, 198.51.100.178, 192.168.1.1");
        assert_eq!(result, Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 195))));
    }

    #[test]
    fn test_parse_forwarded_for_single_ip() {
        let result = ClientIpExtractor::parse_forwarded_for("203.0.113.195");
        assert_eq!(result, Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 195))));
    }

    #[test]
    fn test_parse_forwarded_for_with_whitespace() {
        let result = ClientIpExtractor::parse_forwarded_for("  203.0.113.195  , 198.51.100.178");
        assert_eq!(result, Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 195))));
    }

    #[test]
    fn test_parse_forwarded_for_invalid_ip() {
        let result = ClientIpExtractor::parse_forwarded_for("invalid-ip, 198.51.100.178");
        assert_eq!(result, None);
    }

    #[test]
    fn test_is_valid_ip_format() {
        assert!(ClientIpExtractor::is_valid_ip_format("192.168.1.1"));
        assert!(ClientIpExtractor::is_valid_ip_format("2001:db8::1"));
        assert!(ClientIpExtractor::is_valid_ip_format("::1"));
        assert!(ClientIpExtractor::is_valid_ip_format("127.0.0.1"));

        assert!(!ClientIpExtractor::is_valid_ip_format(""));
        assert!(!ClientIpExtractor::is_valid_ip_format("not-an-ip"));
        assert!(!ClientIpExtractor::is_valid_ip_format("999.999.999.999"));
        assert!(!ClientIpExtractor::is_valid_ip_format("192.168.1"));
        assert!(!ClientIpExtractor::is_valid_ip_format(&"a".repeat(46)));
    }

    #[test]
    fn test_spoofing_attack_prevention() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "192.168.1.100".parse().unwrap());
        let attacker_ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 50));

        // Only 10.0.0.1 is trusted, attacker comes from different IP
        let result =
            ClientIpExtractor::extract_client_ip(&headers, attacker_ip, &["10.0.0.1".to_string()]);
        assert_eq!(result, attacker_ip);
        assert_ne!(result, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));
    }

    #[test]
    fn test_matches_ip_or_cidr() {
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert!(ClientIpExtractor::matches_ip_or_cidr(&ip, "192.168.1.100"));
        assert!(!ClientIpExtractor::matches_ip_or_cidr(&ip, "192.168.1.101"));
        assert!(ClientIpExtractor::matches_ip_or_cidr(&ip, "192.168.1.0/24"));
        assert!(!ClientIpExtractor::matches_ip_or_cidr(&ip, "10.0.0.0/8"));
        assert!(!ClientIpExtractor::matches_ip_or_cidr(&ip, "invalid"));
        assert!(!ClientIpExtractor::matches_ip_or_cidr(&ip, ""));
    }
}
