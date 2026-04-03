use std::{
    collections::VecDeque,
    error::Error as _,
    io::{self, Read},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    sync::{Arc, Condvar, Mutex, OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

use ureq::OrAnyStatus;
use url::{Host, Url};

use super::{
    CONNECT_TIMEOUT, FailureContext, FetchResult, MAX_ADDRESS_ATTEMPTS, MAX_DOWNLOAD_BYTES,
    MAX_REDIRECTS, Warning,
    content::{extract_content, normalize_content_type, push_warning},
};

const DNS_RESOLVER_WORKERS: usize = 4;
const DNS_RESOLVER_QUEUE_CAPACITY: usize = 64;
const BROWSER_USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) ",
    "AppleWebKit/537.36 (KHTML, like Gecko) ",
    "Chrome/135.0.0.0 Safari/537.36"
);

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
    TooLarge,
}

#[derive(Debug, Clone)]
pub(super) struct TransportFailure {
    pub(super) kind: TransportFailureKind,
    pub(super) context: FailureContext,
}

#[derive(Debug, Clone)]
struct ResolveFailure {
    code: &'static str,
    final_url: Option<String>,
}

pub(super) trait DnsResolver {
    fn resolve(&self, host: &str, port: u16, timeout: Duration) -> io::Result<Vec<SocketAddr>>;
}

pub(super) trait HttpTransport {
    fn get(
        &self,
        url: &Url,
        address: SocketAddr,
        timeout: Duration,
    ) -> Result<TransportResponse, TransportFailure>;
}

pub(super) trait ResolveBackend: Send + Sync {
    fn resolve_blocking(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>;
}

struct SystemResolveBackend;

struct ResolveRequest {
    host: String,
    port: u16,
    response: mpsc::Sender<io::Result<Vec<SocketAddr>>>,
}

#[derive(Default)]
struct ResolverPoolState {
    requests: VecDeque<ResolveRequest>,
}

struct ResolverPoolInner {
    backend: Arc<dyn ResolveBackend>,
    queue_capacity: usize,
    state: Mutex<ResolverPoolState>,
    pending: Condvar,
    available: Condvar,
}

#[derive(Clone)]
pub(super) struct ResolverPool {
    inner: Arc<ResolverPoolInner>,
}

pub(super) struct StdDnsResolver;
pub(super) struct UreqTransport;

static DNS_RESOLVER_POOL: OnceLock<ResolverPool> = OnceLock::new();

impl ResolveFailure {
    fn new(code: &'static str, final_url: Option<String>) -> Self {
        Self { code, final_url }
    }

    fn into_fetch_result(self, requested_url: &str) -> FetchResult {
        FetchResult::failure(
            requested_url,
            self.code,
            FailureContext {
                final_url: self.final_url,
                ..FailureContext::default()
            },
        )
    }
}

impl ResolveBackend for SystemResolveBackend {
    fn resolve_blocking(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        (host, port).to_socket_addrs().map(|iter| iter.collect())
    }
}

impl ResolverPool {
    pub(super) fn new(
        worker_count: usize,
        queue_capacity: usize,
        backend: Arc<dyn ResolveBackend>,
    ) -> Self {
        let inner = Arc::new(ResolverPoolInner {
            backend,
            queue_capacity: queue_capacity.max(1),
            state: Mutex::new(ResolverPoolState::default()),
            pending: Condvar::new(),
            available: Condvar::new(),
        });

        for worker_index in 0..worker_count.max(1) {
            let worker_inner = Arc::clone(&inner);
            thread::Builder::new()
                .name(format!("headless-dns-{worker_index}"))
                .spawn(move || resolver_worker(worker_inner))
                .expect("spawn dns resolver worker");
        }

        Self { inner }
    }

    pub(super) fn resolve(
        &self,
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> io::Result<Vec<SocketAddr>> {
        let deadline = Instant::now() + timeout;
        let (response_tx, response_rx) = mpsc::channel();
        self.enqueue(
            ResolveRequest {
                host: host.to_string(),
                port,
                response: response_tx,
            },
            deadline,
        )?;

        let Some(remaining) = remaining_timeout(deadline) else {
            return Err(timeout_error(host));
        };

        match response_rx.recv_timeout(remaining) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(timeout_error(host)),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(format!(
                "dns lookup worker disconnected for `{host}`"
            ))),
        }
    }

