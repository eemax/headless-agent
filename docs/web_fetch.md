# Web Fetch Tool

The `web_fetch` tool is the repo's read-only HTTP fetcher. It accepts one URL, follows a bounded redirect chain, classifies the response body, and returns cleaned text plus structured metadata instead of throwing ordinary tool errors for normal fetch failures.

This page is the canonical deep dive for how `web_fetch` works today, what guarantees it tries to provide, and where to look when we need to change or extend it.

## Why This Tool Exists

`web_fetch` is meant to cover the common "go read this URL for me" cases that show up in coding and automation workflows:

- product/docs/help pages
- policies and changelogs
- JSON APIs
- plain-text files
- HTML pages that need readable extraction
- binary resources where a summary is still useful

The implementation is intentionally more opinionated than a raw HTTP client:

- it blocks local and private-network targets
- it performs its own DNS resolution and address selection
- it follows redirects manually so each hop is revalidated
- it extracts readable markdown-like text from HTML
- it emits structured failure payloads instead of surfacing most fetch issues as framework-level tool errors

## Entry Points And File Map

The tool lives under [`src/tools/web_fetch/`](../src/tools/web_fetch/).

- [`src/tools/web_fetch/mod.rs`](../src/tools/web_fetch/mod.rs): public tool surface, shared constants, result types, CLI rendering
- [`src/tools/web_fetch/transport.rs`](../src/tools/web_fetch/transport.rs): URL validation, DNS resolution, SSRF guards, HTTP transport, redirects, body download
- [`src/tools/web_fetch/content.rs`](../src/tools/web_fetch/content.rs): content sniffing, charset detection, JSON/text/binary extraction
- [`src/tools/web_fetch/html.rs`](../src/tools/web_fetch/html.rs): HTML metadata extraction, root selection, schema fallback, warning assignment
- [`src/tools/web_fetch/render.rs`](../src/tools/web_fetch/render.rs): DOM-to-markdown-ish renderer
- [`src/tools/web_fetch/sites/mod.rs`](../src/tools/web_fetch/sites/mod.rs): site-specific extractor registry
- [`src/tools/web_fetch/sites/github.rs`](../src/tools/web_fetch/sites/github.rs): GitHub-specialized extraction
- [`src/tools/web_fetch/tests.rs`](../src/tools/web_fetch/tests.rs): main offline test corpus plus live canary runner
- [`src/tools/web_fetch/live_canaries.rs`](../src/tools/web_fetch/live_canaries.rs): live-canary manifest loader and tier helpers
- [`src/tools/web_fetch/fixtures/`](../src/tools/web_fetch/fixtures/): real-page fixtures and live canary manifest
- [`scripts/bench_webfetch.sh`](../scripts/bench_webfetch.sh): manual live benchmark comparing headless vs defuddle vs curl

Integration points outside the module:

- [`src/tools/mod.rs`](../src/tools/mod.rs): tool registration, dispatch, plan-mode behavior, retry policy
- [`src/app.rs`](../src/app.rs): CLI `headless webfetch <url>` entrypoint
- [`src/cli.rs`](../src/cli.rs): CLI parsing for the standalone `webfetch` command
- [`tests/tools_web_fetch.rs`](../tests/tools_web_fetch.rs): end-to-end tool contract check
- [`tests/cli_webfetch.rs`](../tests/cli_webfetch.rs): CLI behavior checks

## Public Surface

### Tool spec

The built-in tool name is `web_fetch`.

Input schema:

```json
{
  "type": "object",
  "properties": {
    "url": { "type": "string" }
  },
  "required": ["url"]
}
```

The tool is registered as read-only in [`src/tools/mod.rs`](../src/tools/mod.rs), so it:

- is allowed in plan mode
- does not require mutating access
- is not framework-retried on ordinary tool failure

That last point matters: the runtime treats `web_fetch` as single-attempt, but the tool itself has internal fallback behavior for DNS results and transport addresses before it gives up.

### CLI surface

There is also a direct CLI:

```bash
headless webfetch <url>
```

The CLI path uses [`render_cli_output`](../src/tools/web_fetch/mod.rs) to format the same `FetchResult` into a human-readable header block plus content body.

