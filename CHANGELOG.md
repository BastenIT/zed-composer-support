# Changelog

## 0.2.5

- Rename the Zed extension to Composer LSP and use the LSP-specific `composer-lsp` extension ID.
- Use Zed's `lsp.composer-language-server.binary.path` setting for local server builds instead of detecting build artifacts automatically.

## 0.2.4

- Keep installed-version hints stable while Composer rewrites `installed.json` during an update.
- Prevent duplicate inlay hints from being returned for the same dependency position.

## 0.2.3

- Persist Packagist results and the hourly request budget across language-server and editor restarts.
- Coordinate cache access across worktrees and editor processes with file locking.
- Recover from malformed, oversized, or unwritable cache files without interrupting language-server features.
- Add a Neovim 0.10+ integration with native-server installation, inlay hints, and Packagist navigation.

## 0.2.2

- Bound document and installed-metadata processing, reject non-file metadata sources, and enforce the metadata byte limit while reading.
- Cache parsed documents and unchanged installed-package metadata to reduce repeated CPU and filesystem work.
- Reduce Packagist concurrency, extend cache lifetimes, add an hourly request budget, and require HTTPS across redirects.
- Retry cached-server permission setup and select offline fallbacks by numeric version.

## 0.2.1

- Ship self-contained musl binaries on Linux so the language server does not depend on a distribution's glibc version.
- Set explicit macOS deployment targets for Intel and Apple Silicon releases.
- Keep Linux 0.2.0 GNU binaries eligible as offline fallbacks during the transition.

## 0.2.0

- Replace the Node.js language server with a native Rust implementation.
- Build dedicated Intel and ARM binaries for macOS, Linux, and Windows.
- Validate downloaded executables before starting them and retain an older native server as an offline fallback.
- Preserve package links, installed-version hints, optional Packagist update checks, bounded caching, and background refreshes.
- Use a maintained range-aware JSON scanner while keeping partial-document recovery for active edits.

## 0.1.0

First public release.

- Link Composer dependency names to their Packagist pages.
- Show compact installed-version hints from Composer 1 and Composer 2 metadata.
- Support custom Composer vendor directories.
- Check Packagist for newer stable releases, with an option to disable network requests.
- Show installed versions immediately while update checks continue in the background.
- Refresh inlay hints when update metadata arrives.
- Handle missing metadata, offline requests, malformed messages, and failed downloads gracefully.
- Bound network concurrency, cache growth, LSP message sizes, and installed metadata reads.
