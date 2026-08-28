use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
};

use zed_extension_api::{self as zed, Result};

const LANGUAGE_SERVER_ID: &str = "composer-language-server";
const SERVER_VERSION: &str = "0.2.5";
const SERVER_NAME: &str = "composer-language-server";
const CACHE_DIRECTORY_ENV: &str = "COMPOSER_LANGUAGE_SERVER_CACHE_DIR";
const MIN_SERVER_BYTES: u64 = 64 * 1024;

struct ComposerExtension;

#[derive(Clone, Copy)]
enum ExecutableFormat {
    Elf,
    MachO,
    Windows,
}

struct ServerPlatform {
    target: &'static str,
    fallback_targets: &'static [&'static str],
    executable_suffix: &'static str,
    format: ExecutableFormat,
}

impl ComposerExtension {
    fn work_dir() -> Result<PathBuf> {
        env::current_dir()
            .map_err(|error| format!("failed to locate the extension work directory: {error}"))
    }

    fn platform() -> Result<ServerPlatform> {
        use zed::{Architecture, Os};

        let (os, architecture) = zed::current_platform();
        let platform = match (os, architecture) {
            (Os::Mac, Architecture::Aarch64) => ServerPlatform {
                target: "aarch64-apple-darwin",
                fallback_targets: &["aarch64-apple-darwin"],
                executable_suffix: "",
                format: ExecutableFormat::MachO,
            },
            (Os::Mac, Architecture::X8664) => ServerPlatform {
                target: "x86_64-apple-darwin",
                fallback_targets: &["x86_64-apple-darwin"],
                executable_suffix: "",
                format: ExecutableFormat::MachO,
            },
            (Os::Linux, Architecture::Aarch64) => ServerPlatform {
                target: "aarch64-unknown-linux-musl",
                fallback_targets: &["aarch64-unknown-linux-musl", "aarch64-unknown-linux-gnu"],
                executable_suffix: "",
                format: ExecutableFormat::Elf,
            },
            (Os::Linux, Architecture::X8664) => ServerPlatform {
                target: "x86_64-unknown-linux-musl",
                fallback_targets: &["x86_64-unknown-linux-musl", "x86_64-unknown-linux-gnu"],
                executable_suffix: "",
                format: ExecutableFormat::Elf,
            },
            (Os::Windows, Architecture::Aarch64) => ServerPlatform {
                target: "aarch64-pc-windows-msvc",
                fallback_targets: &["aarch64-pc-windows-msvc"],
                executable_suffix: ".exe",
                format: ExecutableFormat::Windows,
            },
            (Os::Windows, Architecture::X8664) => ServerPlatform {
                target: "x86_64-pc-windows-msvc",
                fallback_targets: &["x86_64-pc-windows-msvc"],
                executable_suffix: ".exe",
                format: ExecutableFormat::Windows,
            },
            (_, Architecture::X86) => {
                return Err(
                    "Composer LSP does not provide a language server for 32-bit systems".to_owned(),
                );
            }
        };
        Ok(platform)
    }

    fn is_valid_server(path: &Path, format: ExecutableFormat) -> bool {
        let Ok(metadata) = fs::metadata(path) else {
            return false;
        };
        if !metadata.is_file() || metadata.len() < MIN_SERVER_BYTES {
            return false;
        }

        let mut signature = [0_u8; 4];
        let read = fs::File::open(path)
            .and_then(|mut file| file.read(&mut signature))
            .unwrap_or(0);
        match format {
            ExecutableFormat::Elf => read == 4 && signature == *b"\x7fELF",
            ExecutableFormat::Windows => read >= 2 && signature[..2] == *b"MZ",
            ExecutableFormat::MachO => {
                read == 4
                    && matches!(
                        signature,
                        [0xfe, 0xed, 0xfa, 0xce]
                            | [0xce, 0xfa, 0xed, 0xfe]
                            | [0xfe, 0xed, 0xfa, 0xcf]
                            | [0xcf, 0xfa, 0xed, 0xfe]
                            | [0xca, 0xfe, 0xba, 0xbe]
                    )
            }
        }
    }