### Runtime budget behavior

The shared default request timeout is 10 seconds.

- Agent tool calls use `min(context.remaining_budget(), 10s)`.
- The direct CLI uses a fixed 10-second timeout.

So the effective timeout inside an agent run may be lower than 10 seconds if the overall run budget is nearly exhausted.

## Result Shape

The tool serializes [`FetchResult`](../src/tools/web_fetch/mod.rs) directly:

```json
{
  "ok": true,
  "requested_url": "https://example.com",
  "final_url": "https://example.com/docs",
  "status": 200,
  "content_type": "text/html",
  "content": "Title: Example\n\nUseful text...",
  "extraction_kind": "HtmlPrimary",
  "warnings": [],
  "error": null,
  "truncated": false,
  "bytes_read": 12345
}
```

Field meanings:

- `ok`: true only when the final HTTP status is 2xx and extraction did not report an extraction error
- `requested_url`: original input URL
- `final_url`: resolved final URL after redirects, when known
- `status`: final HTTP status, when known
- `content_type`: normalized media type, lowercased and stripped of parameters where appropriate
- `content`: extracted text payload or summary
- `extraction_kind`: one of `Json`, `Text`, `HtmlPrimary`, `HtmlFallback`, `BinarySummary`, or `Error`
- `warnings`: non-fatal extraction signals
- `error`: structured error code, or `null`
- `truncated`: whether the downloaded or rendered content was truncated
- `bytes_read`: bytes actually read from the response body

Important behavior:

- Invalid URLs, blocked addresses, DNS failures, timeouts, redirects, and HTTP 4xx/5xx responses all return a structured payload.
- Those cases do not become ordinary tool-dispatch errors unless something outside the normal fetch path breaks.

## Limits And Tunables

The current constants are defined in [`src/tools/web_fetch/mod.rs`](../src/tools/web_fetch/mod.rs), [`src/tools/web_fetch/transport.rs`](../src/tools/web_fetch/transport.rs), and [`src/tools/web_fetch/content.rs`](../src/tools/web_fetch/content.rs):

- request timeout: `10s`
- connect timeout: `3s`
- max redirects: `5`
- max download size: `4 MiB`
- max rendered content: `256 * 1024` (262,144) characters
- minimum "healthy" HTML visible content target: `200` characters
- max address attempts per resolved target: `4`
- DNS resolver worker count: `4`
- DNS resolver queue capacity: `64`
- max JSON sniff size for generic bodies: `256 KiB`
- max HTML shell-marker scan size: `64 KiB`

Two size limits are easy to confuse:

- download limit: caps how many bytes we read from the network
- rendered content limit: caps how much extracted text we return

Those limits fail or truncate at different stages.

## End-To-End Flow

High-level fetch lifecycle:

1. Parse and validate the requested URL.
2. Reject unsupported schemes and blocked hostnames/IPs.
3. Resolve the current host through the DNS worker pool.
4. Select up to four candidate addresses and try them in order.
5. Perform an HTTP GET with browser-like headers and redirects disabled in the HTTP library.
6. If the response is a redirect, resolve the new target again and repeat, up to five hops.
7. Download the body subject to the 4 MiB limit.
8. Classify the body as JSON, HTML, text, or binary.
9. Extract text content and warnings.
10. Convert HTTP and extraction outcomes into a `FetchResult`.

## Transport Layer

The transport layer lives in [`src/tools/web_fetch/transport.rs`](../src/tools/web_fetch/transport.rs).

### URL validation and scheme checks

`resolve_target()` parses the user input with `url::Url`.

Current hard failures:

- `invalid_url`: parse failure or missing host
- `unsupported_scheme`: anything other than `http` or `https`

### SSRF and local-network blocking

The tool rejects obvious local and private-network destinations before issuing a request.

Blocked hostnames:

- `localhost`
- `localhost.localdomain`
- any `*.localhost`
- `metadata.google.internal`

Blocked IPv4 ranges:

