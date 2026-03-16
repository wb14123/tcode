use std::collections::HashMap;
use std::fmt::Write;
use std::net::{IpAddr, Ipv6Addr, ToSocketAddrs};

use anyhow::{anyhow, bail, Result};
use headless_chrome::protocol::cdp::Accessibility;
use url::Url;

use crate::browser;

/// Roles that add no semantic value — skip them and emit their children directly.
const SKIP_ROLES: &[&str] = &[
    "generic",
    "none",
    "presentation",
    "InlineTextBox",
    "LineBreak",
];

// ---------------------------------------------------------------------------
// Lenient CDP Accessibility types
//
// Chrome's AX tree can return property names (like "uninteresting") that the
// headless_chrome crate's strict AXPropertyName enum doesn't know about,
// causing deserialization failures. We define our own types that use String
// where the crate uses strict enums.
// ---------------------------------------------------------------------------

/// Our own GetFullAXTree command with a lenient return type.
#[derive(Debug, serde::Serialize)]
pub struct GetFullAXTree {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "frameId")]
    pub frame_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct GetFullAXTreeResponse {
    pub nodes: Vec<AXNode>,
}

impl headless_chrome::protocol::cdp::types::Method for GetFullAXTree {
    const NAME: &'static str = "Accessibility.getFullAXTree";
    type ReturnObject = GetFullAXTreeResponse;
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AXNode {
    #[serde(rename = "nodeId")]
    pub node_id: String,
    #[serde(default)]
    pub ignored: bool,
    pub role: Option<AXValue>,
    pub name: Option<AXValue>,
    pub description: Option<AXValue>,
    pub value: Option<AXValue>,
    pub properties: Option<Vec<AXProperty>>,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    #[serde(rename = "childIds")]
    pub child_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AXValue {
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AXProperty {
    /// String instead of a strict enum — tolerates unknown property names from Chrome.
    pub name: String,
    pub value: AXValue,
}

// ---------------------------------------------------------------------------

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

/// Fetch a web page using headless Chrome and extract content via the accessibility tree.
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
    tracing::info!("open_tab: navigating to {url}");
    let tab = browser::open_tab(url)?;
    tracing::info!("open_tab: done");

    // Extract content via Chrome's accessibility tree
    tracing::info!("enabling Accessibility domain");
    tab.call_method(Accessibility::Enable(None))?;
    tracing::info!("calling GetFullAXTree");
    let tree = tab.call_method(GetFullAXTree {
        depth: None,
        frame_id: None,
    })?;
    tracing::info!("got AX tree with {} nodes", tree.nodes.len());

    let content = format_ax_tree(&tree.nodes);
    if content.trim().is_empty() {
        bail!("Could not extract meaningful content from this page");
    }
    Ok(content)
}

/// Extract the string value from an `AXValue`, returning an empty string if absent.
fn ax_value_str(val: &Option<AXValue>) -> &str {
    match val {
        Some(v) => match &v.value {
            Some(serde_json::Value::String(s)) => s.as_str(),
            _ => "",
        },
        None => "",
    }
}

/// Get a named property's string value from an AX node's properties list.
fn ax_prop_str<'a>(props: &'a Option<Vec<AXProperty>>, name: &str) -> Option<&'a str> {
    props.as_ref()?.iter().find(|p| p.name == name).and_then(|p| {
        match &p.value.value {
            Some(serde_json::Value::String(s)) => Some(s.as_str()),
            Some(serde_json::Value::Number(n)) => {
                // Numbers don't have a string repr we can return as &str,
                // so we skip them here — callers use ax_prop_number instead
                let _ = n;
                None
            }
            _ => None,
        }
    })
}

/// Get a named property's numeric value.
fn ax_prop_number(props: &Option<Vec<AXProperty>>, name: &str) -> Option<i64> {
    props.as_ref()?.iter().find(|p| p.name == name).and_then(|p| {
        match &p.value.value {
            Some(serde_json::Value::Number(n)) => n.as_i64(),
            _ => None,
        }
    })
}

/// Format a flat CDP accessibility tree into compact text for LLM consumption.
pub fn format_ax_tree(nodes: &[AXNode]) -> String {
    if nodes.is_empty() {
        return String::new();
    }

    // Build lookup: node_id -> &AXNode
    let node_map: HashMap<&str, &AXNode> =
        nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();

    // Find root nodes (no parent_id, or parent not in map)
    let roots: Vec<&AXNode> = nodes
        .iter()
        .filter(|n| {
            match &n.parent_id {
                None => true,
                Some(pid) => !node_map.contains_key(pid.as_str()),
            }
        })
        .collect();

    let mut out = String::new();
    for root in &roots {
        format_node(&mut out, root, &node_map, 0);
    }
    out
}

fn format_node(
    out: &mut String,
    node: &AXNode,
    node_map: &HashMap<&str, &AXNode>,
    depth: usize,
) {
    // Skip ignored nodes entirely
    if node.ignored {
        // Still recurse into children — some ignored containers have visible children
        emit_children(out, node, node_map, depth);
        return;
    }

    let role = ax_value_str(&node.role);
    let name = ax_value_str(&node.name);

    // Skip noise roles — just emit children at the same depth
    if SKIP_ROLES.contains(&role) {
        emit_children(out, node, node_map, depth);
        return;
    }

    // StaticText: emit the text content directly
    if role == "StaticText" {
        if !name.is_empty() {
            write_indent(out, depth);
            out.push_str(name);
            out.push('\n');
        }
        return;
    }

    // Skip nodes with no role and no name
    if role.is_empty() && name.is_empty() {
        emit_children(out, node, node_map, depth);
        return;
    }

    // Build the line: role "name" [properties]
    let mut line = String::new();

    if !role.is_empty() {
        line.push_str(role);
    }

    if !name.is_empty() {
        if !line.is_empty() {
            line.push(' ');
        }
        write!(line, "\"{}\"", name).ok();
    }

    // Append useful properties
    if let Some(level) = ax_prop_number(&node.properties, "level") {
        write!(line, " level: {level}").ok();
    }
    if let Some(url) = ax_prop_str(&node.properties, "url") {
        write!(line, " url: {url}").ok();
    }
    if let Some(checked) = ax_prop_str(&node.properties, "checked") {
        write!(line, " checked: {checked}").ok();
    }
    if let Some(expanded) = ax_prop_str(&node.properties, "expanded") {
        write!(line, " expanded: {expanded}").ok();
    }
    if let Some(selected) = ax_prop_str(&node.properties, "selected") {
        write!(line, " selected: {selected}").ok();
    }

    // Append value if present (e.g. for inputs)
    let value_str = ax_value_str(&node.value);
    if !value_str.is_empty() {
        write!(line, " value: {value_str}").ok();
    }

    if !line.is_empty() {
        write_indent(out, depth);
        out.push_str(&line);
        out.push('\n');
    }

    // Recurse into children at depth+1
    emit_children(out, node, node_map, depth + 1);
}

fn emit_children(
    out: &mut String,
    node: &AXNode,
    node_map: &HashMap<&str, &AXNode>,
    child_depth: usize,
) {
    if let Some(child_ids) = &node.child_ids {
        for cid in child_ids {
            if let Some(child) = node_map.get(cid.as_str()) {
                format_node(out, child, node_map, child_depth);
            }
        }
    }
}

fn write_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}
