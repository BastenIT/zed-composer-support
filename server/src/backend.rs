use std::{
    cmp::Ordering,
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering as AtomicOrdering},
        Arc,
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
        compare_versions, composer_path_from_uri, dependency_entries, installed_versions,
        is_update_section, newest_stable_version, offset_to_position, package_range,
        position_in_range, version_label,
    },
    SERVER_VERSION,
};

const PACKAGIST_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const PACKAGIST_ERROR_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const PACKAGIST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONCURRENT_REQUESTS: usize = 10;
const MAX_CACHE_ENTRIES: usize = 512;

#[derive(Clone, Debug)]
struct Document {
    text: String,
}

#[derive(Debug)]
enum CacheEntry {
    Pending,
    Ready {
        value: Option<String>,
        expires_at: Instant,
        inserted_at: Instant,
    },
}

#[derive(Debug)]
enum CacheLookup {
    Ready(Option<String>),
    Pending,
    Missing,
}

#[derive(Debug, Default)]
struct VersionCache {
    entries: HashMap<String, CacheEntry>,
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
        self.entries.insert(key, CacheEntry::Pending);
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
                inserted_at: now,
            },
        );
    }

    fn prune(&mut self, now: Instant) {
        self.entries.retain(
            |_, entry| !matches!(entry, CacheEntry::Ready { expires_at, .. } if *expires_at <= now),
        );
        if self.entries.len() < MAX_CACHE_ENTRIES {
            return;
        }

        let oldest = self
            .entries
            .iter()
            .filter_map(|(key, entry)| match entry {
                CacheEntry::Ready { inserted_at, .. } => Some((key.clone(), *inserted_at)),
                CacheEntry::Pending => None,
            })
            .min_by_key(|(_, inserted_at)| *inserted_at)
            .map(|(key, _)| key);
        if let Some(oldest) = oldest {
            self.entries.remove(&oldest);
        }
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
    documents: RwLock<HashMap<String, Document>>,
    check_updates: AtomicBool,
    updates: Arc<UpdateState>,
}

impl Backend {
    pub(crate) fn new(client: Client) -> Self {
        Self {
            client,
            documents: RwLock::new(HashMap::new()),
            check_updates: AtomicBool::new(true),
            updates: Arc::new(UpdateState::new()),
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
            Document {
                text: params.text_document.text,
            },
        );
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let Some(change) = params.content_changes.into_iter().last() else {
            return;
        };
        self.documents.write().await.insert(
            params.text_document.uri.to_string(),
            Document { text: change.text },
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

        let links = dependency_entries(&document.text)
            .into_iter()
            .filter_map(|dependency| {
                let target = format!("https://packagist.org/packages/{}", dependency.name)
                    .parse()
                    .ok()?;
                Some(DocumentLink {
                    range: package_range(&document.text, &dependency),
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
        let installed = tokio::task::spawn_blocking(move || {
            installed_versions(&installed_uri, &installed_text)
        })
        .await
        .unwrap_or_default();
        let mut hints = Vec::new();

        for dependency in dependency_entries(&document.text) {
            let Some(version) = installed.get(&dependency.name.to_ascii_lowercase()) else {
                continue;
            };
            let position = offset_to_position(&document.text, dependency.value_end);
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
    }
}
