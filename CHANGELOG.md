# Changelog

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
