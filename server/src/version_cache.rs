use std::{
    collections::{HashMap, HashSet, VecDeque},
    env, fs,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    path::PathBuf,
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

pub(crate) const CACHE_DIRECTORY_ENV: &str = "COMPOSER_LANGUAGE_SERVER_CACHE_DIR";

const CACHE_SCHEMA_VERSION: u32 = 1;
const CACHE_FILE_NAME: &str = "packagist-versions-v1.json";
const CACHE_LOCK_FILE_NAME: &str = "packagist-versions.lock";
const CACHE_TTL_SECONDS: u64 = 6 * 60 * 60;
const ERROR_CACHE_TTL_SECONDS: u64 = 15 * 60;
const PENDING_TTL_SECONDS: u64 = 30;
const REQUEST_WINDOW_SECONDS: u64 = 60 * 60;
const MAX_REQUESTS_PER_WINDOW: usize = 256;
const MAX_CACHE_ENTRIES: usize = 256;
const MAX_CACHE_FILE_BYTES: u64 = 256 * 1024;
const MAX_PACKAGE_NAME_BYTES: usize = 256;
const MAX_VERSION_BYTES: usize = 256;
const CLOCK_SKEW_TOLERANCE_SECONDS: u64 = 5 * 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CacheLookup {
    Ready(Option<String>),
    Pending,
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RequestClaim {
    Started,
    Ready(Option<String>),
    Pending,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum CacheEntry {
    Pending {
        expires_at: u64,
    },
    Ready {
        value: Option<String>,
        expires_at: u64,
    },
}

impl CacheEntry {
    fn expires_at(&self) -> u64 {
        match self {
            Self::Pending { expires_at } | Self::Ready { expires_at, .. } => *expires_at,
        }
    }

    fn is_valid(&self, now: u64) -> bool {
        let expires_at = self.expires_at();
        let maximum_ttl = match self {
            Self::Pending { .. } => PENDING_TTL_SECONDS,
            Self::Ready { value: Some(_), .. } => CACHE_TTL_SECONDS,
            Self::Ready { value: None, .. } => ERROR_CACHE_TTL_SECONDS,
        };
        if expires_at <= now
            || expires_at
                > now
                    .saturating_add(maximum_ttl)
                    .saturating_add(CLOCK_SKEW_TOLERANCE_SECONDS)
        {
            return false;
        }

        match self {
            Self::Ready {
                value: Some(version),
                ..
            } => version.len() <= MAX_VERSION_BYTES,
            Self::Pending { .. } | Self::Ready { value: None, .. } => true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedCache {
    schema_version: u32,
    #[serde(default)]
    entries: HashMap<String, CacheEntry>,
    #[serde(default)]
    request_timestamps: VecDeque<u64>,
}

impl Default for PersistedCache {
    fn default() -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            entries: HashMap::new(),
            request_timestamps: VecDeque::new(),
        }
    }
}

impl PersistedCache {
    fn sanitize(&mut self, now: u64) {
        if self.schema_version != CACHE_SCHEMA_VERSION {
            *self = Self::default();
            return;
        }

        let mut entries: Vec<_> = self
            .entries
            .drain()
            .filter(|(package_name, entry)| {
                !package_name.is_empty()
                    && package_name.len() <= MAX_PACKAGE_NAME_BYTES
                    && entry.is_valid(now)
            })
            .collect();
        entries.sort_unstable_by_key(|(_, entry)| std::cmp::Reverse(entry.expires_at()));
        entries.truncate(MAX_CACHE_ENTRIES);
        self.entries = entries.into_iter().collect();

        let latest_allowed = now.saturating_add(CLOCK_SKEW_TOLERANCE_SECONDS);
        self.request_timestamps.retain(|timestamp| {
            *timestamp <= latest_allowed && now.saturating_sub(*timestamp) < REQUEST_WINDOW_SECONDS
        });
        self.request_timestamps.make_contiguous().sort_unstable();
        while self.request_timestamps.len() > MAX_REQUESTS_PER_WINDOW {
            self.request_timestamps.pop_front();
        }
    }

    fn lookup(&mut self, package_name: &str, now: u64) -> CacheLookup {
        let key = package_name.to_ascii_lowercase();
        match self.entries.get(&key) {
            Some(CacheEntry::Pending { expires_at }) if *expires_at > now => CacheLookup::Pending,
            Some(CacheEntry::Ready {
                value, expires_at, ..
            }) if *expires_at > now => CacheLookup::Ready(value.clone()),
            Some(_) => {
                self.entries.remove(&key);
                CacheLookup::Missing
            }
            None => CacheLookup::Missing,
        }
    }

    fn claim(&mut self, package_name: &str, now: u64) -> RequestClaim {
        let key = package_name.to_ascii_lowercase();
        match self.lookup(&key, now) {
            CacheLookup::Ready(value) => return RequestClaim::Ready(value),
            CacheLookup::Pending => return RequestClaim::Pending,
            CacheLookup::Missing => {}
        }

        self.sanitize(now);
        if self.request_timestamps.len() >= MAX_REQUESTS_PER_WINDOW {
            return RequestClaim::Rejected;
        }
        if self.entries.len() >= MAX_CACHE_ENTRIES {
            let oldest_ready = self
                .entries
                .iter()
                .filter(|(_, entry)| matches!(entry, CacheEntry::Ready { .. }))
                .min_by_key(|(_, entry)| entry.expires_at())
                .map(|(package_name, _)| package_name.clone());
            let Some(oldest_ready) = oldest_ready else {
                return RequestClaim::Rejected;
            };
            self.entries.remove(&oldest_ready);
        }

        self.entries.insert(
            key,
            CacheEntry::Pending {
                expires_at: now.saturating_add(PENDING_TTL_SECONDS),
            },
        );
        self.request_timestamps.push_back(now);
        RequestClaim::Started
    }

    fn finish(&mut self, package_name: &str, value: Option<String>, now: u64) {
        let ttl = if value.is_some() {
            CACHE_TTL_SECONDS
        } else {
            ERROR_CACHE_TTL_SECONDS
        };
        self.entries.insert(
            package_name.to_ascii_lowercase(),
            CacheEntry::Ready {
                value,
                expires_at: now.saturating_add(ttl),
            },
        );
        self.sanitize(now);
    }
}

#[derive(Clone, Debug)]
struct CacheStore {
    directory: PathBuf,
}

impl CacheStore {
    fn from_environment() -> Option<Self> {
        env::var_os(CACHE_DIRECTORY_ENV)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(default_cache_directory)
            .map(Self::new)
    }

    fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    fn data_path(&self) -> PathBuf {
        self.directory.join(CACHE_FILE_NAME)
    }

    fn lock_path(&self) -> PathBuf {
        self.directory.join(CACHE_LOCK_FILE_NAME)
    }

    fn load(&self, now: u64) -> io::Result<PersistedCache> {
        self.transaction(now, |_| {}).map(|(cache, ())| cache)
    }

    fn claim_many(
        &self,
        package_names: &[String],
        now: u64,
    ) -> io::Result<(PersistedCache, Vec<(String, RequestClaim)>)> {
        self.transaction(now, |cache| {
            let mut seen = HashSet::new();
            package_names
                .iter()
                .filter_map(|package_name| {
                    let key = package_name.to_ascii_lowercase();
                    seen.insert(key.clone())
                        .then(|| (key.clone(), cache.claim(&key, now)))
                })
                .collect()
        })
    }

    fn finish(
        &self,
        package_name: &str,
        value: Option<String>,
        now: u64,
    ) -> io::Result<PersistedCache> {
        self.transaction(now, |cache| cache.finish(package_name, value, now))
            .map(|(cache, ())| cache)
    }

    fn transaction<T>(
        &self,
        now: u64,
        operation: impl FnOnce(&mut PersistedCache) -> T,
    ) -> io::Result<(PersistedCache, T)> {
        fs::create_dir_all(&self.directory)?;
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.lock_path())?;
        lock_file.lock()?;

        let mut cache = self.read().unwrap_or_default();
        cache.sanitize(now);
        let result = operation(&mut cache);
        self.write(&cache)?;
        lock_file.unlock()?;
        Ok((cache, result))
    }

    fn read(&self) -> io::Result<PersistedCache> {
        let path = self.data_path();
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(PersistedCache::default());
            }
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() > MAX_CACHE_FILE_BYTES {
            return Ok(PersistedCache::default());
        }

        let mut contents = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_CACHE_FILE_BYTES.saturating_add(1))
            .read_to_end(&mut contents)?;
        if contents.len() as u64 > MAX_CACHE_FILE_BYTES {
            return Ok(PersistedCache::default());
        }

        Ok(serde_json::from_slice(&contents).unwrap_or_default())
    }

    fn write(&self, cache: &PersistedCache) -> io::Result<()> {
        let contents = serde_json::to_vec(cache).map_err(io::Error::other)?;
        if contents.len() as u64 > MAX_CACHE_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "persistent Packagist cache exceeds its size limit",
            ));
        }

        let temporary_path = self
            .directory
            .join(format!("{CACHE_FILE_NAME}.{}.tmp", process::id()));
        let mut temporary = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary_path)?;
        temporary.write_all(&contents)?;
        temporary.sync_all()?;
        drop(temporary);

        let destination = self.data_path();
        #[cfg(windows)]
        if destination.exists() {
            fs::remove_file(&destination)?;
        }
        if let Err(error) = fs::rename(&temporary_path, destination) {
            let _ = fs::remove_file(temporary_path);
            return Err(error);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct VersionCache {
    cache: PersistedCache,
    store: Option<CacheStore>,
    warned_about_storage: bool,
}

impl Default for VersionCache {
    fn default() -> Self {
        Self::new_at(None, unix_timestamp())
    }
}

impl VersionCache {
    pub(crate) fn from_environment() -> Self {
        Self::new_at(CacheStore::from_environment(), unix_timestamp())
    }

    fn new_at(store: Option<CacheStore>, now: u64) -> Self {
        let mut result = Self {
            cache: PersistedCache::default(),
            store,
            warned_about_storage: false,
        };
        if let Some(store) = result.store.clone() {
            match store.load(now) {
                Ok(cache) => result.cache = cache,
                Err(error) => result.warn_storage_error(&error),
            }
        }
        result
    }

    pub(crate) fn get(&mut self, package_name: &str) -> CacheLookup {
        self.get_at(package_name, unix_timestamp())
    }

    fn get_at(&mut self, package_name: &str, now: u64) -> CacheLookup {
        self.cache.lookup(package_name, now)
    }

    pub(crate) fn claim_requests(
        &mut self,
        package_names: &[String],
    ) -> Vec<(String, RequestClaim)> {
        self.claim_requests_at(package_names, unix_timestamp())
    }

    fn claim_requests_at(
        &mut self,
        package_names: &[String],
        now: u64,
    ) -> Vec<(String, RequestClaim)> {
        if let Some(store) = self.store.clone() {
            match store.claim_many(package_names, now) {
                Ok((cache, claims)) => {
                    self.cache = cache;
                    return claims;
                }
                Err(error) => self.warn_storage_error(&error),
            }
        }

        let mut seen = HashSet::new();
        package_names
            .iter()
            .filter_map(|package_name| {
                let key = package_name.to_ascii_lowercase();
                seen.insert(key.clone())
                    .then(|| (key.clone(), self.cache.claim(&key, now)))
            })
            .collect()
    }

    pub(crate) fn finish_request(&mut self, package_name: &str, value: Option<String>) {
        self.finish_request_at(package_name, value, unix_timestamp());
    }

    fn finish_request_at(&mut self, package_name: &str, value: Option<String>, now: u64) {
        if let Some(store) = self.store.clone() {
            match store.finish(package_name, value.clone(), now) {
                Ok(cache) => {
                    self.cache = cache;
                    return;
                }
                Err(error) => self.warn_storage_error(&error),
            }
        }
        self.cache.finish(package_name, value, now);
    }

    fn warn_storage_error(&mut self, error: &io::Error) {
        if self.warned_about_storage {
            return;
        }
        self.warned_about_storage = true;
        eprintln!(
            "composer-language-server: persistent cache is unavailable; using memory only: {error}"
        );
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(target_os = "windows")]
fn default_cache_directory() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(|path| path.join("composer-language-server"))
}

#[cfg(target_os = "macos")]
fn default_cache_directory() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(|path| path.join("Library/Caches/composer-language-server"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn default_cache_directory() -> Option<PathBuf> {
    env::var_os("XDG_CACHE_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .map(|path| path.join(".cache"))
        })
        .map(|path| path.join("composer-language-server"))
}

#[cfg(not(any(unix, target_os = "windows")))]
fn default_cache_directory() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::{Arc, Barrier};
    use tempfile::tempdir;

    const NOW: u64 = 1_700_000_000;

    fn persistent_cache(directory: &Path, now: u64) -> VersionCache {
        VersionCache::new_at(Some(CacheStore::new(directory.to_owned())), now)
    }

    #[test]
    fn successful_results_survive_a_restart() {
        let directory = tempdir().expect("temporary cache directory");
        let package = "vendor/package".to_owned();

        let mut first = persistent_cache(directory.path(), NOW);
        assert_eq!(
            first.claim_requests_at(std::slice::from_ref(&package), NOW),
            vec![(package.clone(), RequestClaim::Started)]
        );
        first.finish_request_at(&package, Some("v1.2.3".to_owned()), NOW);

        let mut restarted = persistent_cache(directory.path(), NOW + 1);
        assert_eq!(
            restarted.get_at(&package, NOW + 1),
            CacheLookup::Ready(Some("v1.2.3".to_owned()))
        );
        assert_eq!(
            restarted.claim_requests_at(std::slice::from_ref(&package), NOW + 1),
            vec![(package, RequestClaim::Ready(Some("v1.2.3".to_owned())))]
        );
    }

    #[test]
    fn request_budget_survives_a_restart() {
        let directory = tempdir().expect("temporary cache directory");
        let packages: Vec<_> = (0..MAX_REQUESTS_PER_WINDOW)
            .map(|index| format!("vendor/package-{index}"))
            .collect();

        let mut first = persistent_cache(directory.path(), NOW);
        assert!(first
            .claim_requests_at(&packages, NOW)
            .iter()
            .all(|(_, claim)| *claim == RequestClaim::Started));

        let mut restarted = persistent_cache(directory.path(), NOW + 1);
        assert_eq!(
            restarted.claim_requests_at(&["vendor/extra".to_owned()], NOW + 1),
            vec![("vendor/extra".to_owned(), RequestClaim::Rejected)]
        );
        assert_eq!(
            restarted
                .claim_requests_at(&["vendor/extra".to_owned()], NOW + REQUEST_WINDOW_SECONDS,),
            vec![("vendor/extra".to_owned(), RequestClaim::Started)]
        );
    }

    #[test]
    fn cache_instances_share_pending_and_ready_entries() {
        let directory = tempdir().expect("temporary cache directory");
        let package = "vendor/package".to_owned();
        let mut first = persistent_cache(directory.path(), NOW);
        let mut second = persistent_cache(directory.path(), NOW);

        assert_eq!(
            first.claim_requests_at(std::slice::from_ref(&package), NOW),
            vec![(package.clone(), RequestClaim::Started)]
        );
        assert_eq!(
            second.claim_requests_at(std::slice::from_ref(&package), NOW),
            vec![(package.clone(), RequestClaim::Pending)]
        );

        first.finish_request_at(&package, Some("v2.0.0".to_owned()), NOW + 1);
        assert_eq!(
            second.claim_requests_at(std::slice::from_ref(&package), NOW + 1),
            vec![(package, RequestClaim::Ready(Some("v2.0.0".to_owned())))]
        );
    }

    #[test]
    fn concurrent_claims_are_deduplicated() {
        let directory = tempdir().expect("temporary cache directory");
        let directory = Arc::new(directory.path().to_owned());
        let barrier = Arc::new(Barrier::new(2));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let directory = Arc::clone(&directory);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                let mut cache = persistent_cache(&directory, NOW);
                barrier.wait();
                cache
                    .claim_requests_at(&["vendor/package".to_owned()], NOW)
                    .pop()
                    .expect("request claim")
                    .1
            }));
        }

        let claims: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().expect("cache thread"))
            .collect();
        assert_eq!(
            claims
                .iter()
                .filter(|claim| **claim == RequestClaim::Started)
                .count(),
            1
        );
        assert_eq!(
            claims
                .iter()
                .filter(|claim| **claim == RequestClaim::Pending)
                .count(),
            1
        );
    }

    #[test]
    fn malformed_and_oversized_cache_files_are_replaced() {
        let directory = tempdir().expect("temporary cache directory");
        let store = CacheStore::new(directory.path().to_owned());
        fs::write(store.data_path(), b"not json").expect("malformed cache file");

        let mut cache = persistent_cache(directory.path(), NOW);
        assert_eq!(
            cache.claim_requests_at(&["vendor/package".to_owned()], NOW),
            vec![("vendor/package".to_owned(), RequestClaim::Started)]
        );

        fs::write(
            store.data_path(),
            vec![b'x'; MAX_CACHE_FILE_BYTES as usize + 1],
        )
        .expect("oversized cache file");
        let mut cache = persistent_cache(directory.path(), NOW + 1);
        assert_eq!(
            cache.claim_requests_at(&["vendor/other".to_owned()], NOW + 1),
            vec![("vendor/other".to_owned(), RequestClaim::Started)]
        );
    }

    #[test]
    fn cache_entries_with_implausible_expiry_times_are_discarded() {
        let mut cache = PersistedCache::default();
        cache.entries.insert(
            "vendor/pending".to_owned(),
            CacheEntry::Pending {
                expires_at: NOW + CACHE_TTL_SECONDS,
            },
        );
        cache.entries.insert(
            "vendor/failure".to_owned(),
            CacheEntry::Ready {
                value: None,
                expires_at: NOW + CACHE_TTL_SECONDS,
            },
        );

        cache.sanitize(NOW);

        assert!(cache.entries.is_empty());
    }

    #[test]
    fn full_cache_evicts_the_oldest_ready_entry() {
        let mut cache = PersistedCache::default();
        for index in 0..MAX_CACHE_ENTRIES {
            cache.entries.insert(
                format!("vendor/package-{index}"),
                CacheEntry::Ready {
                    value: Some("v1.0.0".to_owned()),
                    expires_at: NOW + CACHE_TTL_SECONDS - index as u64,
                },
            );
        }

        assert_eq!(cache.claim("vendor/new", NOW), RequestClaim::Started);
        assert_eq!(cache.entries.len(), MAX_CACHE_ENTRIES);
        assert!(!cache.entries.contains_key("vendor/package-255"));
        assert!(cache.entries.contains_key("vendor/new"));
    }
}