- `0.0.0.0/8`
- `10.0.0.0/8`
- `127.0.0.0/8`
- `169.254.0.0/16`
- `172.16.0.0/12`
- `192.168.0.0/16`
- `100.64.0.0/10`
- `198.18.0.0/15`
- multicast and higher reserved space (`224.0.0.0/4` and above)

Blocked IPv6 ranges and cases:

- loopback
- unspecified
- multicast
- unique-local (`fc00::/7`)
- link-local (`fe80::/10`)
- IPv4-mapped IPv6 addresses are normalized and rechecked against the IPv4 rules

If the hostname itself is blocked, or DNS resolution yields any blocked address, the fetch fails with `blocked_address`.

### DNS resolution model

The tool does not rely only on the HTTP client's own hostname resolution. It resolves names through a small shared worker pool:

- one global `ResolverPool`
- configurable worker count at construction, currently `4`
- bounded in-memory queue, currently `64`
- request-specific time budget derived from the overall deadline

Why it exists:

- lets the tool control DNS timeout behavior
- lets the tool inspect candidate addresses before connecting
- supports custom address retry logic
- keeps blocking system DNS work off the main fetch path

Failure mapping:

- timed-out DNS lookup -> `timeout`
- other DNS resolution failure -> `dns_error`

### Candidate address selection

After resolution, the tool deduplicates socket addresses and keeps at most four attempts.

Selection rules:

- if all candidates are from one IP family and already within the limit, keep them in order
- otherwise interleave IPv6 and IPv4 candidates, starting with the family returned first
- retry only on `connect` and `timeout` transport failures

This means the tool can recover from a bad first address without turning into an unbounded retry loop.

### HTTP client behavior

The concrete transport is `UreqTransport`.

Notable settings:

- proxy lookup from environment is disabled
- library-level redirect following is disabled
- connect, overall, read, and write timeouts are all bounded
- a custom resolver pins the request to the chosen socket address

Request headers are intentionally browser-like:

- `User-Agent`: a Chrome-like desktop browser UA
- `Accept`: prefers HTML, XHTML, JSON, and plain text, then falls back to `*/*`
- `Accept-Language`: `en-US,en;q=0.9`

The browser-like headers are not cosmetic. They materially improve the chances that docs/help sites return their primary human-readable content instead of thin bot fallback pages.

### Redirect handling

Redirects are handled manually for `301`, `302`, `303`, `307`, and `308`.

Why manual redirects matter:

- every redirect target is reparsed and revalidated
- every new hostname is rerun through DNS resolution
- every new resolved address is rerun through the blocking checks

Redirect failures return `redirect_error` when:

- the redirect chain exceeds five hops
- a redirect response has no `Location`
- the `Location` cannot be joined onto the current URL

### Download limits and oversize handling

There are two oversize paths:

1. If `Content-Length` is declared and already exceeds 4 MiB, the fetch aborts early with `content_too_large`.
2. Otherwise the body is streamed up to `4 MiB + 1 byte`; if more data exists, the extra byte is dropped, the body is marked truncated, and extraction continues.

That distinction is intentional:

- obviously too-large responses fail fast
- unknown-size streaming responses still yield partial, useful content when possible

## Content Classification And Decoding

The classification logic lives in [`src/tools/web_fetch/content.rs`](../src/tools/web_fetch/content.rs).

### Content-type normalization

`normalize_content_type()`:

- lowercases the media type
- strips parameters such as `charset=utf-8`

Example:

- `text/html; charset=UTF-8` -> `text/html`

### Sniffed content kinds

Internal classification buckets:

- JSON
- HTML
- text
- binary

Decision order is based on both headers and body sniffing.

JSON is selected when:

- the normalized content type is `application/json`
- the normalized content type ends with `+json`
- or the body parses as JSON and is at most 256 KiB when the content type is generic/ambiguous

HTML is selected when:

- the content type is `text/html` or `application/xhtml+xml`
- or the body looks like HTML based on common tags and prefixes

Text is selected when:

- the content type starts with `text/`
- the content type is XML or ends with `+xml`
- or the content type is `application/*` but the body still looks text-like and lacks obvious binary signatures

Binary is the fallback when the body does not pass the text/HTML/JSON tests.

### Charset handling

For text and HTML, decoding order is:

