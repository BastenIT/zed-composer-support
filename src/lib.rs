use std::{
    env, fs,
    path::{Path, PathBuf},
};

use zed_extension_api::{self as zed, Result};

const LANGUAGE_SERVER_ID: &str = "composer-language-server";
const SERVER_FILE_NAME: &str = "composer-language-server.js";
const SERVER_VERSION: &str = "0.1.0";
const SERVER_DOWNLOAD_URL: &str =
    "https://github.com/BastenIT/zed-composer-support/releases/download/v0.1.0/composer-language-server.js";

struct ComposerExtension;

impl ComposerExtension {
    fn work_dir() -> Result<PathBuf> {
        env::current_dir()
            .map_err(|error| format!("failed to locate the extension work directory: {error}"))
    }

    fn is_valid_server(path: &Path) -> bool {
        fs::read_to_string(path)
            .map(|contents| {
                contents.len() > 1_024
                    && contents.starts_with("\"use strict\";")
                    && contents.contains("composer-language-server")
            })
            .unwrap_or(false)
    }

    fn server_path() -> Result<PathBuf> {
        let work_dir = Self::work_dir()?;

        // Prefer the source file when a development host uses the checkout as
        // its work directory. Published builds do not contain this path.
        let development_path = work_dir.join("server").join(SERVER_FILE_NAME);
        if Self::is_valid_server(&development_path) {
            return Ok(development_path);
        }

        let path = work_dir.join(format!("composer-language-server-{SERVER_VERSION}.js"));
        if Self::is_valid_server(&path) {
            return Ok(path);
        }

        // Older versions wrote the server under an unversioned filename. Keep
        // it as an offline fallback, but still try the matching release first.
        let legacy_path = work_dir.join(SERVER_FILE_NAME);

        if path.exists() {
            fs::remove_file(&path).map_err(|error| {
                format!(
                    "failed to replace an incomplete language-server download at {}: {error}",
                    path.display()
                )
            })?;
        }

        let download_result = zed::download_file(
            SERVER_DOWNLOAD_URL,
            path.to_string_lossy().as_ref(),
            zed::DownloadedFileType::Uncompressed,
        );

        if let Err(error) = download_result {
            if Self::is_valid_server(&legacy_path) {
                return Ok(legacy_path);
            }

            return Err(format!(
                "failed to download Composer language server {SERVER_VERSION}: {error}. No previously installed server is available; check your network connection and extension download permissions"
            ));
        }

        if !Self::is_valid_server(&path) {
            if Self::is_valid_server(&legacy_path) {
                return Ok(legacy_path);
            }

            return Err(format!(
                "downloaded Composer language server {SERVER_VERSION} is empty or incomplete, and no previously installed server is available"
            ));
        }

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

        let settings = zed::settings::LspSettings::for_worktree(LANGUAGE_SERVER_ID, worktree)?;
        if let Some(binary) = settings.binary {
            if let Some(command) = binary.path {
                return Ok(zed::Command {
                    command,
                    args: binary.arguments.unwrap_or_default(),
                    env: binary.env.unwrap_or_default().into_iter().collect(),
                });
            }
        }

        let server_path = Self::server_path()?;

        Ok(zed::Command {
            command: zed::node_binary_path()?,
            args: vec![server_path.to_string_lossy().into_owned()],
            env: Default::default(),
        })
    }
}

zed::register_extension!(ComposerExtension);
