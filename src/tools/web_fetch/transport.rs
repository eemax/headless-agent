use std::{
    collections::VecDeque,
    error::Error as _,
    io::Read,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use ureq::OrAnyStatus;
use url::{Host, Url};

use super::{
    CONNECT_TIMEOUT, FetchResult, MAX_ADDRESS_ATTEMPTS, MAX_DOWNLOAD_BYTES, MAX_REDIRECTS, Warning,
    content::{extract_content, normalize_content_type, push_warning},
};

#[derive(Debug, Clone)]
pub(super) struct ResolvedTarget {
    pub(super) url: Url,
    addresses: Vec<SocketAddr>,
}

#[derive(Debug, Clone)]
pub(super) struct TransportResponse {
    pub(super) url: Url,
    pub(super) status: u16,
    pub(super) content_type: Option<String>,
    pub(super) location: Option<String>,
    pub(super) content_length: Option<u64>,
    pub(super) body: Vec<u8>,
    pub(super) bytes_read: u64,
    pub(super) body_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TransportFailureKind {
    Dns,
    Connect,
    Timeout,
    Redirect,
    Decode,
}

#[derive(Debug, Clone)]
pub(super) struct TransportFailure {
    pub(super) kind: TransportFailureKind,
    pub(super) url: Option<Url>,
}

pub(super) trait DnsResolver {
    fn resolve(&self, host: &str, port: u16, timeout: Duration)
    -> std::io::Result<Vec<SocketAddr>>;
}

pub(super) trait HttpTransport {
    fn get(
        &self,
        url: &Url,
        address: SocketAddr,
        timeout: Duration,
    ) -> Result<TransportResponse, TransportFailure>;
}

pub(super) struct StdDnsResolver;
pub(super) struct UreqTransport;

impl DnsResolver for StdDnsResolver {
    fn resolve(
        &self,
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> std::io::Result<Vec<SocketAddr>> {
        let host = host.to_string();
        let worker_host = host.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = (worker_host.as_str(), port)
                .to_socket_addrs()
                .map(|iter| iter.collect());
            let _ = sender.send(result);
        });

        match receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("dns lookup timed out for `{host}`"),
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("dns lookup worker disconnected for `{host}`"),
            )),
        }
    }
}

impl HttpTransport for UreqTransport {
    fn get(
        &self,
        url: &Url,
        address: SocketAddr,
        timeout: Duration,
    ) -> Result<TransportResponse, TransportFailure> {
        let connect_timeout = CONNECT_TIMEOUT.min(timeout);
        let agent = ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .redirects(0)
            .timeout_connect(connect_timeout)
            .timeout(timeout)
            .timeout_read(timeout)
            .timeout_write(timeout)
            .resolver(move |_netloc: &str| Ok(vec![address]))
            .build();
        let response = agent
            .get(url.as_str())
            .set(
                "User-Agent",
                concat!("headless/", env!("CARGO_PKG_VERSION")),
            )
            .set(
                "Accept",
                "text/html,application/xhtml+xml,application/json,text/plain;q=0.9,*/*;q=0.8",
            )
            .set("Accept-Language", "en-US,en;q=0.9")
            .timeout(timeout)
            .call()
            .or_any_status()
            .map_err(|error| map_transport_error(error, Some(url.clone())))?;

        let status = response.status();
        let response_url = response
            .get_url()
            .parse::<Url>()
            .map_err(|_| TransportFailure {
                kind: TransportFailureKind::Decode,
                url: Some(url.clone()),
            })?;
        let content_type = response.header("content-type").map(ToOwned::to_owned);
        let location = response.header("location").map(ToOwned::to_owned);
        let content_length = response
            .header("content-length")
            .and_then(|value: &str| value.parse::<u64>().ok());
        let mut reader = response.into_reader().take((MAX_DOWNLOAD_BYTES as u64) + 1);
        let mut body = Vec::new();
        reader
            .read_to_end(&mut body)
            .map_err(|error| map_read_error(error, Some(response_url.clone())))?;
        let body_truncated = body.len() > MAX_DOWNLOAD_BYTES;
        if body_truncated {
            body.truncate(MAX_DOWNLOAD_BYTES);
        }
        Ok(TransportResponse {
            url: response_url,
            status,
            content_type,
            location,
            content_length,
            bytes_read: body.len() as u64,
            body,
            body_truncated,
        })
    }
}

