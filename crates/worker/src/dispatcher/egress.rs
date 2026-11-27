//! Keeps HTTP dispatch away from cloud instance-metadata services.
//!
//! Endpoint URLs are caller-controlled, and the worker runs inside the
//! deployment's network, so `http://169.254.169.254/...` would hand the
//! caller the node's cloud credentials (AWS, GCP, Azure and OCI all serve
//! metadata there). Link-local addresses are never a legitimate job target, so
//! they are refused outright. Private ranges and loopback are *not* blocked:
//! calling internal services is what Invokr is for.
//!
//! Enforced at three points, since any one alone is bypassable:
//! - [`check_url`] before sending, for IP-literal hosts (no DNS lookup happens);
//! - [`client`]'s resolver, for hostnames that resolve to a blocked address;
//! - [`client`]'s redirect policy, for a redirect to an IP-literal host.

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{redirect, Client, Url};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

/// AWS's IPv6 instance-metadata endpoint. Unique-local, not link-local, so it
/// needs naming explicitly.
const AWS_IMDS_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);

/// Same limit as reqwest's default redirect policy.
const MAX_REDIRECTS: usize = 10;

pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.is_link_local(),
            None => v6.is_unicast_link_local() || v6 == AWS_IMDS_V6,
        },
    }
}

/// `Err` with a reason when `url`'s host is an IP literal in a blocked range.
/// Hostnames pass here; [`client`]'s resolver vets what they resolve to.
pub fn check_url(url: &Url) -> Result<(), String> {
    // `Url` has already normalised IP literals (e.g. `0xA9FEA9FE` becomes
    // `169.254.169.254`); IPv6 ones keep their brackets.
    let host = url.host_str().unwrap_or_default();
    let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    else {
        return Ok(());
    };
    if is_blocked_ip(ip) {
        return Err(format!(
            "destination {ip} is a link-local / instance-metadata address and is not allowed"
        ));
    }
    Ok(())
}

/// The HTTP client used for dispatch: reqwest's defaults plus the egress
/// checks in the resolver and redirect policy.
pub fn client() -> Client {
    Client::builder()
        .dns_resolver(Arc::new(EgressResolver))
        .redirect(redirect::Policy::custom(|attempt| {
            if let Err(reason) = check_url(attempt.url()) {
                attempt.error(reason)
            } else if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else {
                attempt.follow()
            }
        }))
        .build()
        // Same failure mode as `Client::new()`, which also panics here.
        .expect("failed to build the dispatch HTTP client")
}

/// System resolution (getaddrinfo, as reqwest's default resolver uses) with
/// blocked addresses filtered out.
struct EgressResolver;

impl Resolve for EgressResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let resolved = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let allowed: Vec<SocketAddr> = resolved.filter(|a| !is_blocked_ip(a.ip())).collect();
            if allowed.is_empty() {
                return Err(format!(
                    "{host} resolves only to link-local / instance-metadata addresses, \
                     which are not allowed"
                )
                .into());
            }
            Ok(Box::new(allowed.into_iter()) as Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn blocks_metadata_and_link_local_addresses() {
        for blocked in [
            "169.254.169.254",
            "169.254.170.2", // ECS task metadata
            "169.254.0.1",
            "::ffff:169.254.169.254",
            "fe80::1",
            "fd00:ec2::254",
        ] {
            assert!(is_blocked_ip(ip(blocked)), "{blocked} should be blocked");
        }
    }

    #[test]
    fn allows_internal_and_public_addresses() {
        for allowed in [
            "127.0.0.1",
            "10.0.0.5",
            "172.16.0.1",
            "192.168.1.1",
            "8.8.8.8",
            "::1",
            "fd00::1",
            "2001:4860:4860::8888",
        ] {
            assert!(!is_blocked_ip(ip(allowed)), "{allowed} should be allowed");
        }
    }

    #[test]
    fn check_url_refuses_ip_literal_metadata_hosts_in_any_spelling() {
        for url in [
            "http://169.254.169.254/latest/meta-data/",
            "http://[fd00:ec2::254]/latest/meta-data/",
            "http://[::ffff:a9fe:a9fe]/",
            // WHATWG parsing turns these into 169.254.169.254 too.
            "http://2852039166/",
            "http://0xA9FEA9FE/",
        ] {
            let parsed = Url::parse(url).unwrap();
            assert!(check_url(&parsed).is_err(), "{url} should be refused");
        }
    }

    #[test]
    fn check_url_allows_hostnames_and_ordinary_ips() {
        for url in [
            "http://localhost:9999/success",
            "http://10.1.2.3/hook",
            "https://example.com/x",
        ] {
            assert!(check_url(&Url::parse(url).unwrap()).is_ok(), "{url}");
        }
    }

    #[tokio::test]
    async fn resolver_keeps_ordinary_hosts() {
        let addrs: Vec<SocketAddr> = EgressResolver
            .resolve("localhost".parse().unwrap())
            .await
            .unwrap()
            .collect();
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(|a| a.ip().is_loopback()));
    }

    /// A local server that answers every request with a redirect to the
    /// metadata service.
    fn redirect_to_metadata_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf);
                let _ = conn.write_all(
                    b"HTTP/1.1 302 Found\r\n\
                      Location: http://169.254.169.254/latest/meta-data/\r\n\
                      Content-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        port
    }

    #[tokio::test]
    async fn client_refuses_a_redirect_to_the_metadata_service() {
        let port = redirect_to_metadata_server();
        let err = client()
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap_err();
        assert!(err.is_redirect(), "expected a redirect error, got: {err}");
    }
}
