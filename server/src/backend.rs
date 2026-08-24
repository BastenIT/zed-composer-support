use std::{
    cmp::Ordering,
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering as AtomicOrdering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, Instant},
};

use serde_json::Value;
use tokio::sync::{Mutex, RwLock, Semaphore};
use tower_lsp_server::{
    jsonrpc::Result,
    ls_types::{
        DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
        DocumentLink, DocumentLinkOptions, DocumentLinkParams, InitializeParams, InitializeResult,
        InlayHint, InlayHintLabel, InlayHintParams, InlayHintTooltip, OneOf, ServerCapabilities,
        ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind,
    },
    Client, LanguageServer,
};
use ureq::Agent;

use crate::{
    composer::{
        compare_versions, composer_path_from_uri, dependency_entries, dependency_position,
        is_update_section, newest_stable_version, package_range, position_in_range, version_label,
        DependencyEntry, InstalledVersionCache,
    },
    SERVER_VERSION,
};

const PACKAGIST_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const PACKAGIST_ERROR_CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const PACKAGIST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONCURRENT_REQUESTS: usize = 4;
const MAX_CACHE_ENTRIES: usize = 256;
const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const PACKAGIST_REQUEST_WINDOW: Duration = Duration::from_secs(60 * 60);
const MAX_PACKAGIST_REQUESTS_PER_WINDOW: usize = 256;

#[derive(Debug)]
struct Document {
    text: String,
    dependencies: Vec<DependencyEntry>,
}

impl Document {
    fn new(text: String) -> Self {
        if text.len() > MAX_DOCUMENT_BYTES {
            return Self {
                text: String::new(),
                dependencies: Vec::new(),
            };
        }
        let dependencies = dependency_entries(&text);
        Self { text, dependencies }
    }
}

#[derive(Debug)]
enum CacheEntry {
    Pending,
    Ready {
        value: Option<String>,
        expires_at: Instant,
    },
}

#[derive(Debug)]
enum CacheLookup {
    Ready(Option<String>),
    Pending,
    Missing,
}

#[derive(Debug)]
struct VersionCache {
    entries: HashMap<String, CacheEntry>,
    request_window_started: Instant,
    requests_in_window: usize,
}

impl Default for VersionCache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            request_window_started: Instant::now(),
            requests_in_window: 0,
        }
    }
}

impl VersionCache {
    fn get(&mut self, package_name: &str, now: Instant) -> CacheLookup {
        let key = package_name.to_ascii_lowercase();
        match self.entries.get(&key) {
            Some(CacheEntry::Pending) => CacheLookup::Pending,
            Some(CacheEntry::Ready {
                value, expires_at, ..
            }) if *expires_at > now => CacheLookup::Ready(value.clone()),
            Some(CacheEntry::Ready { .. }) => {
                self.entries.remove(&key);
                CacheLookup::Missing
            }
            None => CacheLookup::Missing,
        }
    }

    fn begin_request(&mut self, package_name: &str, now: Instant) -> bool {
        let key = package_name.to_ascii_lowercase();
        if !matches!(self.get(&key, now), CacheLookup::Missing) {
            return false;
        }

        self.prune(now);
        if self.entries.len() >= MAX_CACHE_ENTRIES {
            return false;
        }
        if now.saturating_duration_since(self.request_window_started) >= PACKAGIST_REQUEST_WINDOW {
            self.request_window_started = now;
            self.requests_in_window = 0;
        }
        if self.requests_in_window >= MAX_PACKAGIST_REQUESTS_PER_WINDOW {
            return false;
        }
        self.entries.insert(key, CacheEntry::Pending);
        self.requests_in_window += 1;
        true
    }

    fn finish_request(&mut self, package_name: &str, value: Option<String>, now: Instant) {
        let ttl = if value.is_some() {
            PACKAGIST_CACHE_TTL
        } else {
            PACKAGIST_ERROR_CACHE_TTL
        };
        self.entries.insert(
            package_name.to_ascii_lowercase(),
            CacheEntry::Ready {
                value,
                expires_at: now + ttl,
            },
        );
    }

    fn prune(&mut self, now: Instant) {
        self.entries.retain(
            |_, entry| !matches!(entry, CacheEntry::Ready { expires_at, .. } if *expires_at <= now),
        );
    }
}

#[derive(Debug)]
struct UpdateState {
    cache: Mutex<VersionCache>,
    request_limit: Arc<Semaphore>,
    agent: Agent,
    refresh_supported: AtomicBool,
    refresh_scheduled: AtomicBool,
}