pub(super) fn fetch_with_clients(
    requested_url: &str,
    timeout: Duration,
    resolver: &dyn DnsResolver,
    transport: &dyn HttpTransport,
) -> FetchResult {
    let deadline = Instant::now() + timeout;
    let mut current = match resolve_target(requested_url, resolver, deadline) {
        Ok(target) => target,
        Err(result) => return result,
    };

    let mut redirects_followed = 0usize;
    loop {
        let response = match fetch_target(&current, deadline, transport) {
            Ok(response) => response,
            Err(error) => {
                let final_url = error
                    .url
                    .map(|value| value.to_string())
                    .or_else(|| Some(current.url.to_string()));
                return FetchResult::failure(
                    requested_url,
                    final_url,
                    None,
                    None,
                    String::new(),
                    Vec::new(),
                    transport_error_code(error.kind),
                    false,
                    0,
                );
            }
        };

        if is_redirect_status(response.status) {
            if redirects_followed >= MAX_REDIRECTS {
                return FetchResult::failure(
                    requested_url,
                    Some(response.url.to_string()),
                    Some(response.status),
                    normalize_content_type(response.content_type.as_deref()),
                    String::new(),
                    Vec::new(),
                    "redirect_error",
                    false,
                    response.bytes_read,
                );
            }
            let Some(location) = response.location.as_deref() else {
                return FetchResult::failure(
                    requested_url,
                    Some(response.url.to_string()),
                    Some(response.status),
                    normalize_content_type(response.content_type.as_deref()),
                    String::new(),
                    Vec::new(),
                    "redirect_error",
                    false,
                    response.bytes_read,
                );
            };
            let next_url = match response.url.join(location) {
                Ok(url) => url,
                Err(_) => {
                    return FetchResult::failure(
                        requested_url,
                        Some(response.url.to_string()),
                        Some(response.status),
                        normalize_content_type(response.content_type.as_deref()),
                        String::new(),
                        Vec::new(),
                        "redirect_error",
                        false,
                        response.bytes_read,
                    );
                }
            };
            current = match resolve_target_url(
                next_url,
                resolver,
                requested_url,
                Some(response.url.to_string()),
                deadline,
            ) {
                Ok(target) => target,
                Err(result) => return result,
            };
            redirects_followed += 1;
            continue;
        }

        return build_fetch_result(requested_url, response);
    }
}

fn fetch_target(
    target: &ResolvedTarget,
    deadline: Instant,
    transport: &dyn HttpTransport,
) -> Result<TransportResponse, TransportFailure> {
    let mut last_retryable = None;
    for address in candidate_addresses(&target.addresses) {
        let Some(remaining) = remaining_timeout(deadline) else {
            return Err(last_retryable.unwrap_or_else(|| TransportFailure {
                kind: TransportFailureKind::Timeout,
                url: Some(target.url.clone()),
            }));
        };
        match transport.get(&target.url, address, remaining) {
            Ok(response) => return Ok(response),
            Err(error) if should_retry_address(error.kind) => last_retryable = Some(error),
            Err(error) => return Err(error),
        }
    }

    Err(last_retryable.unwrap_or_else(|| TransportFailure {
        kind: TransportFailureKind::Connect,
        url: Some(target.url.clone()),
    }))
}

