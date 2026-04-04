use std::{
    collections::{HashMap, HashSet},
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use url::Url;

use super::{
    FailureContext, FetchResult, REQUEST_TIMEOUT, Warning,
    content::ExtractedContent,
    fetch_url_with_timeout,
    html::extract_html,
    live_canaries::{self, CanaryCase, CanaryTier},
    render_cli_output,
    transport::{
        DnsResolver, HttpTransport, ResolveBackend, ResolverPool, TransportFailure,
        TransportFailureKind, TransportResponse, UreqTransport, fetch_with_clients,
    },
};

const GITHUB_REPO_FIXTURE: &str = include_str!("fixtures/github/repo.html");
const GITHUB_TREE_FIXTURE: &str = include_str!("fixtures/github/tree.html");
const GITHUB_BLOB_FIXTURE: &str = include_str!("fixtures/github/blob.html");
const GITHUB_ISSUE_FIXTURE: &str = include_str!("fixtures/github/issue.html");
const GITHUB_PULL_FIXTURE: &str = include_str!("fixtures/github/pull.html");
const GITHUB_RELEASES_FIXTURE: &str = include_str!("fixtures/github/releases.html");

#[derive(Default)]
struct FakeResolver {
    values: HashMap<(String, u16), FakeDnsEntry>,
}

#[derive(Debug, Clone)]
struct FakeDnsEntry {
    required_budget: Duration,
    outcome: FakeDnsOutcome,
}

#[derive(Debug, Clone)]
enum FakeDnsOutcome {
    Addresses(Vec<SocketAddr>),
    Error {
        kind: io::ErrorKind,
        message: String,
    },
}

impl FakeResolver {
    fn with_mapping(mut self, host: &str, port: u16, addresses: Vec<SocketAddr>) -> Self {
        self.values.insert(
            (host.to_string(), port),
            FakeDnsEntry {
                required_budget: Duration::ZERO,
                outcome: FakeDnsOutcome::Addresses(addresses),
            },
        );
        self
    }

    fn with_budgeted_mapping(
        mut self,
        host: &str,
        port: u16,
        required_budget: Duration,
        addresses: Vec<SocketAddr>,
    ) -> Self {
        self.values.insert(
            (host.to_string(), port),
            FakeDnsEntry {
                required_budget,
                outcome: FakeDnsOutcome::Addresses(addresses),
            },
        );
        self
    }

    fn with_error(mut self, host: &str, port: u16, kind: io::ErrorKind, message: &str) -> Self {
        self.values.insert(
            (host.to_string(), port),
            FakeDnsEntry {
                required_budget: Duration::ZERO,
                outcome: FakeDnsOutcome::Error {
                    kind,
                    message: message.to_string(),
                },
            },
        );
        self
    }
}

impl DnsResolver for FakeResolver {
    fn resolve(&self, host: &str, port: u16, timeout: Duration) -> io::Result<Vec<SocketAddr>> {
        match self.values.get(&(host.to_string(), port)) {
            Some(entry) => {
                if timeout < entry.required_budget {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "fake dns timeout"));
                }
                match &entry.outcome {
                    FakeDnsOutcome::Addresses(addresses) => Ok(addresses.clone()),
                    FakeDnsOutcome::Error { kind, message } => {
                        Err(io::Error::new(*kind, message.clone()))
                    }
                }
            }
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "missing fake dns entry",
            )),
        }
    }
}

#[derive(Default, Clone)]
struct FakeTransport {
    responses: Arc<HashMap<(String, SocketAddr), Result<TransportResponse, TransportFailureKind>>>,
}

impl FakeTransport {
    fn new(
        map: HashMap<(String, SocketAddr), Result<TransportResponse, TransportFailureKind>>,
    ) -> Self {
        Self {
            responses: Arc::new(map),
        }
    }
}

impl HttpTransport for FakeTransport {
    fn get(
        &self,
        url: &Url,
        address: SocketAddr,
        _timeout: Duration,
    ) -> Result<TransportResponse, TransportFailure> {
        match self.responses.get(&(url.to_string(), address)) {
            Some(Ok(response)) => Ok(response.clone()),
            Some(Err(kind)) => Err(TransportFailure {
                kind: *kind,
                context: FailureContext {
                    final_url: Some(url.to_string()),
                    ..FailureContext::default()
                },
            }),
            None => Err(TransportFailure {
                kind: TransportFailureKind::Connect,
                context: FailureContext {
                    final_url: Some(url.to_string()),
                    ..FailureContext::default()
                },
            }),
        }
    }
}

fn socket(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), port)
}

fn socket_v4(a: u8, b: u8, c: u8, d: u8, port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(a, b, c, d)), port)
}

fn socket_v6(segments: [u16; 8], port: u16) -> SocketAddr {
    SocketAddr::new(
        IpAddr::V6(Ipv6Addr::new(
            segments[0],
            segments[1],
            segments[2],
            segments[3],
            segments[4],
            segments[5],
            segments[6],
            segments[7],
        )),
        port,
    )
}

fn socket_v4_mapped(a: u8, b: u8, c: u8, d: u8, port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V6(Ipv4Addr::new(a, b, c, d).to_ipv6_mapped()), port)
}

fn response(url: &str, status: u16, content_type: Option<&str>, body: &[u8]) -> TransportResponse {
    TransportResponse {
        url: Url::parse(url).expect("url"),
        status,
        content_type: content_type.map(ToOwned::to_owned),
        location: None,
        content_length: Some(body.len() as u64),
        body: body.to_vec(),
        bytes_read: body.len() as u64,
        body_truncated: false,
    }
}

fn spawn_http_test_server(
    response: String,
) -> (SocketAddr, Arc<Mutex<String>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
    let address = listener.local_addr().expect("listener addr");
    let request = Arc::new(Mutex::new(String::new()));
    let captured_request = Arc::clone(&request);
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        let mut buffer = [0u8; 8192];
        let bytes_read = stream.read(&mut buffer).expect("read request");
        *captured_request.lock().expect("capture request") =
            String::from_utf8_lossy(&buffer[..bytes_read]).into_owned();
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    });
    (address, request, handle)
}

fn html_output(body: &str) -> ExtractedContent {
    extract_html(None, Some("text/html"), body, false)
}

fn html_output_at_url(url: &str, body: &str) -> ExtractedContent {
    extract_html(Some(url), Some("text/html"), body, false)
}

fn fetch_body(url: &str, content_type: Option<&str>, body: &[u8]) -> FetchResult {
    let parsed = Url::parse(url).expect("url");
    let host = parsed.host_str().expect("host");
    let port = parsed.port_or_known_default().expect("known port");
    let resolver = FakeResolver::default().with_mapping(host, port, vec![socket(port)]);
    let transport = FakeTransport::new(HashMap::from([(
        (url.to_string(), socket(port)),
        Ok(response(url, 200, content_type, body)),
    )]));
    fetch_with_clients(url, REQUEST_TIMEOUT, &resolver, &transport)
}

#[derive(Debug)]
struct BlockingResolveBackend {
    release: Arc<(Mutex<bool>, Condvar)>,
    active: AtomicUsize,
    max_active: AtomicUsize,
    completed: AtomicUsize,
}

impl BlockingResolveBackend {
    fn new() -> Self {
        Self {
            release: Arc::new((Mutex::new(false), Condvar::new())),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
        }
    }

    fn release_all(&self) {
        let (lock, condvar) = &*self.release;
        *lock.lock().expect("release lock") = true;
        condvar.notify_all();
    }
}

impl ResolveBackend for BlockingResolveBackend {
    fn resolve_blocking(&self, _host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        update_max(&self.max_active, active);

        let (lock, condvar) = &*self.release;
        let mut released = lock.lock().expect("release lock");
        while !*released {
            released = condvar.wait(released).expect("release wait");
        }
        drop(released);

        self.active.fetch_sub(1, Ordering::SeqCst);
        self.completed.fetch_add(1, Ordering::SeqCst);
        Ok(vec![socket(port)])
    }
}

fn update_max(target: &AtomicUsize, value: usize) {
    let mut current = target.load(Ordering::SeqCst);
    while current < value {
        match target.compare_exchange(current, value, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return,
            Err(next) => current = next,
        }
    }
}

fn wait_until<F>(timeout: Duration, condition: F)
where
    F: Fn() -> bool,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(condition(), "condition was not met before timeout");
}