1. `charset=` from the `Content-Type` header
2. for HTML only, `<meta charset=...>` or equivalent within the first 4 KiB
3. UTF-8 if the bytes are valid UTF-8
4. Windows-1252 fallback

That Windows-1252 fallback is important for older HTML pages and some edge-case docs pages that are still not UTF-8 clean.

### JSON extraction

JSON handling tries to be helpful without pretending partial payloads are fully valid:

- JSON bodies up to 2 MiB are parsed and pretty-printed; larger bodies are returned as raw text to avoid excessive memory use
- UTF-8 BOM is stripped before parsing
- if pretty parsing fails, the raw text body is returned
- explicitly declared JSON remains `Json` even when large
- generic `application/octet-stream` content can still become `Json` if it parses and is within the sniff cap

### Text extraction

Plain text extraction is deliberately conservative:

- line endings are normalized
- indentation, blank lines, tabs, and nested structure are preserved
- content is truncated only at the 262,144-character output cap

This matters for files like:

- source code
- YAML
- Makefiles
- Markdown
- RFC text

### Binary extraction

Binary responses produce a summary instead of raw bytes:

```text
[BINARY CONTENT]
content_type: application/pdf
bytes: 182344
```

When `Content-Length` is available, the summary prefers it over the bytes actually downloaded so the caller sees the resource's real declared size.

## HTML Extraction Pipeline

The HTML extraction logic lives in [`src/tools/web_fetch/html.rs`](../src/tools/web_fetch/html.rs), with rendering support in [`src/tools/web_fetch/render.rs`](../src/tools/web_fetch/render.rs).

### Goals

The HTML path tries to do three things at once:

- preserve useful author/title/body metadata
- return readable markdown-like text instead of raw HTML
- avoid being fooled by navigation shells, JS app chrome, or tiny metadata-only pages

### Metadata extraction

The extractor builds a metadata block before the body.

Supported fields:

- `Title`
- `Description`
- `Author`
- `Published`

Source precedence is not identical for every field, but the broad pattern is:

- site-specific extraction first
- document metadata next
- schema.org metadata next
- nearby DOM hints last

Examples:

- title can come from the site extractor, `<title>`, Open Graph tags, Twitter tags, or schema.org
- author can come from the site extractor, schema.org, meta tags, or nearby `.author` / `.byline` content
- published can come from the site extractor, schema.org dates, meta tags, or `time[datetime]`

The metadata block is only included for fields that are actually present.

### Root candidate selection

Generic HTML extraction does not just render the entire `<body>`.

It gathers candidate roots from:

- `<main>`
- `<article>`
- `[role="main"]`
- `#content`
- `#main`
- `.content`
- `<body>` as the fallback candidate

Each candidate is rendered into markdown-ish text and scored using:

- visible text length
- text yield vs raw HTML size
- paragraph count
- list item count
- code block count
- table count
- link density
- "noisy" class/id penalties
- extra penalties for body-wide fallbacks
- extra penalties when the rendered output already looks low-signal

Nested candidates are deduplicated so a smaller high-signal child can beat its larger ancestor.

Returned extraction kind:

- `HtmlPrimary` when a non-body root wins, or a site extractor rescues the body
- `HtmlFallback` when the body/root fallback path is used

### Low-signal detection

`is_low_signal_extraction()` is the generic "this page mostly did not yield real content" check.

Pages under 2 KiB of raw HTML are never flagged (they are genuinely tiny, not failed extractions). For larger pages, the check requires at least 200 visible characters or 2% of the raw HTML size, whichever is larger. This single continuous threshold replaces the earlier multi-band approach and eliminates gap coverage between the old discrete conditions.

This feeds both candidate scoring and final warnings.

### Schema fallback

If generic DOM extraction looks weak, the extractor can rescue content from schema.org JSON-LD:

- `articleBody`
- `text`
- `headline`
- `description`
- author fields
- common published-date fields

Schema text is used only when it clears usefulness checks and beats the DOM under a small set of low-signal conditions. Healthy DOM extraction stays DOM-first.

### Site-specific rescue

The site extractor hook runs before generic body selection is finalized. Today that hook is used only for GitHub, but the mechanism is generic:

