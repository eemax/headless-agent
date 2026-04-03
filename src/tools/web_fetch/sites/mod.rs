use scraper::Html;

mod github;

#[derive(Debug, Default, Clone)]
pub(super) struct SiteExtraction {
    pub(super) title: Option<String>,
    pub(super) description: Option<String>,
    pub(super) author: Option<String>,
    pub(super) published: Option<String>,
    pub(super) body: String,
}

impl SiteExtraction {
    fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.author.is_none()
            && self.published.is_none()
            && self.body.trim().is_empty()
    }
}

type Extractor = fn(Option<&str>, &Html) -> Option<SiteExtraction>;

pub(super) fn extract(source_url: Option<&str>, document: &Html) -> Option<SiteExtraction> {
    const EXTRACTORS: &[Extractor] = &[github::extract];

    EXTRACTORS
        .iter()
        .find_map(|extractor| extractor(source_url, document))
        .filter(|extraction| !extraction.is_empty())
}
