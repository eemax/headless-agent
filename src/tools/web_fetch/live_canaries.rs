use std::{env, sync::OnceLock};

use serde::Deserialize;

use super::{ExtractionKind, Warning};

pub(super) const CASE_ID_ENV: &str = "HEADLESS_WEB_FETCH_CANARY_CASE_ID";
pub(super) const SELF_HOSTED_BASE_URL_ENV: &str = "HEADLESS_WEB_FETCH_CANARY_SELF_HOSTED_BASE_URL";
pub(super) const TIER_ENV: &str = "HEADLESS_WEB_FETCH_CANARY_TIER";

const RAW_MANIFEST: &str = include_str!("fixtures/live_canaries.toml");

static MANIFEST: OnceLock<CanaryManifest> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CanaryTier {
    Gating,
    Observational,
    SelfHostedEdge,
}

#[derive(Debug)]
pub(super) struct CanaryCase {
    pub(super) id: String,
    pub(super) url: String,
    pub(super) tier: CanaryTier,
    pub(super) expect_ok: bool,
    pub(super) expected_status: Option<u16>,
    pub(super) expected_error: Option<String>,
    pub(super) expected_extraction_kind: ExtractionKind,
    pub(super) expected_content_type_prefix: Option<String>,
    pub(super) expected_final_url_prefix: Option<String>,
    pub(super) required_markers: Vec<String>,
    pub(super) forbidden_markers: Vec<String>,
    pub(super) required_warnings: Vec<Warning>,
    pub(super) forbidden_warnings: Vec<Warning>,
    pub(super) min_content_chars: usize,
}

#[derive(Debug)]
struct CanaryManifest {
    cases: Vec<CanaryCase>,
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    #[serde(rename = "case")]
    cases: Vec<RawCanaryCase>,
}

#[derive(Debug, Deserialize)]
struct RawCanaryCase {
    id: String,
    url: String,
    tier: String,
    expect_ok: bool,
    #[serde(default)]
    expected_status: Option<u16>,
    #[serde(default)]
    expected_error: Option<String>,
    expected_extraction_kind: String,
    #[serde(default)]
    expected_content_type_prefix: Option<String>,
    #[serde(default)]
    expected_final_url_prefix: Option<String>,
    #[serde(default)]
    required_markers: Vec<String>,
    #[serde(default)]
    forbidden_markers: Vec<String>,
    #[serde(default)]
    required_warnings: Vec<String>,
    #[serde(default)]
    forbidden_warnings: Vec<String>,
    #[serde(default)]
    min_content_chars: usize,
}

impl CanaryTier {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Gating => "gating",
            Self::Observational => "observational",
            Self::SelfHostedEdge => "self_hosted_edge",
        }
    }
}

impl TryFrom<RawCanaryCase> for CanaryCase {
    type Error = String;

    fn try_from(raw: RawCanaryCase) -> Result<Self, Self::Error> {
        Ok(Self {
            id: raw.id,
            url: raw.url,
            tier: parse_tier(&raw.tier)?,
            expect_ok: raw.expect_ok,
            expected_status: raw.expected_status,
            expected_error: raw.expected_error,
            expected_extraction_kind: parse_extraction_kind(&raw.expected_extraction_kind)?,
            expected_content_type_prefix: raw.expected_content_type_prefix,
            expected_final_url_prefix: raw.expected_final_url_prefix,
            required_markers: raw.required_markers,
            forbidden_markers: raw.forbidden_markers,
            required_warnings: raw
                .required_warnings
                .into_iter()
                .map(|value| parse_warning(&value))
                .collect::<Result<Vec<_>, _>>()?,
            forbidden_warnings: raw
                .forbidden_warnings
                .into_iter()
                .map(|value| parse_warning(&value))
                .collect::<Result<Vec<_>, _>>()?,
            min_content_chars: raw.min_content_chars,
        })
    }
}

pub(super) fn cases() -> &'static [CanaryCase] {
    &manifest().cases
}

