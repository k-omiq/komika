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

/// One installed extension as reported by Suwayomi, flattened to the coordinates a
/// native device needs to install it (§2.1). One extension can back several sources
/// (e.g. an "all" extension), so `source_ids` carries every source it provides.
///
/// NOTE (could_not_verify): the upstream `ExtensionType` field names below are a
/// best guess and cannot be confirmed without a live operator Suwayomi. Parsing is
/// deliberately lenient (Option fields + `#[serde(default)]`) so a shape mismatch
/// degrades to empty/None rather than panicking. The query lives in one place
/// (`fetch_extensions`) so it's easy to correct once the real schema is checked.
#[derive(Debug, Clone)]
pub struct SuwayomiExtension {
    pub pkg_name: String,
    pub repo: Option<String>,
    pub apk_name: Option<String>,
    pub version_code: Option<i64>,
    pub lang: Option<String>,
    pub is_nsfw: bool,
    /// Suwayomi source ids this extension provides (join key to `source_series`).
    pub source_ids: Vec<String>,
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

/// One extension row from the configured extension stores — the admin-facing
/// management view (EXT-1), covering the FULL store index, not just installed
/// ones (`SuwayomiExtension` is the installed-only catalogue view). Verified
/// against Suwayomi v2.3.2243 `ExtensionType` (GQL-SCHEMA-FINDINGS.md §B2).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionListEntry {
    pub pkg_name: String,
    pub name: String,
    pub lang: String,
    pub version_name: String,
    pub is_installed: bool,
    pub has_update: bool,
    pub is_nsfw: bool,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
}