    fn enqueue(&self, request: ResolveRequest, deadline: Instant) -> io::Result<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("dns resolver queue lock poisoned");
        while state.requests.len() >= self.inner.queue_capacity {
            let Some(remaining) = remaining_timeout(deadline) else {
                return Err(timeout_error(&request.host));
            };
            let (next_state, wait_result) = self
                .inner
                .available
                .wait_timeout(state, remaining)
                .expect("dns resolver queue wait poisoned");
            state = next_state;
            if wait_result.timed_out() && state.requests.len() >= self.inner.queue_capacity {
                return Err(timeout_error(&request.host));
            }
        }

        state.requests.push_back(request);
        self.inner.pending.notify_one();
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn queued_requests(&self) -> usize {
        self.inner
            .state
            .lock()
            .expect("dns resolver queue lock poisoned")
            .requests
            .len()
    }
}

fn dns_resolver_pool() -> &'static ResolverPool {
    DNS_RESOLVER_POOL.get_or_init(|| {
        ResolverPool::new(
            DNS_RESOLVER_WORKERS,
            DNS_RESOLVER_QUEUE_CAPACITY,
            Arc::new(SystemResolveBackend),
        )
    })
}

fn resolver_worker(inner: Arc<ResolverPoolInner>) {
    loop {
        let request = {
            let mut state = inner
                .state
                .lock()
                .expect("dns resolver queue lock poisoned");
            while state.requests.is_empty() {
                state = inner
                    .pending
                    .wait(state)
                    .expect("dns resolver queue wait poisoned");
            }
            let request = state.requests.pop_front().expect("queued dns request");
            inner.available.notify_one();
            request
        };

        let result = inner.backend.resolve_blocking(&request.host, request.port);
        let _ = request.response.send(result);
    }
}

impl DnsResolver for StdDnsResolver {
    fn resolve(&self, host: &str, port: u16, timeout: Duration) -> io::Result<Vec<SocketAddr>> {
        dns_resolver_pool().resolve(host, port, timeout)
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
            .set("User-Agent", BROWSER_USER_AGENT)
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
                context: FailureContext {
                    final_url: Some(url.to_string()),
                    ..FailureContext::default()
                },
            })?;
        let content_type = response.header("content-type").map(ToOwned::to_owned);
        let location = response.header("location").map(ToOwned::to_owned);
        let content_length = response
            .header("content-length")
            .and_then(|value: &str| value.parse::<u64>().ok());
        if content_length.is_some_and(|value| value > MAX_DOWNLOAD_BYTES as u64) {
            return Err(TransportFailure {
                kind: TransportFailureKind::TooLarge,
                context: FailureContext {
                    final_url: Some(response_url.to_string()),
                    status: Some(status),
                    content_type: normalize_content_type(content_type.as_deref()),
                    ..FailureContext::default()
                },
            });
        }
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
        Err(error) => return error.into_fetch_result(requested_url),
    };

    let mut redirects_followed = 0usize;
    loop {
        let response = match fetch_target(&current, deadline, transport) {
            Ok(response) => response,
            Err(error) => {
                let mut context = error.context;
                if context.final_url.is_none() {
                    context.final_url = Some(current.url.to_string());
                }
                return FetchResult::failure(
                    requested_url,
                    transport_error_code(error.kind),
                    context,
                );
            }
        };

        if is_redirect_status(response.status) {
            let redirect_context = FailureContext {
                final_url: Some(response.url.to_string()),
                status: Some(response.status),
                content_type: normalize_content_type(response.content_type.as_deref()),
                bytes_read: response.bytes_read,
                ..FailureContext::default()
            };
            if redirects_followed >= MAX_REDIRECTS {
                return FetchResult::failure(requested_url, "redirect_error", redirect_context);
            }
            let Some(location) = response.location.as_deref() else {
                return FetchResult::failure(requested_url, "redirect_error", redirect_context);
            };
            let next_url = match response.url.join(location) {
                Ok(url) => url,
                Err(_) => {
                    return FetchResult::failure(requested_url, "redirect_error", redirect_context);
                }
            };
            current = match resolve_target_url(
                next_url,
                resolver,
                Some(response.url.to_string()),
                deadline,
            ) {
                Ok(target) => target,
                Err(error) => return error.into_fetch_result(requested_url),
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
                context: FailureContext {
                    final_url: Some(target.url.to_string()),
                    ..FailureContext::default()
                },
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
        context: FailureContext {
            final_url: Some(target.url.to_string()),
            ..FailureContext::default()
        },
    }))
}