    fn fallback_server(
        work_dir: &Path,
        current_path: &Path,
        platform: &ServerPlatform,
    ) -> Option<PathBuf> {
        let suffixes: Vec<_> = platform
            .fallback_targets
            .iter()
            .map(|target| format!("-{target}{}", platform.executable_suffix))
            .collect();
        let mut candidates: Vec<_> = fs::read_dir(work_dir)
            .ok()?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path != current_path)
            .filter_map(|path| {
                let name = path.file_name()?.to_str()?;
                let (target_priority, suffix) = suffixes
                    .iter()
                    .enumerate()
                    .find(|(_, suffix)| name.ends_with(suffix.as_str()))?;
                let version = name
                    .strip_prefix(&format!("{SERVER_NAME}-"))?
                    .strip_suffix(suffix)?
                    .split('.')
                    .map(str::parse::<u64>)
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .ok()?;
                (!version.is_empty()).then_some((path, version, target_priority))
            })
            .filter(|(path, _, _)| Self::is_valid_server(path, platform.format))
            .collect();
        candidates.sort_unstable_by(|left, right| {
            right.1.cmp(&left.1).then_with(|| left.2.cmp(&right.2))
        });
        candidates
            .into_iter()
            .map(|(path, _, _)| path)
            .find(|path| zed::make_file_executable(path.to_string_lossy().as_ref()).is_ok())
    }

    fn server_path(language_server_id: &zed::LanguageServerId) -> Result<PathBuf> {
        let work_dir = Self::work_dir()?;
        let platform = Self::platform()?;

        let asset_name = format!(
            "{SERVER_NAME}-{}{}",
            platform.target, platform.executable_suffix
        );
        let path = work_dir.join(format!(
            "{SERVER_NAME}-{SERVER_VERSION}-{}{}",
            platform.target, platform.executable_suffix
        ));
        if Self::is_valid_server(&path, platform.format) {
            if zed::make_file_executable(path.to_string_lossy().as_ref()).is_ok() {
                return Ok(path);
            }
            if let Err(error) = fs::remove_file(&path) {
                if let Some(fallback) = Self::fallback_server(&work_dir, &path, &platform) {
                    return Ok(fallback);
                }
                return Err(format!(
                    "failed to repair or replace the cached Composer language server at {}: {error}",
                    path.display()
                ));
            }
        }

        if path.exists() {
            fs::remove_file(&path).map_err(|error| {
                format!(
                    "failed to replace an incomplete language-server download at {}: {error}",
                    path.display()
                )
            })?;
        }

        let download_url = format!(
            "https://github.com/BastenIT/zed-composer-support/releases/download/v{SERVER_VERSION}/{asset_name}"
        );
        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::Downloading,
        );
        let result = zed::download_file(
            &download_url,
            path.to_string_lossy().as_ref(),
            zed::DownloadedFileType::Uncompressed,
        )
        .and_then(|_| zed::make_file_executable(path.to_string_lossy().as_ref()));

        if let Err(error) = result {
            let _ = fs::remove_file(&path);
            if let Some(fallback) = Self::fallback_server(&work_dir, &path, &platform) {
                zed::set_language_server_installation_status(
                    language_server_id,
                    &zed::LanguageServerInstallationStatus::None,
                );
                return Ok(fallback);
            }
            let message = format!(
                "failed to install Composer language server {SERVER_VERSION} for {}: {error}. No previously installed native server is available; check the v{SERVER_VERSION} GitHub release and Zed's download permissions",
                platform.target
            );
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Failed(message.clone()),
            );
            return Err(message);
        }

        if !Self::is_valid_server(&path, platform.format) {
            if let Some(fallback) = Self::fallback_server(&work_dir, &path, &platform) {
                zed::set_language_server_installation_status(
                    language_server_id,
                    &zed::LanguageServerInstallationStatus::None,
                );
                return Ok(fallback);
            }
            let _ = fs::remove_file(&path);
            let message = format!(
                "downloaded Composer language server {SERVER_VERSION} for {} is not a valid executable",
                platform.target
            );
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Failed(message.clone()),
            );
            return Err(message);
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::None,
        );
        Ok(path)
    }
}

impl zed::Extension for ComposerExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        if language_server_id.as_ref() != LANGUAGE_SERVER_ID {
            return Err(format!("unknown language server: {language_server_id:?}"));
        }

        let cache_directory = Self::work_dir()?
            .join("cache")
            .to_string_lossy()
            .into_owned();
        let settings = zed::settings::LspSettings::for_worktree(LANGUAGE_SERVER_ID, worktree)?;
        if let Some(binary) = settings.binary {
            if let Some(command) = binary.path {
                let mut env: Vec<_> = binary.env.unwrap_or_default().into_iter().collect();
                if !env.iter().any(|(name, _)| name == CACHE_DIRECTORY_ENV) {
                    env.push((CACHE_DIRECTORY_ENV.to_owned(), cache_directory));
                }
                return Ok(zed::Command {
                    command,
                    args: binary.arguments.unwrap_or_default(),
                    env,
                });
            }
        }

        Ok(zed::Command {
            command: Self::server_path(language_server_id)?
                .to_string_lossy()
                .into_owned(),
            args: Vec::new(),
            env: vec![(CACHE_DIRECTORY_ENV.to_owned(), cache_directory)],
        })
    }
}

zed::register_extension!(ComposerExtension);
