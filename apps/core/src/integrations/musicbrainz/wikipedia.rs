use reqwest::Url;
use serde_json::Value;

pub const DISAMBIGUATION_PAGE_TYPE: &str = "disambiguation";

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the Wikipedia response was malformed")]
pub struct ParseError;
const SUFFIX: &str = ".wikipedia.org";
const SITELINK_SUFFIX: &str = "wiki";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikipediaSummary {
    pub page_type: Option<String>,
    pub extract: Option<String>,
    pub title: Option<String>,
    pub page_url: Option<String>,
}

pub fn wikipedia_target(raw: &str) -> Option<(String, String)> {
    let url = Url::parse(raw).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let path = url.path();
    if !host.ends_with(SUFFIX) || !path.starts_with("/wiki/") {
        return None;
    }
    let encoded = format!("title={}", &path[6..]);
    let title = url::form_urlencoded::parse(encoded.as_bytes())
        .next()?
        .1
        .replace('_', " ")
        .trim()
        .to_owned();
    (!title.is_empty()).then_some((host, title))
}

pub fn sitelink_wiki_target(sitelinks: &Value, language: Option<&str>) -> Option<(String, String)> {
    let links = sitelinks.as_object()?;
    let primary = language
        .filter(|language| *language != "en")
        .unwrap_or("en");
    for code in [Some(primary), (primary != "en").then_some("en")]
        .into_iter()
        .flatten()
    {
        let Some(title) = links
            .get(&format!("{code}{SITELINK_SUFFIX}"))
            .and_then(|entry| entry.get("title"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if !title.trim().is_empty() {
            return Some((format!("{code}{SUFFIX}"), title.trim().to_owned()));
        }
    }
    None
}

pub fn parse_summary(payload: &Value) -> Result<WikipediaSummary, ParseError> {
    let object = payload.as_object().ok_or(ParseError)?;
    let text = |member: &str| -> Result<Option<String>, ParseError> {
        match object.get(member) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.clone())),
            Some(_) => Err(ParseError),
        }
    };
    let page_url = object
        .get("content_urls")
        .and_then(|value| value.get("desktop"))
        .and_then(|value| value.get("page"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
        .map(str::to_owned);
    Ok(WikipediaSummary {
        page_type: text("type")?,
        extract: text("extract")?,
        title: text("title")?,
        page_url,
    })
}

pub fn parse_extract(payload: &Value) -> Result<Option<String>, ParseError> {
    let Some(query) = payload.get("query") else {
        return Ok(None);
    };
    let query = query.as_object().ok_or(ParseError)?;
    let Some(pages) = query.get("pages") else {
        return Ok(None);
    };
    let pages = pages.as_object().ok_or(ParseError)?;
    for page in pages.values() {
        let page = page.as_object().ok_or(ParseError)?;
        match page.get("extract") {
            None | Some(Value::Null) => {}
            Some(Value::String(extract)) if !extract.trim().is_empty() => {
                return Ok(Some(extract.clone()));
            }
            Some(Value::String(_)) => {}
            Some(_) => return Err(ParseError),
        }
    }
    Ok(None)
}
