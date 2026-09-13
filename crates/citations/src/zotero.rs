//! A client for Zotero's local HTTP API (Zotero 7 and later), the one
//! surface that needs no add-on and no API key. It is off by default: the
//! user ticks Settings → Advanced → "Allow other applications on this
//! computer to communicate with Zotero", and until then every `/api/`
//! request answers 403 with `Local API is not enabled`.

use std::{path::PathBuf, sync::Arc};

use anyhow::{Context as _, Result, anyhow};
use futures::AsyncReadExt as _;
use http_client::{AsyncBody, HttpClient};
use serde_json::Value;

pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:23119";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoteroStatus {
    Ready,
    /// Nothing answered on the port.
    NotRunning,
    /// Zotero answered, but the local API preference is off.
    LocalApiDisabled,
}

#[derive(Debug)]
pub enum ZoteroError {
    NotRunning(anyhow::Error),
    LocalApiDisabled,
    Http { status: u16, body: String },
    Other(anyhow::Error),
}

impl std::fmt::Display for ZoteroError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning(_) => write!(formatter, "Zotero is not running"),
            Self::LocalApiDisabled => write!(
                formatter,
                "Zotero's local API is off: tick Settings › Advanced › \"Allow other applications on this computer to communicate with Zotero\""
            ),
            Self::Http { status, body } => {
                write!(formatter, "Zotero answered {status}: {}", body.trim())
            }
            Self::Other(error) => write!(formatter, "{error:#}"),
        }
    }
}

impl std::error::Error for ZoteroError {}

impl From<anyhow::Error> for ZoteroError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoteroItem {
    pub key: String,
    pub item_type: String,
    pub title: String,
    /// Creators as "Given Family" display strings, in Zotero's order.
    pub creators: Vec<String>,
    /// Family name of the first author, for key minting.
    pub first_surname: Option<String>,
    pub date: String,
    /// Filled by Better BibTeX when that add-on is installed; Zotero itself
    /// never generates a value for this field.
    pub citation_key: Option<String>,
}

impl ZoteroItem {
    pub fn year(&self) -> Option<String> {
        crate::bibliography::first_year(&self.date)
    }
}

pub struct ZoteroClient {
    http: Arc<dyn HttpClient>,
    base_url: String,
}

impl ZoteroClient {
    pub fn new(http: Arc<dyn HttpClient>, base_url: impl Into<String>) -> Self {
        let mut base_url = base_url.into();
        while base_url.ends_with('/') {
            base_url.pop();
        }
        Self { http, base_url }
    }

    /// A client to the Zotero on this machine. Deliberately not Zed's shared
    /// HTTP client: that one honors the proxy environment variables, and a
    /// proxied request to localhost comes back 502 from the proxy.
    pub fn local(base_url: impl Into<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(2))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .context("building the Zotero HTTP client")?;
        let http: Arc<dyn HttpClient> = Arc::new(reqwest_client::ReqwestClient::from(client));
        Ok(Self::new(http, base_url))
    }

    pub async fn status(&self) -> ZoteroStatus {
        match self.get("/api/users/0/items?limit=1&format=keys").await {
            Ok(_) => ZoteroStatus::Ready,
            Err(ZoteroError::LocalApiDisabled) => ZoteroStatus::LocalApiDisabled,
            Err(ZoteroError::NotRunning(_)) => ZoteroStatus::NotRunning,
            // Anything else answered: Zotero is up and the API is on.
            Err(_) => ZoteroStatus::Ready,
        }
    }

    /// Top-level items whose title, creators or year match `query`.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<ZoteroItem>, ZoteroError> {
        let path = format!(
            "/api/users/0/items/top?q={}&qmode=titleCreatorYear&limit={limit}&format=json&sort=dateModified&direction=desc",
            urlencoding::encode(query.trim())
        );
        let (_, body) = self.get(&path).await?;
        Ok(parse_items(&body)?)
    }

    /// The item as Zotero's own BibLaTeX export, key and all; the caller
    /// renames the key before writing it anywhere.
    pub async fn biblatex(&self, item_key: &str) -> Result<String, ZoteroError> {
        let (_, body) = self
            .get(&format!("/api/users/0/items/{item_key}?format=biblatex"))
            .await?;
        Ok(body)
    }

    /// The on-disk path of the item's first PDF attachment, if it has one
    /// Zotero can locate. The file endpoint answers with a redirect to a
    /// `file://` URL rather than the bytes.
    pub async fn pdf_path(&self, item_key: &str) -> Result<Option<PathBuf>, ZoteroError> {
        let (_, body) = self
            .get(&format!(
                "/api/users/0/items/{item_key}/children?format=json"
            ))
            .await?;
        let Some(attachment_key) = first_pdf_attachment_key(&body)? else {
            return Ok(None);
        };
        let uri = format!("{}/api/users/0/items/{attachment_key}/file", self.base_url);
        let response = self
            .http
            .get(&uri, AsyncBody::empty(), false)
            .await
            .map_err(ZoteroError::NotRunning)?;
        let location = response
            .headers()
            .get(http_client::http::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        Ok(location.as_deref().and_then(file_url_to_path))
    }

    async fn get(&self, path_and_query: &str) -> Result<(u16, String), ZoteroError> {
        let uri = format!("{}{path_and_query}", self.base_url);
        let mut response = self
            .http
            .get(&uri, AsyncBody::empty(), true)
            .await
            .map_err(ZoteroError::NotRunning)?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut bytes)
            .await
            .with_context(|| format!("reading the response from {uri}"))?;
        let body = String::from_utf8_lossy(&bytes).into_owned();
        if status == 403 && body.contains("Local API is not enabled") {
            return Err(ZoteroError::LocalApiDisabled);
        }
        if !(200..300).contains(&status) {
            return Err(ZoteroError::Http { status, body });
        }
        Ok((status, body))
    }
}

