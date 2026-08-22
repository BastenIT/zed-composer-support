# Changelog

## 0.3.0

- Check Packagist for newer stable package releases, with an option to disable network requests.
- Show installed-version hints immediately while update checks continue in the background.
- Refresh inlay hints when update metadata arrives.
- Bound network concurrency, cache growth, LSP message sizes, and installed metadata reads.
- Download the language server from a matching GitHub release instead of embedding it in the extension.
- Fall back to a valid previously installed server when an upgrade is temporarily unable to download the new one.
- Add cross-platform CI, release automation, and version consistency checks.

## 0.2.0

- Show compact installed-version hints from Composer 1 and Composer 2 metadata.
- Support custom Composer vendor directories.

## 0.1.0

- Add Packagist links for package names in Composer dependency sections.