pub(super) fn cases_for_tier(tier: CanaryTier) -> Vec<&'static CanaryCase> {
    let requested_case_id = requested_case_filter();

    cases()
        .iter()
        .filter(|case| case.tier == tier)
        .filter(|case| {
            requested_case_id
                .as_deref()
                .map(|value| case.id == value)
                .unwrap_or(true)
        })
        .collect()
}

pub(super) fn requested_case_filter() -> Option<String> {
    env::var(CASE_ID_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn requested_tier_filter() -> Option<CanaryTier> {
    env::var(TIER_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            parse_tier(&value).unwrap_or_else(|error| {
                panic!("invalid `{}` value `{}`: {}", TIER_ENV, value, error)
            })
        })
}

pub(super) fn resolve_case_url(case: &CanaryCase) -> Result<String, String> {
    if case.tier == CanaryTier::SelfHostedEdge && case.url.starts_with('/') {
        return self_hosted_base_url().map(|base| join_base_url(&base, &case.url));
    }
    Ok(case.url.clone())
}

pub(super) fn resolve_expected_final_url_prefix(
    case: &CanaryCase,
) -> Result<Option<String>, String> {
    let Some(expected) = case.expected_final_url_prefix.as_deref() else {
        return Ok(None);
    };
    if case.tier == CanaryTier::SelfHostedEdge && expected.starts_with('/') {
        return self_hosted_base_url().map(|base| Some(join_base_url(&base, expected)));
    }
    Ok(Some(expected.to_string()))
}

fn manifest() -> &'static CanaryManifest {
    MANIFEST.get_or_init(|| {
        let raw: RawManifest = toml::from_str(RAW_MANIFEST)
            .unwrap_or_else(|error| panic!("failed to parse live canary manifest: {error}"));
        let cases = raw
            .cases
            .into_iter()
            .map(CanaryCase::try_from)
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|error| panic!("invalid live canary manifest: {error}"));
        CanaryManifest { cases }
    })
}

fn parse_tier(value: &str) -> Result<CanaryTier, String> {
    match value {
        "gating" => Ok(CanaryTier::Gating),
        "observational" => Ok(CanaryTier::Observational),
        "self_hosted_edge" => Ok(CanaryTier::SelfHostedEdge),
        _ => Err(format!("unknown tier `{value}`")),
    }
}

fn parse_extraction_kind(value: &str) -> Result<ExtractionKind, String> {
    match value {
        "Json" => Ok(ExtractionKind::Json),
        "Text" => Ok(ExtractionKind::Text),
        "HtmlPrimary" => Ok(ExtractionKind::HtmlPrimary),
        "HtmlFallback" => Ok(ExtractionKind::HtmlFallback),
        "BinarySummary" => Ok(ExtractionKind::BinarySummary),
        "Error" => Ok(ExtractionKind::Error),
        _ => Err(format!("unknown extraction kind `{value}`")),
    }
}

fn parse_warning(value: &str) -> Result<Warning, String> {
    match value {
        "LowContentYield" => Ok(Warning::LowContentYield),
        "LowSignalExtraction" => Ok(Warning::LowSignalExtraction),
        "PossibleJsRenderedPage" => Ok(Warning::PossibleJsRenderedPage),
        "ContentTruncated" => Ok(Warning::ContentTruncated),
        _ => Err(format!("unknown warning `{value}`")),
    }
}

fn self_hosted_base_url() -> Result<String, String> {
    let value = env::var(SELF_HOSTED_BASE_URL_ENV).map_err(|_| {
        format!(
            "`{}` is required when running the `self_hosted_edge` live canary tier",
            SELF_HOSTED_BASE_URL_ENV
        )
    })?;
    let trimmed = value.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return Err(format!(
            "`{}` must not be empty when running the `self_hosted_edge` live canary tier",
            SELF_HOSTED_BASE_URL_ENV
        ));
    }
    Ok(trimmed)
}

fn join_base_url(base: &str, path: &str) -> String {
    format!("{base}{path}")
}