/// Reads the web-API-v3 item shape the local API mirrors: an array of
/// `{ key, data: { itemType, title, creators, date, citationKey } }`.
pub fn parse_items(json: &str) -> Result<Vec<ZoteroItem>> {
    let value: Value = serde_json::from_str(json).context("parsing Zotero's item list")?;
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("Zotero's item list is not an array"))?;
    Ok(items.iter().filter_map(parse_item).collect())
}

fn parse_item(value: &Value) -> Option<ZoteroItem> {
    let data = value.get("data")?;
    let key = data
        .get("key")
        .or_else(|| value.get("key"))?
        .as_str()?
        .to_string();
    let string = |name: &str| {
        data.get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    let creator_entries = data
        .get("creators")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut creators = Vec::with_capacity(creator_entries.len());
    let mut first_surname = None;
    for creator in &creator_entries {
        let surname = creator.get("lastName").and_then(Value::as_str);
        let given = creator.get("firstName").and_then(Value::as_str);
        let single = creator.get("name").and_then(Value::as_str);
        let display = match (given, surname, single) {
            (Some(given), Some(surname), _) if !given.is_empty() => format!("{given} {surname}"),
            (_, Some(surname), _) => surname.to_string(),
            (_, _, Some(single)) => single.to_string(),
            _ => continue,
        };
        let is_author = creator
            .get("creatorType")
            .and_then(Value::as_str)
            .is_none_or(|kind| kind == "author");
        if first_surname.is_none() && is_author {
            first_surname = surname
                .filter(|name| !name.is_empty())
                .or(single)
                .map(str::to_string);
        }
        creators.push(display);
    }
    if first_surname.is_none() {
        first_surname = creator_entries.first().and_then(|creator| {
            creator
                .get("lastName")
                .or_else(|| creator.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    }
    Some(ZoteroItem {
        key,
        item_type: string("itemType").unwrap_or_default(),
        title: string("title").unwrap_or_default(),
        creators,
        first_surname,
        date: string("date").unwrap_or_default(),
        citation_key: string("citationKey"),
    })
}

fn first_pdf_attachment_key(children_json: &str) -> Result<Option<String>> {
    let value: Value =
        serde_json::from_str(children_json).context("parsing Zotero's child items")?;
    let children = value
        .as_array()
        .ok_or_else(|| anyhow!("Zotero's child list is not an array"))?;
    Ok(children.iter().find_map(|child| {
        let data = child.get("data")?;
        let is_pdf = data.get("itemType").and_then(Value::as_str) == Some("attachment")
            && data.get("contentType").and_then(Value::as_str) == Some("application/pdf");
        is_pdf.then(|| data.get("key")?.as_str().map(str::to_string))?
    }))
}

fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // `file:///Users/x` → `/Users/x`; `file://localhost/Users/x` → `/Users/x`.
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = urlencoding::decode(path).ok()?;
    Some(PathBuf::from(decoded.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_client::{FakeHttpClient, Response};

    const ITEMS: &str = r#"[
      {"key": "Y5TDIU47", "version": 3, "data": {
        "key": "Y5TDIU47", "itemType": "preprint",
        "title": "SkillsBench: Benchmarking How Well Agent Skills Work",
        "creators": [
          {"creatorType": "editor", "firstName": "Eve", "lastName": "Editor"},
          {"creatorType": "author", "firstName": "Ada", "lastName": "Lovelace"},
          {"creatorType": "author", "name": "The Consortium"}
        ],
        "date": "2026-02-01", "citationKey": "lovelace2026skillsbench"
      }},
      {"key": "NOAUTHOR", "data": {"key": "NOAUTHOR", "itemType": "webpage", "title": "Anonymous page", "creators": [], "date": ""}}
    ]"#;

    #[test]
    fn parses_the_web_api_item_shape() {
        let items = parse_items(ITEMS).unwrap();
        assert_eq!(items.len(), 2);
        let first = &items[0];
        assert_eq!(first.key, "Y5TDIU47");
        assert_eq!(first.item_type, "preprint");
        assert_eq!(
            first.creators,
            ["Eve Editor", "Ada Lovelace", "The Consortium"]
        );
        assert_eq!(first.first_surname.as_deref(), Some("Lovelace"));
        assert_eq!(first.year().as_deref(), Some("2026"));
        assert_eq!(
            first.citation_key.as_deref(),
            Some("lovelace2026skillsbench")
        );
        let second = &items[1];
        assert_eq!(second.first_surname, None);
        assert_eq!(second.year(), None);
        assert_eq!(second.citation_key, None);
    }

    #[test]
    fn finds_the_pdf_among_children() {
        let children = r#"[
          {"data": {"key": "NOTE1", "itemType": "note"}},
          {"data": {"key": "SNAP1", "itemType": "attachment", "contentType": "text/html"}},
          {"data": {"key": "PDTR8GS2", "itemType": "attachment", "contentType": "application/pdf"}}
        ]"#;
        assert_eq!(
            first_pdf_attachment_key(children).unwrap().as_deref(),
            Some("PDTR8GS2")
        );
        assert_eq!(first_pdf_attachment_key("[]").unwrap(), None);
    }

    #[test]
    fn converts_file_urls_to_paths() {
        assert_eq!(
            file_url_to_path("file:///Users/h/Zotero/storage/AB12/Paper%20Title.pdf"),
            Some(PathBuf::from(
                "/Users/h/Zotero/storage/AB12/Paper Title.pdf"
            ))
        );
        assert_eq!(
            file_url_to_path("file://localhost/tmp/a.pdf"),
            Some(PathBuf::from("/tmp/a.pdf"))
        );
        assert_eq!(file_url_to_path("https://example.com/a.pdf"), None);
    }

    fn client_answering(status: u16, body: &'static str) -> ZoteroClient {
        let http = FakeHttpClient::create(move |_request| async move {
            Ok(Response::builder()
                .status(status)
                .body(AsyncBody::from(body))
                .unwrap())
        });
        ZoteroClient::new(http, DEFAULT_BASE_URL)
    }

    #[test]
    fn status_distinguishes_disabled_from_running() {
        let disabled = client_answering(403, "Local API is not enabled");
        assert_eq!(
            futures::executor::block_on(disabled.status()),
            ZoteroStatus::LocalApiDisabled
        );
        let ready = client_answering(200, "[]");
        assert_eq!(
            futures::executor::block_on(ready.status()),
            ZoteroStatus::Ready
        );

        let refused =
            FakeHttpClient::create(|_request| async move { Err(anyhow!("connection refused")) });
        let refused = ZoteroClient::new(refused, DEFAULT_BASE_URL);
        assert_eq!(
            futures::executor::block_on(refused.status()),
            ZoteroStatus::NotRunning
        );
    }

    #[test]
    fn search_surfaces_the_disabled_state_as_a_typed_error() {
        let disabled = client_answering(403, "Local API is not enabled");
        let error = futures::executor::block_on(disabled.search("skills", 10)).unwrap_err();
        assert!(matches!(error, ZoteroError::LocalApiDisabled), "{error}");
        let ready = client_answering(200, ITEMS);
        let items = futures::executor::block_on(ready.search("skills", 10)).unwrap();
        assert_eq!(items.len(), 2);
    }
}