- site extractor can provide title/description/author/published/body
- site body wins when it is non-empty
- metadata from the site extractor takes precedence over generic metadata sources

This is why GitHub pages can return structured repo trees, issue discussions, and release assets even when the visible DOM is React-heavy or not ideal for generic extraction.

## DOM Renderer Behavior

The renderer converts HTML into markdown-like plain text.

Supported structures:

- headings
- paragraphs
- ordered and unordered lists
- nested lists with indentation
- blockquotes
- fenced code blocks
- inline code
- simple tables rendered as markdown tables
- complex tables downgraded to readable row text
- HTTP/HTTPS links rendered as markdown links

Notable rendering behaviors:

- duplicate top headings that match the metadata title are suppressed
- relative links are resolved against the source URL
- fragment-only and non-HTTP links stay as plain text
- inline code and code fences grow their backtick delimiters when the payload already contains backticks
- truncated rendered output closes any still-open code fence so the result stays valid markdown-ish text

### Noise removal

The renderer aggressively skips obvious chrome.

Always-noisy tags include:

- `script`
- `style`
- `noscript`
- `svg`
- `canvas`
- `iframe`
- `nav`
- `footer`
- `form`

Other noisy/hidden cases:

- `hidden`
- `aria-hidden="true"`
- `role="navigation"`, `banner`, or `complementary`
- inline styles hiding the element via `display:none`, `visibility:hidden`, or `opacity:0`
- utility classes like `hidden`, `invisible`, `sm:hidden`, and similar variants
- noisy class/id tokens such as `nav`, `menu`, `footer`, `sidebar`, `cookie`, `consent`, `modal`, `popup`, `share`, `social`, `breadcrumb`, `advert`, `promo`
- tiny UI crumbs such as `[edit]`, `toggle`, `expand description`, and `source`

The noisy-token matching is intentionally a little careful so it does not hide unrelated content just because a class name contains a substring in the middle of a larger word.

## GitHub-Specific Extraction

The GitHub extractor lives in [`src/tools/web_fetch/sites/github.rs`](../src/tools/web_fetch/sites/github.rs).

It only activates for:

- `github.com`
- `www.github.com`

Supported route families:

- repository overview
- directory tree views
- blob views
- issues
- pull requests
- releases
- latest release
- tagged release pages

Unknown GitHub paths fall back to generic HTML extraction.

### Common strategy

The extractor prefers GitHub's embedded React payload:

- `script[data-target='react-app.embeddedData']`

That payload is often more reliable than the visible DOM for repo trees, blobs, issues, PRs, and release metadata.

When the embedded payload is missing or incomplete, the extractor falls back to visible DOM selectors.

### Repo overview and tree pages

Repo overview output can include:

- a "Top-level entries" section built from the embedded tree listing
- the primary README/overview file rendered from rich text

Tree pages behave similarly, but use:

- a "Directory entries" section
- directory README content when present

### Blob pages

Blob extraction supports two paths:

1. render embedded rich text when GitHub has already rendered the markdown/blob
2. fall back to raw lines and emit a fenced code block, preserving the blob language when GitHub provides it

### Issues and pull requests

For issue/PR pages the extractor tries hard to return the actual discussion content, not just whatever sparse shell the DOM happens to expose.

Data sources:

- embedded `preloadedQueries` issue data
- visible markdown body selectors
- visible timeline and review comment selectors

Behavior highlights:

- route number must match the current issue/PR number
- hidden and minimized discussion items are skipped
- discussion entries are deduplicated
- main PR body is not duplicated when visible discussion containers also include it
- body, author, and published timestamp can all come from embedded data

When comments are present, they are appended under:

```text
## Discussion
```

### Releases

Release extraction supports two shapes:

- the releases index page, where only the first visible release section is used
- a dedicated tagged release page, where the whole document is scanned

Release output may include:

- title
- author
- published timestamp
- body text
- an `## Assets` section listing downloadable artifacts

## Warning Semantics

Warnings are non-fatal signals. They do not necessarily imply `ok = false`.

Current warnings:

- `LowContentYield`
- `LowSignalExtraction`
- `PossibleJsRenderedPage`
- `ContentTruncated`

### `LowContentYield`

Assigned when the final visible text is under 200 characters.

### `LowSignalExtraction`

Assigned when the extractor concludes that the HTML-to-text yield is suspiciously poor and the page likely did not expose enough meaningful content.

### `PossibleJsRenderedPage`

Assigned only when low-signal extraction is already true and the raw HTML also looks like a JS app shell.

Current shell markers include strings such as:

- `__next`
- `id="root"`
- `id="app"`
- `data-reactroot`
- `__nuxt`
- `ng-version`

### `ContentTruncated`

Assigned when either:

- the response body exceeded the download cap
- the rendered output exceeded the 262,144-character output cap

## Error Semantics

Current structured error codes include:

- `invalid_url`
- `unsupported_scheme`
- `blocked_address`
- `dns_error`
- `connect_error`
- `timeout`
- `redirect_error`
- `decode_error`
- `content_too_large`
- `http_<status>` for HTTP 4xx/5xx responses

Important edge cases:

- HTTP error pages still preserve extracted body snippets when possible, but the final `extraction_kind` becomes `Error` and `ok` becomes `false`.
- Extraction-level decode issues also force `ok = false` and `extraction_kind = Error`.
- Many transport failures still include partial context such as `final_url`, `status`, or `content_type` when that information was available before the failure.

## Test Strategy

The main test corpus is [`src/tools/web_fetch/tests.rs`](../src/tools/web_fetch/tests.rs). It is broad enough that this file should be your first stop before changing heuristics.

Major coverage areas:

- SSRF and blocked-address behavior
- DNS timeout and resolver-pool behavior
- bounded address retry across IPv4/IPv6 candidates
- redirect handling
- browser header shaping
- oversize response handling
- content sniffing across JSON/text/XML/binary cases
- charset decoding
- HTML rendering fidelity for lists, links, blockquotes, code fences, tables, and truncation
- low-signal and JS-shell warning behavior
- GitHub route extraction
- fixture coverage against real saved GitHub pages
- live canary manifest validation

Additional contract tests:

- [`tests/tools_web_fetch.rs`](../tests/tools_web_fetch.rs): confirms the tool returns structured failures instead of bubbling a bad URL up as a tool error
- [`tests/cli_webfetch.rs`](../tests/cli_webfetch.rs): confirms CLI formatting and usage behavior

## Live Canaries

Live canary support is split across:

- [`src/tools/web_fetch/live_canaries.rs`](../src/tools/web_fetch/live_canaries.rs)
- [`src/tools/web_fetch/fixtures/live_canaries.toml`](../src/tools/web_fetch/fixtures/live_canaries.toml)

The three tiers are:

- `gating`: small, fast confidence set
- `observational`: broader real-site coverage
- `self_hosted_edge`: deterministic edge cases from a self-hosted test site

Useful commands:

```bash
cargo test web_fetch_live_canaries_gating -- --ignored --nocapture
cargo test web_fetch_live_canaries_observational -- --ignored --nocapture
HEADLESS_WEB_FETCH_CANARY_CASE_ID=gating_openai_docs_function_calling cargo test web_fetch_live_canaries_gating -- --ignored --nocapture
HEADLESS_WEB_FETCH_CANARY_SELF_HOSTED_BASE_URL=https://canary.example.com cargo test web_fetch_live_canaries_self_hosted_edge -- --ignored --nocapture
```

Useful environment variables:

- `HEADLESS_WEB_FETCH_CANARY_TIER`
- `HEADLESS_WEB_FETCH_CANARY_CASE_ID`
- `HEADLESS_WEB_FETCH_CANARY_SELF_HOSTED_BASE_URL`

The self-hosted tier is especially useful for cases that are awkward to guarantee on public internet pages, such as:

- Windows-1252 HTML
- pure JS-shell pages
- error pages with bodies
- redirect loops
- XML feeds
- intentionally truncated large responses

## Comparative Benchmark

The live canaries above verify that our extraction meets absolute quality bars (markers present, min content chars, correct warnings). They do not tell us how we compare against other extraction tools on the same pages.