fn remaining_timeout(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

fn should_retry_address(kind: TransportFailureKind) -> bool {
    matches!(
        kind,
        TransportFailureKind::Connect | TransportFailureKind::Timeout
    )
}

fn candidate_addresses(addresses: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut unique = Vec::new();
    for &address in addresses {
        if !unique.contains(&address) {
            unique.push(address);
        }
    }

    if unique.len() <= MAX_ADDRESS_ATTEMPTS && unique.iter().all(|addr| addr.is_ipv4())
        || unique.iter().all(|addr| addr.is_ipv6())
    {
        unique.truncate(MAX_ADDRESS_ATTEMPTS);
        return unique;
    }

    let first_is_v6 = unique.first().map(SocketAddr::is_ipv6).unwrap_or(false);
    let mut ipv6 = VecDeque::from(
        unique
            .iter()
            .copied()
            .filter(SocketAddr::is_ipv6)
            .collect::<Vec<_>>(),
    );
    let mut ipv4 = VecDeque::from(
        unique
            .iter()
            .copied()
            .filter(SocketAddr::is_ipv4)
            .collect::<Vec<_>>(),
    );

    let mut ordered = Vec::new();
    let mut take_v6 = first_is_v6;
    while ordered.len() < MAX_ADDRESS_ATTEMPTS && (!ipv6.is_empty() || !ipv4.is_empty()) {
        let next = if take_v6 {
            ipv6.pop_front().or_else(|| ipv4.pop_front())
        } else {
            ipv4.pop_front().or_else(|| ipv6.pop_front())
        };
        if let Some(address) = next {
            ordered.push(address);
        }
        take_v6 = !take_v6;
    }

    ordered
}

fn build_fetch_result(requested_url: &str, response: TransportResponse) -> FetchResult {
    let extraction = extract_content(
        Some(response.url.as_str()),
        response.content_type.as_deref(),
        &response.body,
        response.content_length,
        response.body_truncated,
    );
    let mut warnings = extraction.warnings;
    let truncated = extraction.truncated;
    if truncated {
        push_warning(&mut warnings, Warning::ContentTruncated);
    }

    let content_type = extraction.content_type;
    let mut result = FetchResult {
        ok: response.status >= 200 && response.status < 300 && extraction.error.is_none(),
        requested_url: requested_url.to_string(),
        final_url: Some(response.url.to_string()),
        status: Some(response.status),
        content_type,
        content: extraction.content,
        extraction_kind: extraction.kind,
        warnings,
        error: extraction.error.map(ToOwned::to_owned),
        truncated,
        bytes_read: response.bytes_read,
    };

    if response.status >= 400 {
        result.ok = false;
        result.error = Some(format!("http_{}", response.status));
        result.extraction_kind = super::ExtractionKind::Error;
    } else if result.error.is_some() {
        result.ok = false;
        result.extraction_kind = super::ExtractionKind::Error;
    }

    result
}

fn resolve_target(
    input: &str,
    resolver: &dyn DnsResolver,
    deadline: Instant,
) -> Result<ResolvedTarget, FetchResult> {
    let url = match Url::parse(input) {
        Ok(url) => url,
        Err(_) => {
            return Err(FetchResult::failure(
                input,
                None,
                None,
                None,
                String::new(),
                Vec::new(),
                "invalid_url",
                false,
                0,
            ));
        }
    };
    resolve_target_url(url, resolver, input, None, deadline)
}

fn resolve_target_url(
    url: Url,
    resolver: &dyn DnsResolver,
    requested_url: &str,
    final_url: Option<String>,
    deadline: Instant,
) -> Result<ResolvedTarget, FetchResult> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FetchResult::failure(
            requested_url,
            final_url,
            None,
            None,
            String::new(),
            Vec::new(),
            "unsupported_scheme",
            false,
            0,
        ));
    }

    let Some(host) = url.host() else {
        return Err(FetchResult::failure(
            requested_url,
            final_url,
            None,
            None,
            String::new(),
            Vec::new(),
            "invalid_url",
            false,
            0,
        ));
    };
    let port = url.port_or_known_default().unwrap_or(80);
    let host_string = host.to_string();

    if matches!(host, Host::Domain(_)) && is_blocked_hostname(&host_string) {
        return Err(FetchResult::failure(
            requested_url,
            final_url,
            None,
            None,
            String::new(),
            Vec::new(),
            "blocked_address",
            false,
            0,
        ));
    }

    let addresses = match host {
        Host::Ipv4(addr) => vec![SocketAddr::new(IpAddr::V4(addr), port)],
        Host::Ipv6(addr) => vec![SocketAddr::new(IpAddr::V6(addr), port)],
        Host::Domain(_) => {
            let Some(timeout) = remaining_timeout(deadline) else {
                return Err(FetchResult::failure(
                    requested_url,
                    final_url,
                    None,
                    None,
                    String::new(),
                    Vec::new(),
                    "timeout",
                    false,
                    0,
                ));
            };
            match resolver.resolve(&host_string, port, timeout) {
                Ok(addresses) if !addresses.is_empty() => addresses,
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                    return Err(FetchResult::failure(
                        requested_url,
                        final_url,
                        None,
                        None,
                        String::new(),
                        Vec::new(),
                        "timeout",
                        false,
                        0,
                    ));
                }
                _ => {
                    return Err(FetchResult::failure(
                        requested_url,
                        final_url,
                        None,
                        None,
                        String::new(),
                        Vec::new(),
                        "dns_error",
                        false,
                        0,
                    ));
                }
            }
        }
    };

    if addresses.iter().any(|value| is_blocked_ip(value.ip())) {
        return Err(FetchResult::failure(
            requested_url,
            final_url,
            None,
            None,
            String::new(),
            Vec::new(),
            "blocked_address",
            false,
            0,
        ));
    }

    Ok(ResolvedTarget { url, addresses })
}

fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn map_transport_error(error: ureq::Transport, fallback_url: Option<Url>) -> TransportFailure {
    use std::io::ErrorKind;
    use ureq::ErrorKind as UreqErrorKind;

    let kind = match error.kind() {
        UreqErrorKind::Dns => TransportFailureKind::Dns,
        UreqErrorKind::TooManyRedirects => TransportFailureKind::Redirect,
        UreqErrorKind::ConnectionFailed | UreqErrorKind::ProxyConnect => {
            TransportFailureKind::Connect
        }
        UreqErrorKind::InvalidUrl | UreqErrorKind::UnknownScheme => TransportFailureKind::Decode,
        UreqErrorKind::Io => match error
            .source()
            .and_then(|source: &(dyn std::error::Error + 'static)| {
                source.downcast_ref::<std::io::Error>()
            })
            .map(std::io::Error::kind)
        {
            Some(ErrorKind::TimedOut) | Some(ErrorKind::WouldBlock) => {
                TransportFailureKind::Timeout
            }
            _ => TransportFailureKind::Connect,
        },
        _ => TransportFailureKind::Decode,
    };
    TransportFailure {
        kind,
        url: error.url().cloned().or(fallback_url),
    }
}

fn map_read_error(error: std::io::Error, url: Option<Url>) -> TransportFailure {
    use std::io::ErrorKind;
    let kind = match error.kind() {
        ErrorKind::TimedOut | ErrorKind::WouldBlock => TransportFailureKind::Timeout,
        _ => TransportFailureKind::Decode,
    };
    TransportFailure { kind, url }
}

fn transport_error_code(kind: TransportFailureKind) -> &'static str {
    match kind {
        TransportFailureKind::Dns => "dns_error",
        TransportFailureKind::Connect => "connect_error",
        TransportFailureKind::Timeout => "timeout",
        TransportFailureKind::Redirect => "redirect_error",
        TransportFailureKind::Decode => "decode_error",
    }
}

fn is_blocked_hostname(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    lower == "localhost"
        || lower == "localhost.localdomain"
        || lower.ends_with(".localhost")
        || lower == "metadata.google.internal"
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_blocked_ipv4(ip),
        IpAddr::V6(ip) => is_blocked_ipv6(ip),
    }
}

fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    matches!(octets, [0, ..])
        || octets[0] == 10
        || octets[0] == 127
        || (octets[0] == 169 && octets[1] == 254)
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 168)
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 198 && matches!(octets[1], 18 | 19))
        || octets[0] >= 224
}

fn is_blocked_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_blocked_ipv4(mapped);
    }
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    let segments = ip.segments();
    (segments[0] & 0xfe00) == 0xfc00 || (segments[0] & 0xffc0) == 0xfe80
}