/// One installed Suwayomi source, flattened for the admin picker (EXT-1): the
/// coordinates the UI needs to feed `sourceBrowse(sourceId)`. `name` is the
/// user-facing display name; `pkg_name` joins back to the owning extension.
#[derive(Debug, Clone)]
pub struct SuwayomiSource {
    pub id: String,
    pub name: String,
    pub lang: String,
    pub is_nsfw: bool,
    pub icon_url: Option<String>,
    pub pkg_name: Option<String>,
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

/// Parse a JSON array element-by-element, skipping (and counting) records that
/// don't deserialize, so one malformed node (e.g. a manga with a null title, or a
/// chapter with a null name) doesn't fail the whole page/list. Mirrors the
/// per-record parse in `mangadex::list_manga`.
fn parse_records<T: DeserializeOwned>(raw: Vec<Value>, what: &str) -> Vec<T> {
    let mut out = Vec::with_capacity(raw.len());
    let mut skipped = 0usize;
    for v in raw {
        match serde_json::from_value::<T>(v) {
            Ok(x) => out.push(x),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        tracing::warn!(
            skipped,
            kind = what,
            "suwayomi: skipped unparseable records"
        );
    }
    out
}

#[derive(Clone)]
pub struct SuwayomiClient {
    base_url: String,
    /// Public base used when building image URLs (covers/pages) handed to the
    /// browser. Defaults to `base_url` when no public URL is configured.
    image_base_url: String,
    http: reqwest::Client,
    /// Cached resolved source id; `Some` once resolved. `Arc<Mutex>` so cheap clones
    /// (e.g. into the cover drainer task) share the one resolved id rather than each
    /// re-resolving it against the engine.
    source_id: std::sync::Arc<Mutex<Option<String>>>,
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
        // Bound every request so a hung/slow upstream (Suwayomi proxies to source
        // sites, often via FlareSolverr, which routinely stalls) can't freeze the
        // scan scheduler or a reader cache-miss path forever. Mirrors
        // `MangaDexClient::new`. Falls back to the default client if the builder
        // somehow fails, so construction stays infallible.
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base_url,
            image_base_url,
            http,
            source_id: std::sync::Arc::new(Mutex::new(configured_source.clone())),
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

    /// Fetch a cover thumbnail's raw bytes for server-side pHash (dedup signal,
    /// DD1). Uses the INTERNAL `base_url` (this runs server-side, not in the
    /// browser, so the public image host may be unreachable). Best-effort: returns
    /// `None` on any missing URL / network / non-success — a missing hash just
    /// means one fewer dedup signal, never a failure.
    pub async fn cover_bytes(&self, thumbnail_url: Option<&str>) -> Option<Vec<u8>> {
        let raw = thumbnail_url?;
        if raw.is_empty() {
            return None;
        }
        let url = if raw.starts_with("http") {
            raw.to_string()
        } else {
            format!("{}{}", self.base_url, raw)
        };
        let res = self.http.get(url).send().await.ok()?;
        if !res.status().is_success() {
            return None;
        }
        Some(res.bytes().await.ok()?.to_vec())
    }

    /// Fetch a Suwayomi image (a cover thumbnail or a chapter page) by its internal
    /// REST path, from the INTERNAL loopback host (`base_url`, e.g. localhost:4567) —
    /// NOT `image_base_url`. Suwayomi is not publicly exposed, so the browser can't
    /// reach `:4567`; instead the public cover/page URLs are built against our own
    /// origin (SUWAYOMI_PUBLIC_URL=https://api.komiq.cc) and this method fetches the
    /// bytes server-side so `serve_suwayomi_image` can re-serve them. Fetching against
    /// `image_base_url` would loop back into ourselves — always use `base_url`.
    ///
    /// `path` is a server-validated image path (see `main::is_suwayomi_image_path`),
    /// so it is trusted. Returns the raw bytes + upstream `Content-Type`.
    pub async fn fetch_image(&self, path: &str) -> Result<(Vec<u8>, String)> {
        let url = format!("{}{}", self.base_url, path);
        let res = self.http.get(url).send().await?;
        if !res.status().is_success() {
            return Err(anyhow!("Suwayomi image error {}", res.status()));
        }
        let content_type = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .filter(|ct| ct.starts_with("image/"))
            .unwrap_or("image/jpeg")
            .to_string();
        let bytes = res.bytes().await?.to_vec();
        Ok((bytes, content_type))
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
    ///
    /// Double-checked locking: the network resolve (`sources` query) runs WITHOUT the
    /// cache lock held, so concurrent first-callers don't serialize behind a single
    /// round-trip. A benign race may resolve twice, but the stored value is stable
    /// (it's process-lifetime; the first writer wins and later writers store the same
    /// deterministic pick), so the extra query is harmless.
    async fn resolve_source(&self) -> Result<String> {
        // Fast path: already cached.
        if let Some(id) = self.source_id.lock().await.as_ref() {
            return Ok(id.clone());
        }
        // A configured source needs no network round-trip; cache and return.
        if let Some(id) = &self.configured_source {
            let mut guard = self.source_id.lock().await;
            let id = guard.get_or_insert_with(|| id.clone()).clone();
            return Ok(id);
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
        // Resolve over the network with NO lock held.
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
        // Re-acquire and store. If another caller won the race, keep their value.
        let mut guard = self.source_id.lock().await;
        let id = guard.get_or_insert_with(|| chosen.id.clone()).clone();
        Ok(id)
    }

    pub async fn fetch_source(
        &self,
        ty: FetchType,
        page: i32,
        query: Option<&str>,
    ) -> Result<(bool, Vec<SuwayomiManga>)> {
        let source = self.resolve_source().await?;
        self.browse_source(&source, ty, page, query).await
    }

    /// Browse/search one EXPLICIT source (`fetchSourceManga`, GQL-SCHEMA-FINDINGS.md
    /// §A1) — the admin "Sources & Extensions" surface picks the source id itself
    /// instead of going through the resolved default (EXT-1). Every returned manga
    /// is persisted by Suwayomi and gets an internal id (§A0), which is what the
    /// bulk-ingest flow feeds to `add_source_series`.
    pub async fn browse_source(
        &self,
        source_id: &str,
        ty: FetchType,
        page: i32,
        query: Option<&str>,
    ) -> Result<(bool, Vec<SuwayomiManga>)> {
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
            mangas: Vec<Value>,
        }
        #[derive(Deserialize)]
        struct Data {
            #[serde(rename = "fetchSourceManga")]
            fetch_source_manga: Payload,
        }
        let data: Data = self
            .gql(
                &doc,
                json!({ "source": source_id, "type": ty.as_str(), "page": page, "query": query }),
            )
            .await?;
        Ok((
            data.fetch_source_manga.has_next_page,
            parse_records::<SuwayomiManga>(data.fetch_source_manga.mangas, "browse_manga"),
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
            chapters: Option<Vec<Value>>,
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
            Ok(d) => Ok(parse_records::<SuwayomiChapter>(
                d.f.chapters.unwrap_or_default(),
                "chapters",
            )),
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
                    nodes: Vec<Value>,
                }
                #[derive(Deserialize)]
                struct Data {
                    chapters: Nodes,
                }
                let d: Data = self.gql(&doc, json!({ "id": series_id })).await?;
                Ok(parse_records::<SuwayomiChapter>(
                    d.chapters.nodes,
                    "chapters",
                ))
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

    /// Resolve the owning manga id for a Suwayomi chapter, for NSFW-gating `pages`.
    /// Suwayomi chapter ids are sequential integers, so a viewer who hasn't opted in
    /// could otherwise hand-craft one to read an NSFW series' page images — the gate
    /// needs the owning series, which isn't mirrored locally for Suwayomi sources.
    /// `None` if the chapter is unknown to the source server.
    pub async fn chapter_manga_id(&self, chapter_id: i64) -> Result<Option<i64>> {
        let doc = "query CM($id: Int!) { chapters(condition: { id: $id }) { nodes { mangaId } } }";
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Node {
            manga_id: i64,
        }
        #[derive(Deserialize)]
        struct Nodes {
            nodes: Vec<Node>,
        }
        #[derive(Deserialize)]
        struct Data {
            chapters: Nodes,
        }
        let d: Data = self.gql(doc, json!({ "id": chapter_id })).await?;
        Ok(d.chapters.nodes.first().map(|n| n.manga_id))
    }

    pub async fn library(&self) -> Result<Vec<SuwayomiManga>> {
        let doc = format!(
            "{MANGA_FIELDS}\nquery L {{ mangas(condition: {{ inLibrary: true }}) {{ nodes {{ ...MangaFields }} }} }}"
        );
        #[derive(Deserialize)]
        struct Nodes {
            nodes: Vec<Value>,
        }
        #[derive(Deserialize)]
        struct Data {
            mangas: Nodes,
        }
        let d: Data = self.gql(&doc, json!({})).await?;
        Ok(parse_records::<SuwayomiManga>(d.mangas.nodes, "library"))
    }

    /// List the installed extensions and their coordinates (§2.1), so the catalogue
    /// can record, per source id, the exact extension a device must install. Only
    /// installed extensions are relevant (they're what actually backs a source), so
    /// the query filters on `isInstalled`.
    ///
    /// Best-effort + could_not_verify: the `ExtensionType` shape is a best guess and
    /// unconfirmed without a live Suwayomi. Parsing is lenient (see `SuwayomiExtension`)
    /// so a schema mismatch surfaces as an error the caller logs and swallows, never a
    /// panic. Keep the query text here — it's the single place to fix if the real
    /// upstream field names differ.
    pub async fn fetch_extensions(&self) -> Result<Vec<SuwayomiExtension>> {
        let doc = "query Extensions { \
            extensions(condition: { isInstalled: true }) { \
              nodes { pkgName repo apkName versionCode lang isNsfw source { nodes { id } } } } }";
        #[derive(Deserialize)]
        struct SourceId {
            id: String,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct SourceNodes {
            nodes: Vec<SourceId>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Node {
            pkg_name: String,
            #[serde(default)]
            repo: Option<String>,
            #[serde(default)]
            apk_name: Option<String>,
            #[serde(default)]
            version_code: Option<i64>,
            #[serde(default)]
            lang: Option<String>,
            #[serde(default)]
            is_nsfw: bool,
            #[serde(default)]
            source: SourceNodes,
        }
        #[derive(Deserialize)]
        struct Nodes {
            nodes: Vec<Node>,
        }
        #[derive(Deserialize)]
        struct Data {
            extensions: Nodes,
        }
        let data: Data = self.gql(doc, json!({})).await?;
        Ok(data
            .extensions
            .nodes
            .into_iter()
            .map(|n| SuwayomiExtension {
                pkg_name: n.pkg_name,
                repo: n.repo,
                apk_name: n.apk_name,
                version_code: n.version_code,
                lang: n.lang,
                is_nsfw: n.is_nsfw,
                source_ids: n.source.nodes.into_iter().map(|s| s.id).collect(),
            })
            .collect())
    }

    // ---- Extension management (EXT-1, GQL-SCHEMA-FINDINGS.md §B) ------------

    /// Number of configured extension stores (repos). Used by the idempotent
    /// Keiyoushi default-store seeding: Suwayomi canonicalizes the index URL on
    /// add (min.json → index.pb), so presence can't be checked by URL equality.
    pub async fn extension_store_count(&self) -> Result<i64> {
        let doc = "query { extensionStores { totalCount } }";
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Stores {
            total_count: i64,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Data {
            extension_stores: Stores,
        }
        let d: Data = self.gql(doc, json!({})).await?;
        Ok(d.extension_stores.total_count)
    }

    /// Register an extension store (repo) by its index URL. Idempotent upstream:
    /// re-adding an existing store returns it unchanged (verified on v2.3.2243).
    /// Returns the store's display name.
    pub async fn add_extension_store(&self, index_url: &str) -> Result<String> {
        let doc = "mutation A($url: String!) { \
            addExtensionStore(input: { indexUrl: $url }) { extensionStore { name } } }";
        #[derive(Deserialize)]
        struct Store {
            name: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Payload {
            extension_store: Store,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Data {
            add_extension_store: Payload,
        }
        let d: Data = self.gql(doc, json!({ "url": index_url })).await?;
        Ok(d.add_extension_store.extension_store.name)
    }

    /// Refresh the available-extension list from every configured store
    /// (`fetchExtensions` — a network fetch of each store index). Returns how
    /// many extensions are now known.
    pub async fn refresh_extensions(&self) -> Result<i64> {
        let doc = "mutation { fetchExtensions(input: {}) { extensions { pkgName } } }";
        #[derive(Deserialize)]
        struct Payload {
            extensions: Vec<Value>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Data {
            fetch_extensions: Payload,
        }
        let d: Data = self.gql(doc, json!({})).await?;
        Ok(d.fetch_extensions.extensions.len() as i64)
    }

    /// List ALL extensions known from the configured stores (installed or not) —
    /// the admin management view. `fetch_extensions` above stays the installed-only
    /// catalogue view (§2.1 coordinates); this one is the EXT-1 surface.
    pub async fn list_extensions(&self) -> Result<Vec<ExtensionListEntry>> {
        let doc = "query { extensions { \
            nodes { pkgName name lang versionName isInstalled hasUpdate isNsfw iconUrl repo } } }";
        #[derive(Deserialize)]
        struct Nodes {
            nodes: Vec<ExtensionListEntry>,
        }
        #[derive(Deserialize)]
        struct Data {
            extensions: Nodes,
        }
        let d: Data = self.gql(doc, json!({})).await?;
        Ok(d.extensions.nodes)
    }

    /// Shared `updateExtension(input:{id, patch})` call — `id` is the pkgName and
    /// the patch carries exactly one of install/uninstall/update (§B3). Returns
    /// the extension's post-mutation state.
    async fn patch_extension(&self, pkg_name: &str, patch: Value) -> Result<ExtensionListEntry> {
        let doc = "mutation U($id: String!, $patch: UpdateExtensionPatchInput!) { \
            updateExtension(input: { id: $id, patch: $patch }) { \
              extension { pkgName name lang versionName isInstalled hasUpdate isNsfw iconUrl repo } } }";
        #[derive(Deserialize)]
        struct Payload {
            extension: Option<ExtensionListEntry>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Data {
            update_extension: Payload,
        }
        let d: Data = self
            .gql(doc, json!({ "id": pkg_name, "patch": patch }))
            .await?;
        d.update_extension
            .extension
            .ok_or_else(|| anyhow!("Suwayomi returned no extension for {pkg_name}"))
    }

    pub async fn install_extension(&self, pkg_name: &str) -> Result<ExtensionListEntry> {
        self.patch_extension(pkg_name, json!({ "install": true }))
            .await
    }

    pub async fn uninstall_extension(&self, pkg_name: &str) -> Result<ExtensionListEntry> {
        self.patch_extension(pkg_name, json!({ "uninstall": true }))
            .await
    }

    pub async fn update_extension(&self, pkg_name: &str) -> Result<ExtensionListEntry> {
        self.patch_extension(pkg_name, json!({ "update": true }))
            .await
    }

    /// One extension by pkgName (`extension(pkgName:)`, §B2) — used to check the
    /// NSFW flag before an install/update is allowed through the posture gate.
    pub async fn get_extension(&self, pkg_name: &str) -> Result<ExtensionListEntry> {
        let doc = "query E($pkg: String!) { extension(pkgName: $pkg) { \
            pkgName name lang versionName isInstalled hasUpdate isNsfw iconUrl repo } }";
        #[derive(Deserialize)]
        struct Data {
            extension: ExtensionListEntry,
        }
        let d: Data = self.gql(doc, json!({ "pkg": pkg_name })).await?;
        Ok(d.extension)
    }

    /// List the installed Suwayomi sources (`sources` query, §B4) — the admin
    /// picker that feeds `sourceBrowse(sourceId)`. Lenient on the nested
    /// extension so a shape mismatch degrades to `pkg_name: None`, not an error.
    pub async fn list_sources(&self) -> Result<Vec<SuwayomiSource>> {
        let doc = "query { sources { nodes { \
            id name displayName lang isNsfw iconUrl extension { pkgName } } } }";
        #[derive(Deserialize, Default)]
        #[serde(default, rename_all = "camelCase")]
        struct Ext {
            pkg_name: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Node {
            id: String,
            name: String,
            #[serde(default)]
            display_name: Option<String>,
            lang: String,
            is_nsfw: bool,
            #[serde(default)]
            icon_url: Option<String>,
            #[serde(default)]
            extension: Option<Ext>,
        }
        #[derive(Deserialize)]
        struct Nodes {
            nodes: Vec<Node>,
        }
        #[derive(Deserialize)]
        struct Data {
            sources: Nodes,
        }
        let d: Data = self.gql(doc, json!({})).await?;
        Ok(d.sources
            .nodes
            .into_iter()
            .map(|n| SuwayomiSource {
                id: n.id,
                // displayName is the user-facing one (e.g. per-language variants);
                // fall back to the raw name when absent/blank.
                name: match n.display_name {
                    Some(d) if !d.is_empty() => d,
                    _ => n.name,
                },
                lang: n.lang,
                is_nsfw: n.is_nsfw,
                icon_url: n.icon_url,
                pkg_name: n.extension.and_then(|e| e.pkg_name),
            })
            .collect())
    }

    /// A source's display name + NSFW flag, for gating the admin browse surface
    /// on the show_nsfw posture (a source id is user input there).
    pub async fn source_meta(&self, source_id: &str) -> Result<(String, bool)> {
        let doc = "query S($id: LongString!) { source(id: $id) { displayName isNsfw } }";
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Src {
            display_name: String,
            is_nsfw: bool,
        }
        #[derive(Deserialize)]
        struct Data {
            source: Src,
        }
        let d: Data = self.gql(doc, json!({ "id": source_id })).await?;
        Ok((d.source.display_name, d.source.is_nsfw))
    }

    pub async fn set_in_library(&self, id: i64, in_library: bool) -> Result<()> {
        let doc = "mutation U($id: Int!, $inLibrary: Boolean!) { updateManga(input: { id: $id, patch: { inLibrary: $inLibrary } }) { manga { id } } }";
        let _: Value = self
            .gql(doc, json!({ "id": id, "inLibrary": in_library }))
            .await?;
        Ok(())
    }

    // NOTE: reading progress is no longer pushed back to Suwayomi — it is per-user and
    // lives in `suwayomi_progress` (see the `set_progress` GraphQL mutation). Suwayomi
    // is a content source only, so the old `updateChapter` progress mutation was removed.
}
