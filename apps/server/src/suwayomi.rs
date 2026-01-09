//! Server-side federation client for a Suwayomi/Tachidesk server.
//!
//! Mirrors the TS `SuwayomiBackend` adapter: it talks Suwayomi's real GraphQL and
//! returns raw Suwayomi shapes, which `graphql` maps onto the Komika contract.

use anyhow::{anyhow, Result};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

const MANGA_FIELDS: &str = r#"
fragment MangaFields on MangaType {
    id title thumbnailUrl author artist description genre status
    inLibrary inLibraryAt lastFetchedAt sourceId
    source { lang }
    chapters { totalCount }
}"#;

const CHAPTER_FIELDS: &str = r#"
fragment ChapterFields on ChapterType {
    id mangaId name chapterNumber scanlator uploadDate
    isRead isBookmarked isDownloaded lastPageRead pageCount
}"#;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuwayomiSourceLang {
    pub lang: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChapterCount {
    #[serde(rename = "totalCount")]
    pub total_count: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuwayomiManga {
    pub id: i64,
    pub title: String,
    pub thumbnail_url: Option<String>,
    pub author: Option<String>,
    pub artist: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub genre: Vec<String>,
    pub status: String,
    pub in_library: bool,
    pub in_library_at: Option<String>,
    pub last_fetched_at: Option<String>,
    pub source_id: String,
    pub source: Option<SuwayomiSourceLang>,
    pub chapters: Option<ChapterCount>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuwayomiChapter {
    pub id: i64,
    pub manga_id: i64,
    pub name: String,
    pub chapter_number: f64,
    pub scanlator: Option<String>,
    pub upload_date: Option<String>,
    pub is_read: bool,
    pub is_bookmarked: bool,
    pub is_downloaded: bool,
    pub last_page_read: i64,
    pub page_count: i64,
}

/// Which source-manga listing to fetch.
#[derive(Clone, Copy)]
pub enum FetchType {
    Popular,
    Latest,
    Search,
}

impl FetchType {
    fn as_str(self) -> &'static str {
        match self {
            FetchType::Popular => "POPULAR",
            FetchType::Latest => "LATEST",
            FetchType::Search => "SEARCH",
        }
    }
}

pub struct SuwayomiClient {
    base_url: String,
    /// Public base used when building image URLs (covers/pages) handed to the
    /// browser. Defaults to `base_url` when no public URL is configured.
    image_base_url: String,
    http: reqwest::Client,
    /// Cached resolved source id; `Some` once resolved.
    source_id: Mutex<Option<String>>,
    configured_source: Option<String>,
}

impl SuwayomiClient {
    pub fn new(
        base_url: String,
        public_url: Option<String>,
        configured_source: Option<String>,
    ) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        let image_base_url = public_url
            .map(|u| u.trim_end_matches('/').to_string())
            .unwrap_or_else(|| base_url.clone());
        Self {
            base_url,
            image_base_url,
            http: reqwest::Client::new(),
            source_id: Mutex::new(configured_source.clone()),
            configured_source,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/api/graphql", self.base_url)
    }

    /// Turn a possibly-relative Suwayomi image URL into an absolute, publicly
    /// reachable one (uses `image_base_url`, not the internal federation host).
    pub fn abs(&self, url: Option<&str>) -> String {
        match url {
            None | Some("") => String::new(),
            Some(u) if u.starts_with("http") => u.to_string(),
            Some(u) => format!("{}{}", self.image_base_url, u),
        }
    }

    async fn gql<T: DeserializeOwned>(&self, query: &str, variables: Value) -> Result<T> {
        let res = self
            .http
            .post(self.endpoint())
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await?;
        if !res.status().is_success() {
            return Err(anyhow!("Suwayomi error {}", res.status()));
        }
        let body: Value = res.json().await?;
        if let Some(errors) = body.get("errors").and_then(|e| e.as_array()) {
            if !errors.is_empty() {
                let msg = errors
                    .iter()
                    .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(anyhow!("{msg}"));
            }
        }
        let data = body
            .get("data")
            .cloned()
            .ok_or_else(|| anyhow!("Suwayomi returned no data"))?;
        Ok(serde_json::from_value(data)?)
    }

    /// Resolve (and cache) a source id to browse: configured → English → first real.
    async fn resolve_source(&self) -> Result<String> {
        let mut guard = self.source_id.lock().await;
        if let Some(id) = guard.as_ref() {
            return Ok(id.clone());
        }
        if let Some(id) = &self.configured_source {
            *guard = Some(id.clone());
            return Ok(id.clone());
        }
        #[derive(Deserialize)]
        struct Src {
            id: String,
            lang: Option<String>,
        }
        #[derive(Deserialize)]
        struct Nodes {
            nodes: Vec<Src>,
        }
        #[derive(Deserialize)]
        struct Data {
            sources: Nodes,
        }
        let data: Data = self
            .gql("query Sources { sources { nodes { id lang } } }", json!({}))
            .await?;
        let real: Vec<Src> = data
            .sources
            .nodes
            .into_iter()
            .filter(|s| s.id != "0")
            .collect();
        let chosen = real
            .iter()
            .find(|s| s.lang.as_deref() == Some("en"))
            .or_else(|| real.first())
            .ok_or_else(|| anyhow!("No Suwayomi source installed — add one first"))?;
        *guard = Some(chosen.id.clone());
        Ok(chosen.id.clone())
    }

    pub async fn fetch_source(
        &self,
        ty: FetchType,
        page: i32,
        query: Option<&str>,
    ) -> Result<(bool, Vec<SuwayomiManga>)> {
        let source = self.resolve_source().await?;
        let doc = format!(
            "{MANGA_FIELDS}\n\
             mutation F($source: LongString!, $type: FetchSourceMangaType!, $page: Int!, $query: String) {{\
               fetchSourceManga(input: {{ source: $source, type: $type, page: $page, query: $query }}) {{\
                 hasNextPage mangas {{ ...MangaFields }} }} }}"
        );
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Payload {
            has_next_page: bool,
            mangas: Vec<SuwayomiManga>,
        }
        #[derive(Deserialize)]
        struct Data {
            #[serde(rename = "fetchSourceManga")]
            fetch_source_manga: Payload,
        }
        let data: Data = self
            .gql(
                &doc,
                json!({ "source": source, "type": ty.as_str(), "page": page, "query": query }),
            )
            .await?;
        Ok((
            data.fetch_source_manga.has_next_page,
            data.fetch_source_manga.mangas,
        ))
    }

    /// Fetch full detail from the source (populates genres/status/description),
    /// falling back to the DB-cached manga if the source fetch fails.
    pub async fn series(&self, id: i64) -> Result<SuwayomiManga> {
        let detail = format!(
            "{MANGA_FIELDS}\n\
             mutation D($id: Int!) {{ fetchMangaAndChapters(input: {{ id: $id, fetchManga: true, fetchChapters: false }}) {{ manga {{ ...MangaFields }} }} }}"
        );
        #[derive(Deserialize)]
        struct DetailPayload {
            manga: SuwayomiManga,
        }
        #[derive(Deserialize)]
        struct DetailData {
            #[serde(rename = "fetchMangaAndChapters")]
            f: DetailPayload,
        }
        match self.gql::<DetailData>(&detail, json!({ "id": id })).await {
            Ok(d) => Ok(d.f.manga),
            Err(_) => {
                let doc = format!(
                    "{MANGA_FIELDS}\nquery M($id: Int!) {{ manga(id: $id) {{ ...MangaFields }} }}"
                );
                #[derive(Deserialize)]
                struct Data {
                    manga: SuwayomiManga,
                }
                let d: Data = self.gql(&doc, json!({ "id": id })).await?;
                Ok(d.manga)
            }
        }
    }

    pub async fn chapters(&self, series_id: i64) -> Result<Vec<SuwayomiChapter>> {
        let fetch = format!(
            "{CHAPTER_FIELDS}\n\
             mutation FC($id: Int!) {{ fetchMangaAndChapters(input: {{ id: $id, fetchManga: false, fetchChapters: true }}) {{ chapters {{ ...ChapterFields }} }} }}"
        );
        #[derive(Deserialize)]
        struct FetchPayload {
            chapters: Option<Vec<SuwayomiChapter>>,
        }
        #[derive(Deserialize)]
        struct FetchData {
            #[serde(rename = "fetchMangaAndChapters")]
            f: FetchPayload,
        }
        match self
            .gql::<FetchData>(&fetch, json!({ "id": series_id }))
            .await
        {
            Ok(d) => Ok(d.f.chapters.unwrap_or_default()),
            Err(e) => {
                if e.to_string().contains("No chapters") {
                    return Ok(vec![]);
                }
                let doc = format!(
                    "{CHAPTER_FIELDS}\n\
                     query C($id: Int!) {{ chapters(condition: {{ mangaId: $id }}, order: {{ by: SOURCE_ORDER, byType: DESC }}) {{ nodes {{ ...ChapterFields }} }} }}"
                );
                #[derive(Deserialize)]
                struct Nodes {
                    nodes: Vec<SuwayomiChapter>,
                }
                #[derive(Deserialize)]
                struct Data {
                    chapters: Nodes,
                }
                let d: Data = self.gql(&doc, json!({ "id": series_id })).await?;
                Ok(d.chapters.nodes)
            }
        }
    }

    pub async fn pages(&self, chapter_id: i64) -> Result<Vec<String>> {
        let doc =
            "mutation P($id: Int!) { fetchChapterPages(input: { chapterId: $id }) { pages } }";
        #[derive(Deserialize)]
        struct Payload {
            pages: Vec<String>,
        }
        #[derive(Deserialize)]
        struct Data {
            #[serde(rename = "fetchChapterPages")]
            fetch_chapter_pages: Payload,
        }
        let d: Data = self.gql(doc, json!({ "id": chapter_id })).await?;
        Ok(d.fetch_chapter_pages
            .pages
            .iter()
            .map(|u| self.abs(Some(u)))
            .collect())
    }

    pub async fn library(&self) -> Result<Vec<SuwayomiManga>> {
        let doc = format!(
            "{MANGA_FIELDS}\nquery L {{ mangas(condition: {{ inLibrary: true }}) {{ nodes {{ ...MangaFields }} }} }}"
        );
        #[derive(Deserialize)]
        struct Nodes {
            nodes: Vec<SuwayomiManga>,
        }
        #[derive(Deserialize)]
        struct Data {
            mangas: Nodes,
        }
        let d: Data = self.gql(&doc, json!({})).await?;
        Ok(d.mangas.nodes)
    }

    pub async fn set_in_library(&self, id: i64, in_library: bool) -> Result<()> {
        let doc = "mutation U($id: Int!, $inLibrary: Boolean!) { updateManga(input: { id: $id, patch: { inLibrary: $inLibrary } }) { manga { id } } }";
        let _: Value = self
            .gql(doc, json!({ "id": id, "inLibrary": in_library }))
            .await?;
        Ok(())
    }

    pub async fn set_progress(&self, id: i64, last_page_read: i64, is_read: bool) -> Result<()> {
        let doc = "mutation U($id: Int!, $lastPageRead: Int!, $isRead: Boolean!) { updateChapter(input: { id: $id, patch: { lastPageRead: $lastPageRead, isRead: $isRead } }) { chapter { id } } }";
        let _: Value = self
            .gql(
                doc,
                json!({ "id": id, "lastPageRead": last_page_read, "isRead": is_read }),
            )
            .await?;
        Ok(())
    }
}