impl UpdateState {
    fn new() -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(PACKAGIST_TIMEOUT))
            .https_only(true)
            .user_agent(format!(
                "zed-composer-support/{SERVER_VERSION} (+https://github.com/BastenIT/zed-composer-support)"
            ))
            .accept("application/json")
            .build();
        Self {
            cache: Mutex::new(VersionCache::default()),
            request_limit: Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS)),
            agent: config.into(),
            refresh_supported: AtomicBool::new(false),
            refresh_scheduled: AtomicBool::new(false),
        }
    }

    async fn cached_version(&self, package_name: &str) -> CacheLookup {
        self.cache.lock().await.get(package_name, Instant::now())
    }

    async fn queue(self: &Arc<Self>, package_name: String, client: Client) {
        if !self
            .cache
            .lock()
            .await
            .begin_request(&package_name, Instant::now())
        {
            return;
        }

        let state = Arc::clone(self);
        tokio::spawn(async move {
            let permit = Arc::clone(&state.request_limit).acquire_owned().await;
            let value = if permit.is_ok() {
                let agent = state.agent.clone();
                let requested_package = package_name.clone();
                tokio::task::spawn_blocking(move || {
                    request_latest_stable_version(&agent, &requested_package)
                })
                .await
                .ok()
                .flatten()
            } else {
                None
            };

            state
                .cache
                .lock()
                .await
                .finish_request(&package_name, value.clone(), Instant::now());
            if value.is_some() {
                state.schedule_refresh(client);
            }
        });
    }

    fn schedule_refresh(self: &Arc<Self>, client: Client) {
        if !self.refresh_supported.load(AtomicOrdering::Relaxed)
            || self.refresh_scheduled.swap(true, AtomicOrdering::AcqRel)
        {
            return;
        }

        let state = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            state
                .refresh_scheduled
                .store(false, AtomicOrdering::Release);
            let _ = client.inlay_hint_refresh().await;
        });
    }
}

fn request_latest_stable_version(agent: &Agent, package_name: &str) -> Option<String> {
    let url = format!("https://repo.packagist.org/p2/{package_name}.json");
    let metadata: Value = agent.get(&url).call().ok()?.body_mut().read_json().ok()?;
    newest_stable_version(&metadata, package_name)
}

#[derive(Debug)]
pub(crate) struct Backend {
    client: Client,
    documents: RwLock<HashMap<String, Arc<Document>>>,
    check_updates: AtomicBool,
    updates: Arc<UpdateState>,
    installed_cache: Arc<StdMutex<InstalledVersionCache>>,
}

