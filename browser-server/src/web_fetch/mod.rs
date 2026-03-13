use std::net::{IpAddr, Ipv6Addr, ToSocketAddrs};

use anyhow::{anyhow, bail, Result};
use url::Url;

use crate::browser;

const READABILITY_JS: &str = include_str!("vendor/readability-0.6.0.js");
const CLEAN_HTML_JS: &str = include_str!("clean-html.js");
const EXTRACT_CONTENT_JS: &str = include_str!("extract-content.js");

/// Returns `true` if the IP address is private, loopback, link-local, or otherwise
/// not suitable for public access (SSRF protection).
fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()              // 127.0.0.0/8
            || v4.is_private()            // 10/8, 172.16/12, 192.168/16
            || v4.is_link_local()         // 169.254/16
            || v4.is_unspecified()        // 0.0.0.0
            || v4.is_broadcast()          // 255.255.255.255
            || matches!(v4.octets(), [100, b, ..] if (64..=127).contains(&b)) // CGNAT 100.64/10
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()              // ::1
            || v6.is_unspecified()        // ::
            || is_ipv4_mapped_private(v6)
        }
    }
}

/// Check if an IPv6 address is an IPv4-mapped address (::ffff:x.x.x.x) that maps to a private IPv4.
fn is_ipv4_mapped_private(v6: Ipv6Addr) -> bool {
    if let Some(v4) = v6.to_ipv4_mapped() {
        is_private_ip(IpAddr::V4(v4))
    } else {
        false
    }
}

/// Validate that a URL is safe to fetch (SSRF protection).
///
/// Rejects non-HTTP(S) schemes, localhost, and private/internal IP addresses.
pub fn validate_url(url: &str) -> Result<()> {
    let parsed = Url::parse(url).map_err(|e| anyhow!("Invalid URL: {e}"))?;

    // Scheme check: only http and https
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => bail!("Blocked URL scheme: {scheme}"),
    }

    // Host check: must have a host, reject localhost
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("URL has no host"))?;

    if host == "localhost" || host.ends_with(".localhost") {
        bail!("Blocked host: {host}");
    }

    // If host is an IP literal, check it directly
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(ip) {
            bail!("Blocked private/internal IP: {ip}");
        }
        return Ok(());
    }

    // Also handle bracket-wrapped IPv6 like [::1]
    let trimmed = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        if is_private_ip(ip) {
            bail!("Blocked private/internal IP: {ip}");
        }
        return Ok(());
    }

    // DNS resolution check: resolve hostname and check all IPs
    let port = parsed.port().unwrap_or(match parsed.scheme() {
        "https" => 443,
        _ => 80,
    });
    let addr = format!("{host}:{port}");

    match addr.to_socket_addrs() {
        Ok(addrs) => {
            let resolved: Vec<_> = addrs.collect();
            if resolved.is_empty() {
                bail!("DNS resolution returned no addresses for {host}");
            }
            for sock_addr in &resolved {
                if is_private_ip(sock_addr.ip()) {
                    bail!(
                        "Blocked: {host} resolves to private/internal IP {}",
                        sock_addr.ip()
                    );
                }
            }
        }
        Err(e) => {
            bail!("DNS resolution failed for {host}: {e}");
        }
    }

    Ok(())
}

/// Fetch a web page using headless Chrome and extract clean HTML using Readability.js.
/// Retries once if the browser connection is lost.
pub fn fetch_and_extract(url: &str) -> Result<String> {
    validate_url(url)?;

    match fetch_and_extract_inner(url) {
        Ok(result) => Ok(result),
        Err(e) if e.to_string().contains("connection is closed") => {
            tracing::warn!("Browser connection lost during web_fetch, restarting: {e}");
            browser::shutdown_browser();
            fetch_and_extract_inner(url)
        }
        Err(e) => Err(e),
    }
}

fn fetch_and_extract_inner(url: &str) -> Result<String> {
    let tab = browser::open_tab(url)?;
    tab.evaluate(READABILITY_JS, false)?;
    tab.evaluate(CLEAN_HTML_JS, false)?;
    let result = tab.evaluate(EXTRACT_CONTENT_JS, false)?;

    match result.value {
        Some(serde_json::Value::String(content)) => Ok(content),
        Some(serde_json::Value::Null) | None => {
            Err(anyhow!("Readability could not extract content from this page"))
        }
        Some(other) => Err(anyhow!("Unexpected result type: {:?}", other)),
    }
}
