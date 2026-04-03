use std::{
    collections::HashMap,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use url::Url;

use super::{
    FetchResult, REQUEST_TIMEOUT, Warning,
    content::ExtractedContent,
    fetch_url_with_timeout,
    html::extract_html,
    render_cli_output,
    transport::{
        DnsResolver, HttpTransport, TransportFailure, TransportFailureKind, TransportResponse,
        fetch_with_clients,
    },
};

#[derive(Default)]
struct FakeResolver {
    values: HashMap<(String, u16), io::Result<Vec<SocketAddr>>>,
}

impl FakeResolver {
    fn with_mapping(mut self, host: &str, port: u16, addresses: Vec<SocketAddr>) -> Self {
        self.values.insert((host.to_string(), port), Ok(addresses));
        self
    }
}

impl DnsResolver for FakeResolver {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        match self.values.get(&(host.to_string(), port)) {
            Some(Ok(addresses)) => Ok(addresses.clone()),
            Some(Err(error)) => Err(io::Error::new(error.kind(), error.to_string())),
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
                url: Some(url.clone()),
            }),
            None => Err(TransportFailure {
                kind: TransportFailureKind::Connect,
                url: Some(url.clone()),
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

fn html_output(body: &str) -> ExtractedContent {
    extract_html(Some("text/html"), body.as_bytes(), false)
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
    assert!(complex.content.contains("A | B"));
    assert!(complex.content.contains("C"));
    assert!(!complex.content.contains("| --- |"));
}

#[test]
fn truncation_closes_fenced_code_blocks() {
    let long_code = "line\n".repeat(5000);
    let html = format!(
        "<html><body><main><pre><code>{}</code></pre></main></body></html>",
        long_code
    );
    let extraction = extract_html(Some("text/html"), html.as_bytes(), false);

    assert!(extraction.truncated);
    assert!(extraction.warnings.contains(&Warning::ContentTruncated));
    assert!(extraction.content.ends_with("\n```"));
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
    assert_eq!(result.content_type.as_deref(), Some("application/json"));
    assert!(result.content.contains("\"hello\": \"world\""));
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