fn remaining_timeout(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

fn timeout_error(host: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("dns lookup timed out for `{host}`"),
    )
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

    if unique.len() <= MAX_ADDRESS_ATTEMPTS
        && (unique.iter().all(|addr| addr.is_ipv4()) || unique.iter().all(|addr| addr.is_ipv6()))
    {
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
) -> Result<ResolvedTarget, ResolveFailure> {
    let url = Url::parse(input).map_err(|_| ResolveFailure::new("invalid_url", None))?;
    resolve_target_url(url, resolver, None, deadline)
}

fn resolve_target_url(
    url: Url,
    resolver: &dyn DnsResolver,
    final_url: Option<String>,
    deadline: Instant,
) -> Result<ResolvedTarget, ResolveFailure> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ResolveFailure::new("unsupported_scheme", final_url));
    }

    let Some(host) = url.host() else {
        return Err(ResolveFailure::new("invalid_url", final_url));
    };
    let port = url.port_or_known_default().unwrap_or(80);
    let host_string = host.to_string();

    if matches!(host, Host::Domain(_)) && is_blocked_hostname(&host_string) {
        return Err(ResolveFailure::new("blocked_address", final_url));
    }

    let addresses = match host {
        Host::Ipv4(addr) => vec![SocketAddr::new(IpAddr::V4(addr), port)],
        Host::Ipv6(addr) => vec![SocketAddr::new(IpAddr::V6(addr), port)],
        Host::Domain(_) => {
            let Some(timeout) = remaining_timeout(deadline) else {
                return Err(ResolveFailure::new("timeout", final_url));
            };
            match resolver.resolve(&host_string, port, timeout) {
                Ok(addresses) if !addresses.is_empty() => addresses,
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                    return Err(ResolveFailure::new("timeout", final_url));
                }
                _ => return Err(ResolveFailure::new("dns_error", final_url)),
            }
        }
    };

    if addresses.iter().any(|value| is_blocked_ip(value.ip())) {
        return Err(ResolveFailure::new("blocked_address", final_url));
    }

    Ok(ResolvedTarget { url, addresses })
}

fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn map_transport_error(error: ureq::Transport, fallback_url: Option<Url>) -> TransportFailure {
    use io::ErrorKind;
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
                source.downcast_ref::<io::Error>()
            })
            .map(io::Error::kind)
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
        context: FailureContext {
            final_url: error
                .url()
                .cloned()
                .or(fallback_url)
                .map(|value| value.to_string()),
            ..FailureContext::default()
        },
    }
}

fn map_read_error(error: io::Error, url: Option<Url>) -> TransportFailure {
    use io::ErrorKind;
    let kind = match error.kind() {
        ErrorKind::TimedOut | ErrorKind::WouldBlock => TransportFailureKind::Timeout,
        _ => TransportFailureKind::Decode,
    };
    TransportFailure {
        kind,
        context: FailureContext {
            final_url: url.map(|value| value.to_string()),
            ..FailureContext::default()
        },
    }
}

fn transport_error_code(kind: TransportFailureKind) -> &'static str {
    match kind {
        TransportFailureKind::Dns => "dns_error",
        TransportFailureKind::Connect => "connect_error",
        TransportFailureKind::Timeout => "timeout",
        TransportFailureKind::Redirect => "redirect_error",
        TransportFailureKind::Decode => "decode_error",
        TransportFailureKind::TooLarge => "content_too_large",
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