[`scripts/bench_webfetch.sh`](../scripts/bench_webfetch.sh) fills that gap. It fetches a set of diverse URLs with three tools in parallel — headless, [defuddle](https://github.com/nichochar/defuddle) (`npm i -g defuddle-cli`), and raw curl — and checks:

1. **Content produced** — headless returned non-empty content
2. **Size parity with defuddle** — headless is at least 60% the size (not drastically worse)
3. **Minimum content chars** — extracted body meets a per-case threshold
4. **No content duplication** — unique long lines (40+ chars) are ≥75% of total
5. **Required markers** — key phrases appear in the output

### Running

```bash
./scripts/bench_webfetch.sh                           # all cases
./scripts/bench_webfetch.sh paulgraham_greatwork       # single case
HEADLESS=./target/debug/headless ./scripts/bench_webfetch.sh   # custom binary
```

Raw outputs for each tool are saved to `$BENCH_WEBFETCH_OUTDIR` (default `/tmp/bench_webfetch/`) for manual inspection after the run.

### Adding cases

Append to the `CASES` array in the script. Each entry is pipe-delimited:

```
"case_id|url|min_chars|required marker 1|required marker 2|..."
```

Good candidates for new cases are pages that:

- use layout tables or unusual DOM structures (the class of bug this benchmark was created to catch)
- are representative of a site category we care about (docs, blogs, academic, link aggregators)
- have stable content that won't rot the required markers quickly
- live at version-pinned or otherwise durable URLs when possible
- can be fetched reliably by automation; for some Q&A sites that may mean using a stable print-friendly view such as StackPrinter instead of the canonical page

### When to run

Run after any change to root selection, table rendering, or the block collector in `render.rs` or `html.rs`. It is not part of CI — it hits live URLs, depends on defuddle being installed, and takes around a minute depending on network conditions. Treat it the same way you treat the live canaries: a manual check before merging extraction changes.

## Maintenance Notes

### If you change fetch safety behavior

Check these areas together:

- blocked hostnames/IPs in [`src/tools/web_fetch/transport.rs`](../src/tools/web_fetch/transport.rs)
- offline safety tests in [`src/tools/web_fetch/tests.rs`](../src/tools/web_fetch/tests.rs)
- CLI/tool contract expectations in [`tests/cli_webfetch.rs`](../tests/cli_webfetch.rs) and [`tests/tools_web_fetch.rs`](../tests/tools_web_fetch.rs)

### If you change content sniffing or rendering

Re-run and review tests covering:

- JSON/text/binary classification
- HTML low-signal warnings
- code-fence truncation
- link rendering
- table/list rendering
- fixture-based GitHub extraction

Those areas are tightly coupled. Small heuristic changes can easily move a page from `HtmlPrimary` to `HtmlFallback`, add/remove warnings, or change whether content is truncated.

Also run `./scripts/bench_webfetch.sh` to verify extraction quality hasn't regressed against defuddle on real pages.

### If you add a new site extractor

Use the GitHub extractor as the template:

1. Add a new module under [`src/tools/web_fetch/sites/`](../src/tools/web_fetch/sites/).
2. Register it in [`src/tools/web_fetch/sites/mod.rs`](../src/tools/web_fetch/sites/mod.rs).
3. Keep it opt-in by hostname/route classification.
4. Prefer embedded structured payloads over brittle visual selectors when the site exposes them.
5. Add focused offline tests first.
6. Add a saved fixture if the DOM/payload shape is large or realistic behavior matters.
7. Consider a live canary only if the public URL is likely to remain stable enough.

### If you change limits or timeouts

Update all three:

- the code constants
- this document
- any tests or canaries whose assertions depend on the old threshold

## Practical Mental Model

When reading or debugging `web_fetch`, the most useful mental model is:

- transport tries to safely reach one public URL
- content classification decides what kind of bytes came back
- extraction tries to turn those bytes into something LLM-friendly
- warnings describe quality concerns
- errors describe fetch or extraction failure classes
- site-specific extractors exist to rescue high-value sites whose generic DOM output is not good enough

That division of responsibility is the main thing worth preserving as the tool grows.
