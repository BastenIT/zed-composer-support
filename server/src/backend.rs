use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering as AtomicOrdering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};

use serde_json::Value;
use tokio::sync::{RwLock, Semaphore};
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
    version_cache::{CacheLookup, RequestClaim, VersionCache},
    SERVER_VERSION,
};

const PACKAGIST_TIMEOUT: Duration = Duration::from_secs(5);
const PENDING_RECHECK_INTERVAL: Duration = Duration::from_secs(1);
const MAX_PENDING_RECHECKS: usize = 35;
const MAX_CONCURRENT_REQUESTS: usize = 4;
const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;

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
struct UpdateState {
    cache: Arc<StdMutex<VersionCache>>,
    request_limit: Arc<Semaphore>,
    agent: Agent,
    pending_rechecks: StdMutex<HashSet<String>>,
    refresh_supported: AtomicBool,
    refresh_scheduled: AtomicBool,
}

impl UpdateState {
    fn new() -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(PACKAGIST_TIMEOUT))
            .https_only(true)
            .user_agent(format!(
                "composer-language-server/{SERVER_VERSION} (+https://github.com/BastenIT/zed-composer-support)"
            ))
            .accept("application/json")
            .build();
        Self {
            cache: Arc::new(StdMutex::new(VersionCache::from_environment())),
            request_limit: Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS)),
            agent: config.into(),
            pending_rechecks: StdMutex::new(HashSet::new()),
            refresh_supported: AtomicBool::new(false),
            refresh_scheduled: AtomicBool::new(false),
        }
    }

    fn cached_version(&self, package_name: &str) -> CacheLookup {
        self.cache
            .lock()
            .map(|mut cache| cache.get(package_name))
            .unwrap_or(CacheLookup::Missing)
    }

    async fn queue(self: &Arc<Self>, package_names: Vec<String>, client: Client) {
        if package_names.is_empty() {
            return;
        }

        let claims = self.claim_requests(package_names).await;

        let mut refresh_from_cache = false;
        for (package_name, claim) in claims {
            match claim {
                RequestClaim::Ready(value) => refresh_from_cache |= value.is_some(),
                RequestClaim::Started => {
                    self.spawn_request(package_name, client.clone());
                }
                RequestClaim::Pending => {
                    self.schedule_pending_recheck(package_name, client.clone());
                }
                RequestClaim::Rejected => {}
            }
        }
        if refresh_from_cache {
            self.schedule_refresh(client);
        }
    }

    async fn claim_requests(&self, package_names: Vec<String>) -> Vec<(String, RequestClaim)> {
        let cache = Arc::clone(&self.cache);
        tokio::task::spawn_blocking(move || {
            cache
                .lock()
                .map(|mut cache| cache.claim_requests(&package_names))
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    }

    fn schedule_pending_recheck(self: &Arc<Self>, package_name: String, client: Client) {
        let scheduled = self
            .pending_rechecks
            .lock()
            .map(|mut pending| pending.insert(package_name.clone()))
            .unwrap_or(false);
        if !scheduled {
            return;
        }

        let state = Arc::clone(self);
        tokio::spawn(async move {
            for _ in 0..MAX_PENDING_RECHECKS {
                tokio::time::sleep(PENDING_RECHECK_INTERVAL).await;
                let mut claims = state.claim_requests(vec![package_name.clone()]).await;
                let Some((_, claim)) = claims.pop() else {
                    break;
                };
                match claim {
                    RequestClaim::Ready(value) => {
                        if value.is_some() {
                            state.schedule_refresh(client.clone());
                        }
                        break;
                    }
                    RequestClaim::Pending => continue,
                    RequestClaim::Started => {
                        state.spawn_request(package_name.clone(), client.clone());
                        break;
                    }
                    RequestClaim::Rejected => break,
                }
            }
            if let Ok(mut pending) = state.pending_rechecks.lock() {
                pending.remove(&package_name);
            }
        });
    }

    fn spawn_request(self: &Arc<Self>, package_name: String, client: Client) {
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
            drop(permit);

            let cache = Arc::clone(&state.cache);
            let completed_package = package_name.clone();
            let completed_value = value.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(mut cache) = cache.lock() {
                    cache.finish_request(&completed_package, completed_value);
                }
            })
            .await;
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
        let mut update_requests = Vec::new();

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
                match self.updates.cached_version(&dependency.name) {
                    CacheLookup::Ready(value) => latest = value,
                    CacheLookup::Missing => update_requests.push(dependency.name.clone()),
                    CacheLookup::Pending => update_requests.push(dependency.name.clone()),
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

        self.updates
            .queue(update_requests, self.client.clone())
            .await;

        Ok(Some(hints))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_documents_are_not_parsed_or_retained() {
        let document = Document::new("x".repeat(MAX_DOCUMENT_BYTES + 1));
        assert!(document.text.is_empty());
        assert!(document.dependencies.is_empty());
    }
}
