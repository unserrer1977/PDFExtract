//! Fetch a caller-supplied PDF without following it onto a private network.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use url::Url;

use crate::logic::{looks_like_pdf, MAX_PDF_BYTES};

const MAX_REDIRECTS: usize = 5;

pub fn screen_url(raw: &str) -> Result<Url, String> {
    if raw.len() > 2048 {
        return Err("URL is too long".into());
    }
    let url = Url::parse(raw).map_err(|_| "URL is not valid".to_string())?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("Only http and https URLs are accepted".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs with a username or password are not accepted".into());
    }
    ensure_public(&url)?;
    Ok(url)
}

pub async fn fetch_pdf(raw: &str) -> Result<Vec<u8>, String> {
    let url = screen_url(raw)?;
    let client = client_for(&url)?;
    let response = client.get(url).send().await.map_err(|err| format!("Could not fetch the PDF: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("Fetching the URL returned HTTP {}", response.status()));
    }
    let mut response = response;
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len().saturating_add(chunk.len()) > MAX_PDF_BYTES {
                    return Err(format!("PDF is larger than {} MB", MAX_PDF_BYTES / (1024 * 1024)));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(err) => return Err(format!("Could not fetch the PDF: {err}")),
        }
    }
    if !looks_like_pdf(&body) {
        return Err("The URL did not return a PDF".into());
    }
    Ok(body)
}

fn client_for(url: &Url) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .user_agent("pdfextract/0.1")
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error(std::io::Error::other("too many redirects"));
            }
            match ensure_public(attempt.url()) {
                Ok(()) => attempt.follow(),
                Err(message) => attempt.error(std::io::Error::other(message)),
            }
        }));
    if let Some((host, address)) = pinned_address(url)? {
        builder = builder.resolve(&host, address);
    }
    builder.build().map_err(|err| format!("Could not prepare the fetch: {err}"))
}

fn pinned_address(url: &Url) -> Result<Option<(String, SocketAddr)>, String> {
    let host = url.host_str().ok_or_else(|| "URL has no host".to_string())?;
    if url.host().map(|item| matches!(item, url::Host::Ipv4(_) | url::Host::Ipv6(_))).unwrap_or(true) {
        return Ok(None);
    }
    let port = url.port_or_known_default().ok_or_else(|| "URL has no port".to_string())?;
    let address = (host, port)
        .to_socket_addrs()
        .map_err(|err| format!("Could not resolve {host}: {err}"))?
        .find(|addr| ip_allowed(addr.ip()));
    let Some(address) = address else {
        return Err(format!("{host} does not resolve to a public address"));
    };
    Ok(Some((host.to_string(), address)))
}

fn ensure_public(url: &Url) -> Result<(), String> {
    let host = url.host().ok_or_else(|| "URL has no host".to_string())?;
    match host {
        url::Host::Ipv4(ip) => check_ip(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => check_ip(IpAddr::V6(ip)),
        url::Host::Domain(name) => {
            if blocked_name(name) {
                return Err("That host is not a public address".into());
            }
            let port = url.port_or_known_default().ok_or_else(|| "URL has no port".to_string())?;
            let mut saw = false;
            for address in (name, port).to_socket_addrs().map_err(|err| format!("Could not resolve {name}: {err}"))? {
                saw = true;
                check_ip(address.ip())?;
            }
            if !saw {
                return Err(format!("Could not resolve {name}"));
            }
            Ok(())
        }
    }
}

fn blocked_name(name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    name == "localhost"
        || name.ends_with(".localhost")
        || name == "metadata"
        || name == "metadata.google.internal"
        || name.ends_with(".local")
        || name.ends_with(".internal")
}

fn check_ip(ip: IpAddr) -> Result<(), String> {
    if ip_allowed(ip) {
        Ok(())
    } else {
        Err("That host is not a public address".into())
    }
}

fn ip_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ipv4_allowed(ip),
        IpAddr::V6(ip) => ipv6_allowed(ip),
    }
}

fn ipv4_allowed(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    if ip.is_unspecified() || ip.is_loopback() || ip.is_link_local() || ip.is_broadcast() || ip.is_multicast() {
        return false;
    }
    if a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168) {
        return false;
    }
    if a == 100 && (64..=127).contains(&b) {
        return false;
    }
    if a == 192 && b == 0 && (c == 0 || c == 2) {
        return false;
    }
    if a == 198 && (b == 18 || b == 19) {
        return false;
    }
    if a == 198 && b == 51 && c == 100 {
        return false;
    }
    if a == 203 && b == 0 && c == 113 {
        return false;
    }
    if a >= 240 {
        return false;
    }
    true
}

fn ipv6_allowed(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return ipv4_allowed(v4);
    }
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return false;
    }
    let [first, ..] = ip.segments();
    if (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80 {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_local_targets_are_rejected() {
        for raw in [
            "http://127.0.0.1/a.pdf",
            "http://10.1.2.3/a.pdf",
            "http://192.168.0.8/a.pdf",
            "http://172.16.5.5/a.pdf",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/a.pdf",
            "http://localhost/a.pdf",
            "http://metadata.google.internal/a.pdf",
            "file:///etc/passwd",
            "http://user:secret@8.8.8.8/a.pdf",
            "ftp://8.8.8.8/a.pdf",
        ] {
            assert!(screen_url(raw).is_err(), "{raw} should be rejected");
        }
    }

    #[test]
    fn a_public_address_is_accepted() {
        let url = screen_url("https://8.8.8.8/report.pdf").expect("public address");
        assert_eq!(url.host_str(), Some("8.8.8.8"));
    }
}
