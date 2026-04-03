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

fn html_output(body: &str) -> ExtractedContent {
    extract_html(None, Some("text/html"), body, false)
}

fn html_output_at_url(url: &str, body: &str) -> ExtractedContent {
    extract_html(Some(url), Some("text/html"), body, false)
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
    let extraction = extract_html(None, Some("text/html"), &html, false);

    assert!(extraction.truncated);
    assert!(extraction.warnings.contains(&Warning::ContentTruncated));
    assert!(extraction.content.ends_with("\n```"));
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
    assert_eq!(result.content_type.as_deref(), Some("application/json"));
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