impl Backend {
    pub(crate) fn new(client: Client) -> Self {
        Self {
            client,
            documents: RwLock::new(HashMap::new()),
            check_updates: AtomicBool::new(true),
            updates: Arc::new(UpdateState::new()),
            installed_cache: Arc::new(StdMutex::new(InstalledVersionCache::default())),
        }
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let check_updates = params
            .initialization_options
            .as_ref()
            .and_then(|options| options.get("check_updates"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        self.check_updates
            .store(check_updates, AtomicOrdering::Relaxed);

        let refresh_supported = serde_json::to_value(&params.capabilities)
            .ok()
            .and_then(|capabilities| {
                capabilities
                    .pointer("/workspace/inlayHint/refreshSupport")
                    .and_then(Value::as_bool)
            })
            .unwrap_or(false);
        self.updates
            .refresh_supported
            .store(refresh_supported, AtomicOrdering::Relaxed);

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                document_link_provider: Some(DocumentLinkOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: Default::default(),
                }),
                inlay_hint_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "composer-language-server".to_owned(),
                version: Some(SERVER_VERSION.to_owned()),
            }),
            offset_encoding: None,
        })
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.documents.write().await.insert(
            params.text_document.uri.to_string(),
            Arc::new(Document::new(params.text_document.text)),
        );
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let Some(change) = params.content_changes.into_iter().last() else {
            return;
        };
        self.documents.write().await.insert(
            params.text_document.uri.to_string(),
            Arc::new(Document::new(change.text)),
        );
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .write()
            .await
            .remove(&params.text_document.uri.to_string());
    }

    async fn document_link(&self, params: DocumentLinkParams) -> Result<Option<Vec<DocumentLink>>> {
        let uri = params.text_document.uri.to_string();
        if composer_path_from_uri(&uri).is_none() {
            return Ok(Some(Vec::new()));
        }
        let Some(document) = self.documents.read().await.get(&uri).cloned() else {
            return Ok(Some(Vec::new()));
        };

        let links = document
            .dependencies
            .iter()
            .filter_map(|dependency| {
                let target = format!("https://packagist.org/packages/{}", dependency.name)
                    .parse()
                    .ok()?;
                Some(DocumentLink {
                    range: package_range(dependency),
                    target: Some(target),
                    tooltip: Some(format!("Open {} on Packagist", dependency.name)),
                    data: None,
                })
            })
            .collect();
        Ok(Some(links))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri.to_string();
        if composer_path_from_uri(&uri).is_none() {
            return Ok(Some(Vec::new()));
        }
        let Some(document) = self.documents.read().await.get(&uri).cloned() else {
            return Ok(Some(Vec::new()));
        };
        let installed_uri = uri.clone();
        let installed_text = document.text.clone();
        let installed_cache = Arc::clone(&self.installed_cache);
        let installed = tokio::task::spawn_blocking(move || {
            installed_cache
                .lock()
                .map(|mut cache| cache.versions(&installed_uri, &installed_text))
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default();
        let mut hints = Vec::new();

        for dependency in &document.dependencies {
            let Some(version) = installed.get(&dependency.name.to_ascii_lowercase()) else {
                continue;
            };
            let position = dependency_position(dependency);
            if !position_in_range(position, params.range) {
                continue;
            }

            let mut latest = None;
            if self.check_updates.load(AtomicOrdering::Relaxed)
                && is_update_section(&dependency.section)
            {
                match self.updates.cached_version(&dependency.name).await {
                    CacheLookup::Ready(value) => latest = value,
                    CacheLookup::Missing => {
                        self.updates
                            .queue(dependency.name.clone(), self.client.clone())
                            .await;
                    }
                    CacheLookup::Pending => {}
                }
            }

            let installed_label = version_label(version);
            let update_available = latest
                .as_deref()
                .is_some_and(|latest| compare_versions(latest, version) == Some(Ordering::Greater));
            let (label, tooltip) = if update_available {
                let latest = latest.as_deref().unwrap_or_default();
                (
                    format!("{installed_label} → {}", version_label(latest)),
                    format!(
                        "{}: {installed_label} is installed; {} is the newest stable release on Packagist",
                        dependency.name,
                        version_label(latest)
                    ),
                )
            } else {
                (
                    installed_label,
                    format!("Version currently installed for {}", dependency.name),
                )
            };

            hints.push(InlayHint {
                position,
                label: InlayHintLabel::String(label),
                kind: None,
                text_edits: None,
                tooltip: Some(InlayHintTooltip::String(tooltip)),
                padding_left: Some(true),
                padding_right: None,
                data: None,
            });
        }

        Ok(Some(hints))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cache_deduplicates_requests_and_expires_failures() {
        let mut cache = VersionCache::default();
        let now = Instant::now();
        assert!(cache.begin_request("Vendor/Package", now));
        assert!(!cache.begin_request("vendor/package", now));
        assert!(matches!(
            cache.get("vendor/package", now),
            CacheLookup::Pending
        ));

        cache.finish_request("vendor/package", Some("v1.2.3".to_owned()), now);
        assert!(matches!(
            cache.get("VENDOR/PACKAGE", now),
            CacheLookup::Ready(Some(version)) if version == "v1.2.3"
        ));

        cache.finish_request("broken/package", None, now);
        assert!(matches!(
            cache.get("broken/package", now + PACKAGIST_ERROR_CACHE_TTL),
            CacheLookup::Missing
        ));
    }

    #[test]
    fn cache_capacity_and_request_budget_prevent_network_churn() {
        let mut cache = VersionCache::default();
        let now = Instant::now();
        for index in 0..MAX_CACHE_ENTRIES {
            let package = format!("vendor/package-{index}");
            assert!(cache.begin_request(&package, now));
            cache.finish_request(&package, None, now);
        }

        let after_failure_expiry = now + PACKAGIST_ERROR_CACHE_TTL;
        assert!(!cache.begin_request("vendor/extra", after_failure_expiry));
        assert!(cache.entries.is_empty());

        let after_budget_reset = now + PACKAGIST_REQUEST_WINDOW;
        assert!(cache.begin_request("vendor/extra", after_budget_reset));
    }

    #[test]
    fn oversized_documents_are_not_parsed_or_retained() {
        let document = Document::new("x".repeat(MAX_DOCUMENT_BYTES + 1));
        assert!(document.text.is_empty());
        assert!(document.dependencies.is_empty());
    }
}