#[test]
fn blocks_loopback_hosts() {
    let resolver = FakeResolver::default();
    let transport = FakeTransport::default();
    let result = fetch_with_clients(
        "http://localhost/path",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert_eq!(result.error.as_deref(), Some("blocked_address"));
    assert!(!result.ok);
}

#[test]
fn blocks_ipv4_mapped_ipv6_loopback_literals() {
    let resolver = FakeResolver::default();
    let transport = FakeTransport::default();
    let result = fetch_with_clients(
        "http://[::ffff:127.0.0.1]:9/",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert_eq!(result.error.as_deref(), Some("blocked_address"));
    assert!(!result.ok);
}

#[test]
fn blocks_resolved_ipv4_mapped_ipv6_private_addresses() {
    let resolver = FakeResolver::default().with_mapping(
        "example.test",
        443,
        vec![socket_v4_mapped(10, 0, 0, 7, 443)],
    );
    let transport = FakeTransport::default();
    let result = fetch_with_clients(
        "https://example.test/",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert_eq!(result.error.as_deref(), Some("blocked_address"));
    assert!(!result.ok);
}

#[test]
fn blocks_resolved_ipv4_mapped_ipv6_link_local_addresses() {
    let resolver = FakeResolver::default().with_mapping(
        "example.test",
        80,
        vec![socket_v4_mapped(169, 254, 169, 254, 80)],
    );
    let transport = FakeTransport::default();
    let result = fetch_with_clients(
        "http://example.test/",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert_eq!(result.error.as_deref(), Some("blocked_address"));
    assert!(!result.ok);
}

#[test]
fn tries_later_address_after_connect_failure() {
    let first = socket_v6([0x2606, 0, 0, 0, 0, 0, 0, 1], 443);
    let second = socket_v4(93, 184, 216, 35, 443);
    let resolver = FakeResolver::default().with_mapping("example.test", 443, vec![first, second]);
    let transport = FakeTransport::new(HashMap::from([
        (
            ("https://example.test/".to_string(), first),
            Err(TransportFailureKind::Connect),
        ),
        (
            ("https://example.test/".to_string(), second),
            Ok(response(
                "https://example.test/",
                200,
                Some("text/html"),
                br#"<html><body><main><p>Hello world.</p></main></body></html>"#,
            )),
        ),
    ]));

    let result = fetch_with_clients(
        "https://example.test/",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );

    assert!(result.ok);
    assert_eq!(result.status, Some(200));
    assert!(result.content.contains("Hello world."));
}

#[test]
fn follows_redirects_and_re_resolves_last_url() {
    let start = socket(80);
    let docs = socket_v4(93, 184, 216, 99, 80);
    let resolver = FakeResolver::default()
        .with_mapping("start.test", 80, vec![start])
        .with_mapping("docs.test", 80, vec![docs]);
    let transport = FakeTransport::new(HashMap::from([
        (
            ("http://start.test/start".to_string(), start),
            Ok(TransportResponse {
                location: Some("http://docs.test/docs".to_string()),
                ..response("http://start.test/start", 302, Some("text/plain"), b"")
            }),
        ),
        (
            ("http://docs.test/docs".to_string(), docs),
            Ok(response(
                "http://docs.test/docs",
                200,
                Some("text/html"),
                br#"<html><body><main><h1>Docs</h1><p>Hello world.</p></main></body></html>"#,
            )),
        ),
    ]));
    let result = fetch_with_clients(
        "http://start.test/start",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert!(result.ok);
    assert_eq!(result.final_url.as_deref(), Some("http://docs.test/docs"));
    assert_eq!(result.extraction_kind, super::ExtractionKind::HtmlPrimary);
    assert!(result.content.contains("Title: Docs"));
    assert!(!result.content.contains("# Docs"));
    assert!(result.content.contains("Hello world."));
}

#[test]
fn dns_resolution_timeout_returns_structured_timeout() {
    let resolver = FakeResolver::default().with_budgeted_mapping(
        "example.test",
        443,
        Duration::from_millis(50),
        vec![socket(443)],
    );
    let transport = FakeTransport::default();
    let result = fetch_with_clients(
        "https://example.test/",
        Duration::from_millis(10),
        &resolver,
        &transport,
    );

    assert!(!result.ok);
    assert_eq!(result.error.as_deref(), Some("timeout"));
    assert_eq!(result.extraction_kind, super::ExtractionKind::Error);
}

#[test]
fn dns_resolution_failures_stay_dns_errors() {
    let resolver = FakeResolver::default().with_error(
        "example.test",
        443,
        io::ErrorKind::Other,
        "resolver failed",
    );
    let transport = FakeTransport::default();
    let result = fetch_with_clients(
        "https://example.test/",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );

    assert!(!result.ok);
    assert_eq!(result.error.as_deref(), Some("dns_error"));
    assert_eq!(result.extraction_kind, super::ExtractionKind::Error);
}

#[test]
fn all_candidate_addresses_fail_with_structured_transport_error() {
    let first = socket_v6([0x2606, 0, 0, 0, 0, 0, 0, 1], 443);
    let second = socket_v4(93, 184, 216, 35, 443);
    let resolver = FakeResolver::default().with_mapping("example.test", 443, vec![first, second]);
    let transport = FakeTransport::new(HashMap::from([
        (
            ("https://example.test/".to_string(), first),
            Err(TransportFailureKind::Connect),
        ),
        (
            ("https://example.test/".to_string(), second),
            Err(TransportFailureKind::Timeout),
        ),
    ]));

    let result = fetch_with_clients(
        "https://example.test/",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );

    assert!(!result.ok);
    assert!(matches!(
        result.error.as_deref(),
        Some("connect_error" | "timeout")
    ));
    assert_eq!(result.extraction_kind, super::ExtractionKind::Error);
}

#[test]
fn html_extraction_strips_obvious_chrome() {
    let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
    let body = br#"
        <html>
          <head>
            <title>Pricing</title>
            <meta name="description" content="Simple plans" />
          </head>
          <body>
            <nav>Top nav</nav>
            <main>
              <h1>Plans</h1>
              <p>Starter plan</p>
              <div class="share-tools">tweet this</div>
              <pre>cargo install headless</pre>
            </main>
          </body>
        </html>
    "#;
    let transport = FakeTransport::new(HashMap::from([(
        ("http://example.test/pricing".to_string(), socket(80)),
        Ok(response(
            "http://example.test/pricing",
            200,
            Some("text/html"),
            body,
        )),
    )]));
    let result = fetch_with_clients(
        "http://example.test/pricing",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert!(result.ok);
    assert!(result.content.contains("Title: Pricing"));
    assert!(result.content.contains("Description: Simple plans"));
    assert!(result.content.contains("# Plans"));
    assert!(result.content.contains("Starter plan"));
    assert!(result.content.contains("```\ncargo install headless\n```"));
    assert!(!result.content.contains("Top nav"));
    assert!(!result.content.contains("tweet this"));
    assert!(!result.content.contains("H1:"));
}

#[test]
fn metadata_fallbacks_and_crumb_filters_are_deterministic() {
    let result = html_output(
        r#"
        <html>
          <head>
            <meta property="og:title" content="OG Title" />
            <meta name="twitter:description" content="Twitter summary" />
          </head>
          <body>
            <main>
              <p>Expand description</p>
              <p>[edit]</p>
              <p>Keep me</p>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(result.content.contains("Title: OG Title"));
    assert!(result.content.contains("Description: Twitter summary"));
    assert!(result.content.contains("Keep me"));
    assert!(!result.content.contains("Expand description"));
    assert!(!result.content.contains("[edit]"));
}

#[test]
fn schema_fallback_replaces_low_signal_dom_content() {
    let result = html_output(
        r#"
        <html>
          <head>
            <title>JS App</title>
            <script type="application/ld+json">
              {
                "@context": "https://schema.org",
                "@type": "Article",
                "headline": "Recovered article",
                "articleBody": "This article body came from schema.org and includes enough text to replace a shell page that would otherwise look empty to the extractor."
              }
            </script>
          </head>
          <body>
            <div id="__next"></div>
          </body>
        </html>
        "#,
    );

    assert!(
        result
            .content
            .contains("This article body came from schema.org")
    );
    assert!(!result.warnings.contains(&Warning::LowSignalExtraction));
    assert!(!result.warnings.contains(&Warning::PossibleJsRenderedPage));
}

#[test]
fn schema_fallback_does_not_override_healthy_dom_content() {
    let result = html_output(
        r#"
        <html>
          <head>
            <title>Healthy page</title>
            <script type="application/ld+json">
              {
                "@context": "https://schema.org",
                "@type": "Article",
                "articleBody": "Schema fallback text that should not replace the visible article because the DOM extraction is already healthy and complete."
              }
            </script>
          </head>
          <body>
            <main>
              <p>This visible article content is already healthy, readable, and long enough that schema fallback should stay unused.</p>
              <p>The extractor should preserve this DOM-first result rather than swapping in alternate schema text.</p>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(
        result
            .content
            .contains("This visible article content is already healthy")
    );
    assert!(
        !result
            .content
            .contains("Schema fallback text that should not replace")
    );
}

#[test]
fn author_and_published_metadata_are_rendered_only_when_present() {
    let with_metadata = html_output(
        r#"
        <html>
          <head>
            <meta name="author" content="Jane Doe" />
            <meta property="article:published_time" content="2026-02-01T09:30:00Z" />
          </head>
          <body>
            <main>
              <p>Useful content lives here.</p>
            </main>
          </body>
        </html>
        "#,
    );
    assert!(with_metadata.content.contains("Author: Jane Doe"));
    assert!(
        with_metadata
            .content
            .contains("Published: 2026-02-01T09:30:00Z")
    );

    let without_metadata = html_output(
        r#"
        <html>
          <body>
            <main>
              <p>Useful content lives here too.</p>
            </main>
          </body>
        </html>
        "#,
    );
    assert!(!without_metadata.content.contains("Author:"));
    assert!(!without_metadata.content.contains("Published:"));
}

#[test]
fn hidden_utility_classes_and_source_crumbs_are_removed() {
    let result = html_output(
        r#"
        <html>
          <body>
            <main>
              <div class="hidden">secret</div>
              <div class="sm:hidden">also secret</div>
              <div class="invisible">ghost</div>
              <p>Source</p>
              <p>Keep this paragraph.</p>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(result.content.contains("Keep this paragraph."));
    assert!(!result.content.contains("secret"));
    assert!(!result.content.contains("ghost"));
    assert!(!result.content.contains("\nSource\n"));
}

#[test]
fn hidden_style_declarations_with_spaces_are_removed() {
    let result = html_output(
        r#"
        <html>
          <body>
            <main>
              <div style="display: none">display hidden</div>
              <div style="visibility: hidden">visibility hidden</div>
              <div style="opacity: 0">opacity hidden</div>
              <p>Keep this paragraph.</p>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(result.content.contains("Keep this paragraph."));
    assert!(!result.content.contains("display hidden"));
    assert!(!result.content.contains("visibility hidden"));
    assert!(!result.content.contains("opacity hidden"));
}

#[test]
fn noisy_token_matching_avoids_mid_token_false_positives() {
    let result = html_output(
        r#"
        <html>
          <body>
            <main>
              <div class="canvas-panel">Canvas guidance stays visible.</div>
              <div class="unavailable-notice">Availability notice stays visible.</div>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(result.content.contains("Canvas guidance stays visible."));
    assert!(
        result
            .content
            .contains("Availability notice stays visible.")
    );
}

#[test]
fn nested_lists_render_with_indentation() {
    let result = html_output(
        r#"
        <html>
          <body>
            <main>
              <ul>
                <li>
                  Install
                  <ul>
                    <li>cargo install headless</li>
                  </ul>
                </li>
                <li>Run it</li>
              </ul>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(
        result
            .content
            .contains("- Install\n    - cargo install headless\n- Run it")
    );
}

#[test]
fn blockquotes_render_with_markdown_prefixes() {
    let result = html_output(
        r#"
        <html>
          <body>
            <main>
              <blockquote>
                <p>Stay hungry.</p>
                <p>Stay foolish.</p>
              </blockquote>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(
        result
            .content
            .contains("> Stay hungry.\n>\n> Stay foolish.")
    );
}

#[test]
fn syntax_highlighted_code_blocks_drop_chrome_and_preserve_lines() {
    let result = html_output(
        r#"
        <html>
          <body>
            <main>
              <pre>
                <code class="language-rust">
                  <button>Copy</button>
                  <span data-line><span class="line-number">1</span>fn main() {</span>
                  <span data-line><span class="lnt">2</span>    println!("hi");</span>
                  <span data-line><span class="line-number">3</span>}</span>
                </code>
              </pre>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(
        result
            .content
            .contains("```rust\nfn main() {\n    println!(\"hi\");\n}\n```")
    );
    assert!(!result.content.contains("Copy"));
    assert!(!result.content.contains("line-number"));
}

#[test]
fn whitespace_preserving_code_without_pre_becomes_fenced_block() {
    let result = html_output(
        r#"
        <html>
          <body>
            <main>
              <code style="white-space: pre">/ip address
  add address=192.168.88.1/24 interface=bridge1</code>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(
        result
            .content
            .contains("```\n/ip address\n  add address=192.168.88.1/24 interface=bridge1\n```")
    );
}

#[test]
fn simple_tables_render_as_markdown_and_complex_tables_fall_back_to_text() {
    let simple = html_output(
        r#"
        <html>
          <body>
            <main>
              <table>
                <thead>
                  <tr><th>Name</th><th>Score</th></tr>
                </thead>
                <tbody>
                  <tr><td>Alice</td><td>95</td></tr>
                  <tr><td>Bob</td><td>87</td></tr>
                </tbody>
              </table>
            </main>
          </body>
        </html>
        "#,
    );
    assert!(
        simple
            .content
            .contains("| Name | Score |\n| --- | --- |\n| Alice | 95 |\n| Bob | 87 |")
    );

    let complex = html_output(
        r#"
        <html>
          <body>
            <main>
              <table>
                <tr><td rowspan="2">A</td><td>B</td></tr>
                <tr><td>C</td></tr>
              </table>
            </main>
          </body>
        </html>
        "#,
    );
    assert!(complex.content.contains('A'));
    assert!(complex.content.contains('B'));
    assert!(complex.content.contains('C'));
    assert!(!complex.content.contains("| --- |"));
}

#[test]
fn truncation_closes_fenced_code_blocks() {
    let long_code = "line\n".repeat(60_000);
    let html = format!(
        "<html><body><main><pre><code>{}</code></pre></main></body></html>",
        long_code
    );
    let extraction = extract_html(None, Some("text/html"), &html, false);

    assert!(extraction.truncated);
    assert!(extraction.warnings.contains(&Warning::ContentTruncated));
    assert!(extraction.content.ends_with("\n```"));
}

#[test]
fn inline_code_uses_a_longer_delimiter_when_needed() {
    let result = html_output(
        r#"
        <html>
          <body>
            <main>
              <p>Use <code>foo`bar</code> now.</p>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(result.content.contains("Use ``foo`bar`` now."));
}

#[test]
fn inline_code_adds_padding_when_payload_starts_and_ends_with_backticks() {
    let result = html_output(
        r#"
        <html>
          <body>
            <main>
              <p><code>`quoted`</code></p>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(result.content.contains("`` `quoted` ``"));
}

#[test]
fn code_fences_grow_when_code_contains_triple_backticks() {
    let result = html_output(
        r#"
        <html>
          <body>
            <main>
              <pre><code class="language-md">alpha
```
beta</code></pre>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(result.content.contains("````md\nalpha\n```\nbeta\n````"));
}

#[test]
fn truncation_closes_the_exact_open_code_fence() {
    let long_code = format!("{}\n```\nclosing candidate", "line\n".repeat(60_000));
    let html = format!(
        "<html><body><main><pre><code class=\"language-md\">{}</code></pre></main></body></html>",
        long_code
    );
    let extraction = extract_html(None, Some("text/html"), &html, false);

    assert!(extraction.truncated);
    assert!(extraction.content.ends_with("\n````"));
}

#[test]
fn http_links_render_as_markdown_and_non_http_links_stay_plain_text() {
    let result = html_output_at_url(
        "https://example.test/articles/start",
        r##"
        <html>
          <body>
            <main>
              <p>
                Read the <a href="/docs">docs</a>,
                browse the <a href="https://api.example.test/v1">API</a>,
                jump to <a href="#footnotes">footnotes</a>,
                and ignore <a href="javascript:alert('x')">this</a>.
              </p>
            </main>
          </body>
        </html>
        "##,
    );

    assert!(
        result
            .content
            .contains("[docs](<https://example.test/docs>)")
    );
    assert!(
        result
            .content
            .contains("[API](<https://api.example.test/v1>)")
    );
    assert!(result.content.contains("footnotes"));
    assert!(!result.content.contains("#footnotes"));
    assert!(result.content.contains("this"));
    assert!(!result.content.contains("javascript:alert"));
}

#[test]
fn candidate_root_scoring_skips_shell_main_for_real_article_content() {
    let result = html_output_at_url(
        "https://example.test/blog/post",
        r#"
        <html>
          <body>
            <main class="nav-shell">
              <p><a href="/docs">Docs</a> <a href="/pricing">Pricing</a> <a href="/blog">Blog</a></p>
            </main>
            <article>
              <h1>Launch notes</h1>
              <p>This release explains how the worker pool is initialized, how retries behave, and how operators should verify rollout health.</p>
              <p>It also covers failure handling, metrics, and the migration path for existing deployments that use older fetch settings.</p>
            </article>
          </body>
        </html>
        "#,
    );

    assert!(
        result
            .content
            .contains("This release explains how the worker pool is initialized")
    );
    assert!(
        !result.content.contains(
            "[Docs](<https://example.test/docs>) [Pricing](<https://example.test/pricing>)"
        )
    );
    assert!(!result.warnings.contains(&Warning::LowSignalExtraction));
}

#[test]
fn github_issue_embedded_data_extracts_body_author_and_published() {
    let result = html_output_at_url(
        "https://github.com/example/repo/issues/42",
        r#"
        <html>
          <head>
            <title>Embedded issue</title>
          </head>
          <body>
            <script type="application/json" data-target="react-app.embeddedData">
              {
                "payload": {
                  "preloadedQueries": [
                    {
                      "result": {
                        "data": {
                          "repository": {
                            "issue": {
                              "number": 42,
                              "__typename": "Issue",
                              "body": "This issue body came from embedded GitHub data.\n\nIt should be extracted even when the visible DOM is sparse.",
                              "createdAt": "2026-02-03T04:05:06Z",
                              "author": { "login": "octocat" }
                            }
                          }
                        }
                      }
                    }
                  ]
                }
              }
            </script>
            <div id="repo-content-pjax-container"></div>
          </body>
        </html>
        "#,
    );

    assert!(
        result
            .content
            .contains("This issue body came from embedded GitHub data.")
    );
    assert!(result.content.contains("Author: octocat"));
    assert!(result.content.contains("Published: 2026-02-03T04:05:06Z"));
}

#[test]
fn github_pr_visible_body_fallback_extracts_body_author_and_published() {
    let result = html_output_at_url(
        "https://github.com/example/repo/pull/7",
        r#"
        <html>
          <body>
            <div class="gh-header-meta">
              <a class="author" href="/octocat">octocat</a>
            </div>
            <relative-time datetime="2026-03-04T05:06:07Z"></relative-time>
            <div class="timeline-comment">
              <div class="comment-body markdown-body">
                <p>Fix the flaky test by waiting for the worker to finish.</p>
                <pre><code class="language-rust">assert!(done);</code></pre>
              </div>
            </div>
          </body>
        </html>
        "#,
    );

    assert!(
        result
            .content
            .contains("Fix the flaky test by waiting for the worker to finish.")
    );
    assert!(result.content.contains("```rust\nassert!(done);\n```"));
    assert!(result.content.contains("Author: octocat"));
    assert!(result.content.contains("Published: 2026-03-04T05:06:07Z"));
}

#[test]
fn github_issue_embedded_discussion_appends_issue_comments() {
    let result = html_output_at_url(
        "https://github.com/example/repo/issues/42",
        r#"
        <html>
          <body>
            <script type="application/json" data-target="react-app.embeddedData">
              {
                "payload": {
                  "preloadedQueries": [
                    {
                      "result": {
                        "data": {
                          "repository": {
                            "issue": {
                              "number": 42,
                              "__typename": "Issue",
                              "bodyHTML": "<p>Main issue body.</p>",
                              "createdAt": "2026-02-03T04:05:06Z",
                              "author": { "login": "octocat" },
                              "frontTimelineItems": {
                                "edges": [
                                  {
                                    "node": {
                                      "__typename": "IssueComment",
                                      "bodyHTML": "<p>First follow-up from the thread.</p>",
                                      "createdAt": "2026-02-04T01:02:03Z",
                                      "author": { "login": "reviewer1" }
                                    }
                                  },
                                  {
                                    "node": {
                                      "__typename": "CrossReferencedEvent",
                                      "createdAt": "2026-02-05T01:02:03Z"
                                    }
                                  }
                                ]
                              }
                            }
                          }
                        }
                      }
                    }
                  ]
                }
              }
            </script>
          </body>
        </html>
        "#,
    );

    assert!(result.content.contains("Main issue body."));
    assert!(result.content.contains("## Discussion"));
    assert!(result.content.contains("### reviewer1"));
    assert!(result.content.contains("First follow-up from the thread."));
}

#[test]
fn github_pr_visible_discussion_includes_review_comments_without_duplicating_main_body() {
    let result = html_output_at_url(
        "https://github.com/example/repo/pull/7",
        r#"
        <html>
          <body>
            <div class="gh-header-meta">
              <a class="author" href="/octocat">octocat</a>
            </div>
            <relative-time datetime="2026-03-04T05:06:07Z"></relative-time>
            <div class="timeline-comment">
              <a class="author" href="/octocat">octocat</a>
              <relative-time datetime="2026-03-04T05:06:07Z"></relative-time>
              <div class="comment-body markdown-body">
                <p>Main PR body.</p>
              </div>
            </div>
            <div class="timeline-comment">
              <a class="author" href="/reviewer1">reviewer1</a>
              <relative-time datetime="2026-03-05T01:02:03Z"></relative-time>
              <div class="comment-body markdown-body">
                <p>Looks good overall.</p>
              </div>
            </div>
            <div class="review-comment">
              <a class="author" href="/reviewer2">reviewer2</a>
              <relative-time datetime="2026-03-05T12:00:00Z"></relative-time>
              <div class="comment-body markdown-body">
                <p>Please add a regression test.</p>
                <pre><code class="language-rust">assert!(stable);</code></pre>
              </div>
            </div>
          </body>
        </html>
        "#,
    );

    assert_eq!(result.content.matches("Main PR body.").count(), 1);
    assert!(result.content.contains("## Discussion"));
    assert!(result.content.contains("Looks good overall."));
    assert!(result.content.contains("Please add a regression test."));
    assert!(result.content.contains("```rust\nassert!(stable);\n```"));
}

#[test]
fn github_clipboard_wrapper_does_not_hide_markdown_body() {
    let result = html_output(
        r#"
        <html>
          <body>
            <main>
              <div class="js-snippet-clipboard-copy-unpositioned DirectoryRichtextContent-module__SharedMarkdownContent__hHXUL">
                <article class="markdown-body">
                  <h1>Example README</h1>
                  <p>This README should survive even when wrapped by clipboard-related classes.</p>
                </article>
              </div>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(
        result
            .content
            .contains("This README should survive even when wrapped by clipboard-related classes.")
    );
    assert!(!result.warnings.contains(&Warning::LowSignalExtraction));
}

#[test]
fn github_repo_overview_extracts_entries_and_primary_readme() {
    let result = html_output_at_url(
        "https://github.com/example/repo",
        r#"
        <html>
          <head>
            <title>example/repo</title>
          </head>
          <body>
            <script type="application/json" data-target="react-app.embeddedData">
              {
                "payload": {
                  "codeViewRepoRoute": {
                    "tree": {
                      "items": [
                        { "name": "src", "path": "src", "contentType": "directory" },
                        { "name": "README.md", "path": "README.md", "contentType": "file" }
                      ]
                    },
                    "overview": {
                      "overviewFiles": [
                        {
                          "displayName": "README.md",
                          "preferredFileType": "readme",
                          "richText": "<article class=\"markdown-body\"><h1>repo readme</h1><p>Repository overview text from the embedded README.</p></article>"
                        }
                      ]
                    }
                  }
                }
              }
            </script>
            <div id="repo-content-pjax-container"></div>
          </body>
        </html>
        "#,
    );

    assert!(result.content.contains("## Top-level entries"));
    assert!(result.content.contains("- src/"));
    assert!(result.content.contains("- README.md"));
    assert!(
        result
            .content
            .contains("Repository overview text from the embedded README.")
    );
    assert!(!result.warnings.contains(&Warning::LowSignalExtraction));
}

#[test]
fn github_tree_extracts_entries_and_directory_readme() {
    let result = html_output_at_url(
        "https://github.com/example/repo/tree/main/docs",
        r#"
        <html>
          <body>
            <script type="application/json" data-target="react-app.embeddedData">
              {
                "payload": {
                  "codeViewTreeRoute": {
                    "tree": {
                      "items": [
                        { "name": "guide.md", "path": "docs/guide.md", "contentType": "file" },
                        { "name": "images", "path": "docs/images", "contentType": "directory" }
                      ],
                      "readme": {
                        "richText": "<article class=\"markdown-body\"><p>Directory README content from the tree payload.</p></article>"
                      }
                    }
                  }
                }
              }
            </script>
          </body>
        </html>
        "#,
    );

    assert!(result.content.contains("## Directory entries"));
    assert!(result.content.contains("- guide.md"));
    assert!(result.content.contains("- images/"));
    assert!(
        result
            .content
            .contains("Directory README content from the tree payload.")
    );
}

#[test]
fn github_blob_renders_embedded_markdown() {
    let result = html_output_at_url(
        "https://github.com/example/repo/blob/main/README.md",
        r#"
        <html>
          <body>
            <script type="application/json" data-target="react-app.embeddedData">
              {
                "payload": {
                  "codeViewBlobRoute": {
                    "richText": "<article class=\"markdown-body\"><h1>Blob Title</h1><p>Rendered markdown blob text.</p></article>"
                  }
                }
              }
            </script>
          </body>
        </html>
        "#,
    );

    assert!(result.content.contains("Rendered markdown blob text."));
    assert!(!result.content.contains("```"));
}

#[test]
fn github_blob_falls_back_to_raw_lines_when_rich_text_is_missing() {
    let result = html_output_at_url(
        "https://github.com/example/repo/blob/main/src/main.rs",
        r#"
        <html>
          <body>
            <script type="application/json" data-target="react-app.embeddedData">
              {
                "payload": {
                  "codeViewBlobLayoutRoute": {
                    "blob": {
                      "language": "Rust"
                    }
                  },
                  "codeViewBlobLayoutRoute.StyledBlob": {
                    "rawLines": [
                      "fn main() {",
                      "    println!(\"hi\");",
                      "}"
                    ]
                  }
                }
              }
            </script>
          </body>
        </html>
        "#,
    );

    assert!(
        result
            .content
            .contains("```rust\nfn main() {\n    println!(\"hi\");\n}\n```")
    );
}

#[test]
fn github_releases_page_uses_only_the_first_visible_release() {
    let result = html_output_at_url(
        "https://github.com/example/repo/releases",
        r#"
        <html>
          <body>
            <section aria-labelledby="release-1">
              <h2 id="release-1">Release 1</h2>
              <relative-time datetime="2026-03-01T00:00:00Z"></relative-time>
              <a class="color-fg-muted wb-break-all" href="/octocat">octocat</a>
              <span class="tmp-mr-3 f1 text-bold d-inline">
                <a href="/example/repo/releases/tag/v1.2.3">Release v1.2.3</a>
              </span>
              <div data-test-selector="body-content" class="markdown-body">
                <p>First release notes stay visible.</p>
              </div>
              <a href="/example/repo/releases/download/v1.2.3/app.tar.gz">app.tar.gz</a>
            </section>
            <section aria-labelledby="release-2">
              <h2 id="release-2">Release 2</h2>
              <relative-time datetime="2026-02-01T00:00:00Z"></relative-time>
              <a class="color-fg-muted wb-break-all" href="/someone">someone</a>
              <span class="tmp-mr-3 f1 text-bold d-inline">
                <a href="/example/repo/releases/tag/v1.2.2">Release v1.2.2</a>
              </span>
              <div data-test-selector="body-content" class="markdown-body">
                <p>Older release notes should not be included.</p>
              </div>
              <a href="/example/repo/releases/download/v1.2.2/old.tar.gz">old.tar.gz</a>
            </section>
          </body>
        </html>
        "#,
    );

    assert!(result.content.contains("Title: Release v1.2.3"));
    assert!(result.content.contains("Author: octocat"));
    assert!(result.content.contains("Published: 2026-03-01T00:00:00Z"));
    assert!(result.content.contains("First release notes stay visible."));
    assert!(result.content.contains("## Assets"));
    assert!(result.content.contains("- app.tar.gz"));
    assert!(
        !result
            .content
            .contains("Older release notes should not be included.")
    );
    assert!(!result.content.contains("old.tar.gz"));
}

#[test]
fn github_release_tag_page_extracts_visible_body_and_assets() {
    let result = html_output_at_url(
        "https://github.com/example/repo/releases/tag/v1.2.3",
        r#"
        <html>
          <body>
            <a href="/example/repo/releases/tag/v1.2.3">Release v1.2.3</a>
            <div class="tmp-mb-3">
              <a class="text-bold color-fg-muted" href="/apps/github-actions">github-actions</a>
              <relative-time datetime="2026-02-03T04:05:06Z"></relative-time>
            </div>
            <div data-test-selector="body-content" class="markdown-body">
              <p>Tagged release notes from the dedicated release page.</p>
            </div>
            <a href="/example/repo/releases/download/v1.2.3/app-macos.tar.gz">app-macos.tar.gz</a>
            <a href="/example/repo/releases/download/v1.2.3/app-linux.tar.gz">app-linux.tar.gz</a>
          </body>
        </html>
        "#,
    );

    assert!(result.content.contains("Title: Release v1.2.3"));
    assert!(result.content.contains("Author: github-actions"));
    assert!(result.content.contains("Published: 2026-02-03T04:05:06Z"));
    assert!(
        result
            .content
            .contains("Tagged release notes from the dedicated release page.")
    );
    assert!(result.content.contains("- app-macos.tar.gz"));
    assert!(result.content.contains("- app-linux.tar.gz"));
}

#[test]
fn unknown_github_surfaces_fall_back_to_generic_html_extraction() {
    let result = html_output_at_url(
        "https://github.com/example/repo/wiki",
        r#"
        <html>
          <body>
            <main>
              <h1>Wiki Home</h1>
              <p>Generic wiki content should still be extracted by the fallback path.</p>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(
        result
            .content
            .contains("Generic wiki content should still be extracted by the fallback path.")
    );
}

#[test]
fn json_sniffing_works_for_generic_content_type() {
    let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
    let transport = FakeTransport::new(HashMap::from([(
        ("http://example.test/data".to_string(), socket(80)),
        Ok(response(
            "http://example.test/data",
            200,
            Some("application/octet-stream"),
            br#"{"hello":"world"}"#,
        )),
    )]));
    let result = fetch_with_clients(
        "http://example.test/data",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert!(result.ok);
    assert_eq!(result.extraction_kind, super::ExtractionKind::Json);
    assert_eq!(
        result.content_type.as_deref(),
        Some("application/octet-stream")
    );
    assert!(result.content.contains("\"hello\": \"world\""));
}

#[test]
fn utf8_unicode_octet_stream_is_treated_as_text() {
    let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
    let transport = FakeTransport::new(HashMap::from([(
        ("http://example.test/unicode".to_string(), socket(80)),
        Ok(response(
            "http://example.test/unicode",
            200,
            Some("application/octet-stream"),
            "こんにちは世界".as_bytes(),
        )),
    )]));
    let result = fetch_with_clients(
        "http://example.test/unicode",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert!(result.ok);
    assert_eq!(result.extraction_kind, super::ExtractionKind::Text);
    assert_eq!(
        result.content_type.as_deref(),
        Some("application/octet-stream")
    );
    assert!(result.content.contains("こんにちは世界"));
}

#[test]
fn octet_stream_with_nul_bytes_stays_binary() {
    let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
    let transport = FakeTransport::new(HashMap::from([(
        ("http://example.test/binary".to_string(), socket(80)),
        Ok(response(
            "http://example.test/binary",
            200,
            Some("application/octet-stream"),
            b"hello\0world",
        )),
    )]));
    let result = fetch_with_clients(
        "http://example.test/binary",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert!(result.ok);
    assert_eq!(result.extraction_kind, super::ExtractionKind::BinarySummary);
    assert!(result.content.contains("[BINARY CONTENT]"));
}

#[test]
fn large_generic_json_body_above_sniff_cap_stays_text() {
    let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
    let payload = format!(r#"{{"payload":"{}"}}"#, "x".repeat(300_000));
    let transport = FakeTransport::new(HashMap::from([(
        ("http://example.test/large-json".to_string(), socket(80)),
        Ok(response(
            "http://example.test/large-json",
            200,
            Some("application/octet-stream"),
            payload.as_bytes(),
        )),
    )]));
    let result = fetch_with_clients(
        "http://example.test/large-json",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert!(result.ok);
    assert_eq!(result.extraction_kind, super::ExtractionKind::Text);
    assert_eq!(
        result.content_type.as_deref(),
        Some("application/octet-stream")
    );
    assert!(result.content.starts_with("{\"payload\":\""));
}

#[test]
fn explicit_json_content_type_still_renders_json_when_large() {
    let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
    let payload = format!(r#"{{"payload":"{}"}}"#, "x".repeat(300_000));
    let transport = FakeTransport::new(HashMap::from([(
        (
            "http://example.test/large-explicit-json".to_string(),
            socket(80),
        ),
        Ok(response(
            "http://example.test/large-explicit-json",
            200,
            Some("application/json"),
            payload.as_bytes(),
        )),
    )]));
    let result = fetch_with_clients(
        "http://example.test/large-explicit-json",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert!(result.ok);
    assert_eq!(result.extraction_kind, super::ExtractionKind::Json);
    assert_eq!(result.content_type.as_deref(), Some("application/json"));
    assert!(result.content.contains("\"payload\": \""));
}

#[test]
fn oversized_json_skips_pretty_printing() {
    let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
    let payload = format!(r#"{{"data":"{}"}}"#, "y".repeat(2_500_000));
    let transport = FakeTransport::new(HashMap::from([(
        ("http://example.test/oversized-json".to_string(), socket(80)),
        Ok(response(
            "http://example.test/oversized-json",
            200,
            Some("application/json"),
            payload.as_bytes(),
        )),
    )]));
    let result = fetch_with_clients(
        "http://example.test/oversized-json",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert!(result.ok);
    assert_eq!(result.extraction_kind, super::ExtractionKind::Json);
    assert!(result.truncated);
    assert!(result.error.is_none());
    // Raw JSON (no spaces around colon) since pretty-printing was skipped
    assert!(result.content.contains("\"data\":\""));
    assert!(!result.content.contains("\"data\": \""));
}

#[test]
fn oversized_invalid_explicit_json_returns_decode_error() {
    let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
    let payload = format!(r#"{{"data":"{}"#, "y".repeat(2_500_000));
    let transport = FakeTransport::new(HashMap::from([(
        (
            "http://example.test/oversized-invalid-json".to_string(),
            socket(80),
        ),
        Ok(response(
            "http://example.test/oversized-invalid-json",
            200,
            Some("application/json"),
            payload.as_bytes(),
        )),
    )]));
    let result = fetch_with_clients(
        "http://example.test/oversized-invalid-json",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert!(!result.ok);
    assert_eq!(result.extraction_kind, super::ExtractionKind::Error);
    assert_eq!(result.error.as_deref(), Some("decode_error"));
    assert!(result.truncated);
    assert!(result.content.contains("\"data\":\""));
}

#[test]
fn xml_content_types_are_treated_as_text() {
    let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
    let transport = FakeTransport::new(HashMap::from([(
        ("http://example.test/feed".to_string(), socket(80)),
        Ok(response(
            "http://example.test/feed",
            200,
            Some("application/xml"),
            br#"<feed><title>News</title></feed>"#,
        )),
    )]));
    let result = fetch_with_clients(
        "http://example.test/feed",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert!(result.ok);
    assert_eq!(result.extraction_kind, super::ExtractionKind::Text);
    assert_eq!(result.content_type.as_deref(), Some("application/xml"));
    assert!(result.content.contains("<feed><title>News</title></feed>"));
}

#[test]
fn text_like_application_content_is_readable() {
    let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
    let transport = FakeTransport::new(HashMap::from([(
        ("http://example.test/script".to_string(), socket(80)),
        Ok(response(
            "http://example.test/script",
            200,
            Some("application/javascript"),
            br#"console.log("hello");"#,
        )),
    )]));
    let result = fetch_with_clients(
        "http://example.test/script",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert!(result.ok);
    assert_eq!(result.extraction_kind, super::ExtractionKind::Text);
    assert!(result.content.contains("console.log(\"hello\");"));
}

#[test]
fn text_plain_python_preserves_indentation() {
    let body = "def create_app(test_config=None):\n    if test_config is None:\n        return \"default\"\n    return test_config\n";
    let result = fetch_body(
        "https://example.test/app.py",
        Some("text/plain"),
        body.as_bytes(),
    );

    assert!(result.ok);
    assert_eq!(result.extraction_kind, super::ExtractionKind::Text);
    assert_eq!(result.content, body);
}

#[test]
fn text_plain_yaml_preserves_nested_spacing() {
    let body = "jobs:\n  build:\n    steps:\n      - run: cargo test\n\n  lint:\n    steps:\n      - run: cargo clippy\n";
    let result = fetch_body(
        "https://example.test/workflow.yml",
        Some("text/plain"),
        body.as_bytes(),
    );

    assert!(result.ok);
    assert_eq!(result.content, body);
}

#[test]
fn text_plain_makefile_preserves_tab_indentation() {
    let body = "all:\n\tcargo test\n";
    let result = fetch_body(
        "https://example.test/Makefile",
        Some("text/plain"),
        body.as_bytes(),
    );

    assert!(result.ok);
    assert_eq!(result.content, body);
}

#[test]
fn text_plain_markdown_preserves_blank_lines_and_nested_lists() {
    let body = "# Title\n\n- item\n  - nested\n\n```rust\nfn main() {}\n```\n";
    let result = fetch_body(
        "https://example.test/README.md",
        Some("text/plain"),
        body.as_bytes(),
    );

    assert!(result.ok);
    assert_eq!(result.content, body);
}

#[test]
fn html_charset_decoding_handles_windows_1252() {
    let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
    let body = b"<html><head><title>Caf\xe9</title></head><body><main><p>\x93Quoted\x94 caf\xe9 costs \x8010.</p></main></body></html>";
    let transport = FakeTransport::new(HashMap::from([(
        ("http://example.test/latin1".to_string(), socket(80)),
        Ok(response(
            "http://example.test/latin1",
            200,
            Some("text/html; charset=windows-1252"),
            body,
        )),
    )]));
    let result = fetch_with_clients(
        "http://example.test/latin1",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );

    assert!(result.content.contains("Title: Café"));
    assert!(result.content.contains("“Quoted” café costs €10."));
}

#[test]
fn binary_summary_uses_content_length_when_present() {
    let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
    let transport = FakeTransport::new(HashMap::from([(
        ("http://example.test/file.pdf".to_string(), socket(80)),
        Ok(TransportResponse {
            content_length: Some(182_344),
            ..response(
                "http://example.test/file.pdf",
                200,
                Some("application/pdf"),
                b"%PDF-1.7",
            )
        }),
    )]));
    let result = fetch_with_clients(
        "http://example.test/file.pdf",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert!(result.ok);
    assert_eq!(result.extraction_kind, super::ExtractionKind::BinarySummary);
    assert!(result.content.contains("bytes: 182344"));
}

#[test]
fn declared_oversized_response_returns_content_too_large_and_uses_browser_headers() {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        super::MAX_DOWNLOAD_BYTES + 1
    );
    let (address, request, handle) = spawn_http_test_server(response);
    let transport = UreqTransport;
    let failure = transport
        .get(
            &Url::parse("http://example.test/large").expect("url"),
            address,
            Duration::from_secs(5),
        )
        .expect_err("oversized response should fail");
    handle.join().expect("server thread");

    assert_eq!(failure.kind, TransportFailureKind::TooLarge);
    assert_eq!(failure.context.status, Some(200));
    assert_eq!(failure.context.content_type.as_deref(), Some("text/html"));
    assert_eq!(
        failure.context.final_url.as_deref(),
        Some("http://example.test/large")
    );

    let request = request.lock().expect("captured request");
    assert!(request.contains("User-Agent: Mozilla/5.0"));
    assert!(request.contains("Chrome/135.0.0.0"));
    assert!(request.contains("Accept-Language: en-US,en;q=0.9"));
}

#[test]
fn low_yield_shell_pages_are_flagged() {
    let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
    let body =
        br#"<html><head><title>App</title></head><body><div id="__next"></div></body></html>"#;
    let transport = FakeTransport::new(HashMap::from([(
        ("http://example.test/app".to_string(), socket(80)),
        Ok(response(
            "http://example.test/app",
            200,
            Some("text/html"),
            body,
        )),
    )]));
    let result = fetch_with_clients(
        "http://example.test/app",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert!(result.warnings.contains(&Warning::LowContentYield));
    assert!(result.warnings.contains(&Warning::LowSignalExtraction));
    assert!(result.warnings.contains(&Warning::PossibleJsRenderedPage));
}

#[test]
fn metadata_only_pages_are_marked_low_signal_without_js_warning() {
    let result = html_output(
        r#"
        <html>
          <head>
            <title>Only metadata</title>
            <meta name="description" content="Summary" />
          </head>
          <body></body>
        </html>
        "#,
    );

    assert!(result.warnings.contains(&Warning::LowContentYield));
    assert!(result.warnings.contains(&Warning::LowSignalExtraction));
    assert!(!result.warnings.contains(&Warning::PossibleJsRenderedPage));
}

#[test]
fn normal_docs_pages_do_not_gain_low_signal_warning() {
    let result = html_output(
        r#"
        <html>
          <body>
            <main>
              <h1>Guide</h1>
              <p>This guide explains how to install, configure, and run the service safely.</p>
              <p>It also covers troubleshooting, logging, metrics, and deployment guidance.</p>
            </main>
          </body>
        </html>
        "#,
    );

    assert!(!result.warnings.contains(&Warning::LowSignalExtraction));
}

#[test]
fn http_errors_keep_body_snippets_and_mark_error() {
    let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
    let body = br#"<html><body><main><h1>Missing</h1><p>Not here.</p></main></body></html>"#;
    let transport = FakeTransport::new(HashMap::from([(
        ("http://example.test/missing".to_string(), socket(80)),
        Ok(response(
            "http://example.test/missing",
            404,
            Some("text/html"),
            body,
        )),
    )]));
    let result = fetch_with_clients(
        "http://example.test/missing",
        REQUEST_TIMEOUT,
        &resolver,
        &transport,
    );
    assert_eq!(result.error.as_deref(), Some("http_404"));
    assert_eq!(result.extraction_kind, super::ExtractionKind::Error);
    assert!(result.content.contains("Missing"));
}

#[test]
fn render_cli_output_shows_header_block() {
    let rendered = render_cli_output(&FetchResult {
        ok: false,
        requested_url: "https://example.com".to_string(),
        final_url: None,
        status: None,
        content_type: None,
        content: String::new(),
        extraction_kind: super::ExtractionKind::Error,
        warnings: vec![Warning::LowSignalExtraction],
        error: Some("timeout".to_string()),
        truncated: false,
        bytes_read: 0,
    });
    assert!(rendered.contains("URL:             https://example.com"));
    assert!(rendered.contains("Warnings:        LowSignalExtraction"));
    assert!(rendered.contains("Error:           timeout"));
    assert!(rendered.contains("(no content)"));
}

#[test]
fn public_fetch_function_keeps_example_https_smoke_shape() {
    let result = fetch_url_with_timeout("notaurl", Duration::from_secs(1));
    assert!(!result.ok);
    assert_eq!(result.error.as_deref(), Some("invalid_url"));
}

#[test]
fn resolver_pool_limits_concurrent_blocking_dns_work() {
    let backend = Arc::new(BlockingResolveBackend::new());
    let pool = ResolverPool::new(2, 4, backend.clone());
    let handles = (0..6)
        .map(|_| {
            let pool = pool.clone();
            thread::spawn(move || {
                pool.resolve("example.test", 80, Duration::from_millis(300))
                    .expect("dns result")
            })
        })
        .collect::<Vec<_>>();

    wait_until(Duration::from_millis(100), || {
        backend.max_active.load(Ordering::SeqCst) == 2
    });
    backend.release_all();

    for handle in handles {
        assert_eq!(handle.join().expect("join"), vec![socket(80)]);
    }
    assert_eq!(backend.max_active.load(Ordering::SeqCst), 2);
}

#[test]
fn resolver_pool_times_out_when_the_queue_is_full() {
    let backend = Arc::new(BlockingResolveBackend::new());
    let pool = ResolverPool::new(1, 1, backend.clone());

    let first_pool = pool.clone();
    let first = thread::spawn(move || {
        first_pool
            .resolve("example.test", 80, Duration::from_millis(500))
            .expect("first dns result")
    });
    wait_until(Duration::from_millis(100), || {
        backend.active.load(Ordering::SeqCst) == 1
    });

    let second_pool = pool.clone();
    let second = thread::spawn(move || {
        second_pool
            .resolve("example.test", 80, Duration::from_millis(500))
            .expect("second dns result")
    });
    wait_until(Duration::from_millis(100), || pool.queued_requests() == 1);

    let error = pool
        .resolve("example.test", 80, Duration::from_millis(20))
        .expect_err("queue timeout");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);

    backend.release_all();
    assert_eq!(first.join().expect("join"), vec![socket(80)]);
    assert_eq!(second.join().expect("join"), vec![socket(80)]);
}

#[test]
fn resolver_pool_drops_late_dns_completions() {
    let backend = Arc::new(BlockingResolveBackend::new());
    let pool = ResolverPool::new(1, 1, backend.clone());

    let error = pool
        .resolve("example.test", 80, Duration::from_millis(20))
        .expect_err("timeout");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(backend.completed.load(Ordering::SeqCst), 0);

    backend.release_all();
    wait_until(Duration::from_millis(100), || {
        backend.completed.load(Ordering::SeqCst) == 1
    });

    assert_eq!(
        pool.resolve("example.test", 80, Duration::from_millis(50))
            .expect("post-timeout resolve"),
        vec![socket(80)]
    );
}

#[test]
fn github_repo_fixture_extracts_real_page_overview() {
    let result = html_output_at_url("https://github.com/rust-lang/rust", GITHUB_REPO_FIXTURE);

    assert!(result.content.contains("## Top-level entries"));
    assert!(result.content.contains("- compiler/"));
    assert!(
        result
            .content
            .contains("This is the main source code repository for")
    );
    assert!(
        result
            .content
            .contains("[Rust](<https://www.rust-lang.org/>)")
    );
}

#[test]
fn github_tree_fixture_extracts_real_directory_entries() {
    let result = html_output_at_url(
        "https://github.com/rust-lang/rust/tree/main/compiler",
        GITHUB_TREE_FIXTURE,
    );

    assert!(result.content.contains("## Directory entries"));
    assert!(result.content.contains("- rustc/"));
    assert!(result.content.contains("- rustc_abi/"));
}

#[test]
fn github_blob_fixture_extracts_real_blob_markdown() {
    let result = html_output_at_url(
        "https://github.com/rust-lang/rust/blob/main/README.md",
        GITHUB_BLOB_FIXTURE,
    );

    assert!(
        result
            .content
            .contains("This is the main source code repository for")
    );
    assert!(
        result
            .content
            .contains("[Rust](<https://www.rust-lang.org/>)")
    );
    assert!(result.content.contains("## Why Rust?"));
}

#[test]
fn github_issue_fixture_targets_the_current_issue_number() {
    let result = html_output_at_url(
        "https://github.com/rust-lang/rust/issues/1",
        GITHUB_ISSUE_FIXTURE,
    );

    assert!(
        result
            .content
            .contains("The IL module doesn't take a session variable")
    );
    assert!(result.content.contains("Author: graydon"));
    assert!(
        !result
            .content
            .contains("Wrong issue body from a related item.")
    );
}

#[test]
fn github_pull_fixture_targets_the_current_pull_number() {
    let result = html_output_at_url(
        "https://github.com/rust-lang/rust/pull/140167",
        GITHUB_PULL_FIXTURE,
    );

    assert!(result.content.contains("I tried this code:"));
    assert!(result.content.contains("Author: bjorn3"));
    assert!(
        !result
            .content
            .contains("Wrong pull body from a related item.")
    );
}

#[test]
fn github_releases_fixture_extracts_the_first_real_release_section() {
    let result = html_output_at_url(
        "https://github.com/rust-lang/rust/releases",
        GITHUB_RELEASES_FIXTURE,
    );

    assert!(result.content.contains("Title: 1.94.1"));
    assert!(result.content.contains("Author: rustbot"));
    assert!(result.content.contains("std::thread::spawn"));
    assert!(result.content.contains("wasm32-wasip1-threads"));
}

#[test]
fn prefers_primary_root_when_body_only_adds_layout_copy() {
    let layout_copy = "Layout chrome that should stay outside the selected content. ".repeat(250);
    let html = format!(
        r#"
        <html>
          <body>
            <main>
              <h1>Guide</h1>
              <p>This guide explains how to install, configure, and run the service safely.</p>
              <p>It also covers troubleshooting, logging, metrics, deployment guidance, and recovery workflows.</p>
            </main>
            <div>{layout_copy}</div>
          </body>
        </html>
        "#
    );

    let result = html_output(&html);

    assert_eq!(result.kind, super::ExtractionKind::HtmlPrimary);
    assert!(result.content.contains("Title: Guide"));
    assert!(result.warnings.contains(&Warning::LowContentYield));
    assert!(
        !result
            .content
            .contains("Layout chrome that should stay outside the selected content.")
    );
}

#[test]
fn live_canary_manifest_has_unique_ids_and_expected_tier_sizes() {
    let mut ids = HashSet::new();
    let mut gating = 0usize;
    let mut observational = 0usize;
    let mut self_hosted_edge = 0usize;

    for case in live_canaries::cases() {
        assert!(
            ids.insert(case.id.clone()),
            "duplicate canary id `{}`",
            case.id
        );
        match case.tier {
            CanaryTier::Gating => gating += 1,
            CanaryTier::Observational => observational += 1,
            CanaryTier::SelfHostedEdge => self_hosted_edge += 1,
        }
    }

    assert!((4..=6).contains(&gating));
    assert!((15..=30).contains(&observational));
    assert!(self_hosted_edge >= 9);
}

#[test]
fn live_canary_manifest_contains_openai_docs_gating_case() {
    let case = live_canaries::cases()
        .iter()
        .find(|case| case.id == "gating_openai_docs_function_calling")
        .expect("openai docs canary");

    assert_eq!(case.tier, CanaryTier::Gating);
    assert_eq!(
        case.url,
        "https://developers.openai.com/api/docs/guides/function-calling"
    );
    assert_eq!(
        case.expected_extraction_kind,
        super::ExtractionKind::HtmlPrimary
    );
    assert_eq!(case.expected_status, Some(200));
    assert_eq!(
        case.expected_content_type_prefix.as_deref(),
        Some("text/html")
    );
    assert_eq!(case.min_content_chars, 1200);
    assert!(
        case.required_markers
            .iter()
            .any(|marker| marker == "### Tool choice")
    );
    assert!(
        case.forbidden_warnings
            .contains(&Warning::LowSignalExtraction)
    );
    assert!(
        case.forbidden_warnings
            .contains(&Warning::PossibleJsRenderedPage)
    );
}

#[test]
#[ignore]
fn web_fetch_live_canaries_gating() {
    run_live_canaries(CanaryTier::Gating);
}

#[test]
#[ignore]
fn web_fetch_live_canaries_observational() {
    run_live_canaries(CanaryTier::Observational);
}

#[test]
#[ignore]
fn web_fetch_live_canaries_self_hosted_edge() {
    run_live_canaries(CanaryTier::SelfHostedEdge);
}

fn run_live_canaries(target_tier: CanaryTier) {
    if let Some(requested_tier) = live_canaries::requested_tier_filter()
        && requested_tier != target_tier
    {
        eprintln!(
            "skipping `{}` because `{}` requested tier `{}`",
            target_tier.as_str(),
            live_canaries::TIER_ENV,
            requested_tier.as_str()
        );
        return;
    }

    let cases = live_canaries::cases_for_tier(target_tier);
    assert!(
        !cases.is_empty(),
        "no live canary cases matched tier `{}`{}",
        target_tier.as_str(),
        live_canaries::requested_case_filter()
            .map(|value| format!(" and case id `{value}`"))
            .unwrap_or_default()
    );

    let mut failures = Vec::new();
    for case in cases {
        let requested_url = live_canaries::resolve_case_url(case)
            .unwrap_or_else(|error| panic!("failed to resolve canary `{}`: {}", case.id, error));
        eprintln!(
            "running live canary `{}` [{}] -> {}",
            case.id,
            target_tier.as_str(),
            requested_url
        );

        let result = fetch_url_with_timeout(&requested_url, Duration::from_secs(20));
        if let Err(failure) = assert_live_canary(case, &requested_url, &result) {
            failures.push(failure);
            continue;
        }

        eprintln!(
            "passed live canary `{}` [{}]",
            case.id,
            target_tier.as_str()
        );
    }

    assert!(
        failures.is_empty(),
        "live canary failures ({}):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

fn assert_live_canary(
    case: &CanaryCase,
    requested_url: &str,
    result: &FetchResult,
) -> Result<(), String> {
    let mut mismatches = Vec::new();

    if result.ok != case.expect_ok {
        mismatches.push(format!(
            "expected ok={}, got ok={}",
            case.expect_ok, result.ok
        ));
    }

    if result.status != case.expected_status {
        mismatches.push(format!(
            "expected status {:?}, got {:?}",
            case.expected_status, result.status
        ));
    }

    if result.extraction_kind != case.expected_extraction_kind {
        mismatches.push(format!(
            "expected extraction {}, got {}",
            case.expected_extraction_kind.as_str(),
            result.extraction_kind.as_str()
        ));
    }

    if let Some(expected_error) = case.expected_error.as_deref()
        && result.error.as_deref() != Some(expected_error)
    {
        mismatches.push(format!(
            "expected error `{}`, got `{}`",
            expected_error,
            result.error.as_deref().unwrap_or("none")
        ));
    } else if case.expected_error.is_none() && result.error.is_some() && case.expect_ok {
        mismatches.push(format!(
            "expected no error, got `{}`",
            result.error.as_deref().unwrap_or("none")
        ));
    }

    if let Some(prefix) = case.expected_content_type_prefix.as_deref()
        && !result
            .content_type
            .as_deref()
            .unwrap_or("")
            .starts_with(prefix)
    {
        mismatches.push(format!(
            "expected content type prefix `{}`, got `{}`",
            prefix,
            result.content_type.as_deref().unwrap_or("-")
        ));
    }

    if let Some(expected_prefix) = live_canaries::resolve_expected_final_url_prefix(case)
        .unwrap_or_else(|error| {
            panic!(
                "failed to resolve final url prefix for `{}`: {}",
                case.id, error
            )
        })
        && !result
            .final_url
            .as_deref()
            .unwrap_or("")
            .starts_with(&expected_prefix)
    {
        mismatches.push(format!(
            "expected final url prefix `{}`, got `{}`",
            expected_prefix,
            result.final_url.as_deref().unwrap_or("-")
        ));
    }

    let content_len = result.content.chars().count();
    if content_len < case.min_content_chars {
        mismatches.push(format!(
            "expected at least {} content chars, got {}",
            case.min_content_chars, content_len
        ));
    }

    for marker in &case.required_markers {
        if !result.content.contains(marker) {
            mismatches.push(format!("missing required marker `{marker}`"));
        }
    }

    for marker in &case.forbidden_markers {
        if result.content.contains(marker) {
            mismatches.push(format!("found forbidden marker `{marker}`"));
        }
    }

    for warning in &case.required_warnings {
        if !result.warnings.contains(warning) {
            mismatches.push(format!("missing required warning `{}`", warning.as_str()));
        }
    }

    for warning in &case.forbidden_warnings {
        if result.warnings.contains(warning) {
            mismatches.push(format!("found forbidden warning `{}`", warning.as_str()));
        }
    }

    if mismatches.is_empty() {
        return Ok(());
    }

    Err(format_live_canary_failure(
        case,
        requested_url,
        result,
        &mismatches,
    ))
}

fn format_live_canary_failure(
    case: &CanaryCase,
    requested_url: &str,
    result: &FetchResult,
    mismatches: &[String],
) -> String {
    let warnings = if result.warnings.is_empty() {
        "none".to_string()
    } else {
        result
            .warnings
            .iter()
            .map(Warning::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    };

    format!(
        "[{}]\nrequested: {}\nfinal_url: {}\nstatus: {}\ncontent_type: {}\nextraction: {}\nwarnings: {}\nerror: {}\ncontent_chars: {}\nexcerpt:\n{}\n\nmismatches:\n- {}",
        case.id,
        requested_url,
        result.final_url.as_deref().unwrap_or("-"),
        result
            .status
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        result.content_type.as_deref().unwrap_or("-"),
        result.extraction_kind.as_str(),
        warnings,
        result.error.as_deref().unwrap_or("none"),
        result.content.chars().count(),
        content_excerpt(&result.content),
        mismatches.join("\n- ")
    )
}

fn content_excerpt(content: &str) -> String {
    let mut excerpt = String::new();
    for (index, ch) in content.chars().enumerate() {
        if index >= 600 {
            excerpt.push_str("\n...[truncated]...");
            break;
        }
        excerpt.push(ch);
    }
    excerpt
}
