# Contributing

Small, focused pull requests are easiest to review. If a change affects package detection, installed metadata, version comparison, or LSP transport behavior, add a test that demonstrates it.

## Before opening a pull request

Run the complete local check:

```sh
npm run check
cargo fmt --check
cargo check --target wasm32-wasip2
```

Then install the repository as a Zed dev extension and verify links and inlay hints in a real Composer project. Restart the Composer language server after changing `server/composer-language-server.js`; rebuilding the extension alone may leave the existing server process running.

## Releasing

1. Add the release notes to `CHANGELOG.md`.
2. Set the same version in `extension.toml`, `Cargo.toml`, `package.json`, `src/lib.rs`, and `server/composer-language-server.js`.
3. Run `node scripts/check-version.js X.Y.Z` and the full local check.
4. Commit and push the changes.
5. Tag the commit as `vX.Y.Z` and push the tag.

The release workflow creates a GitHub release and uploads the language-server script. Confirm the asset is downloadable before submitting or updating the extension in `zed-industries/extensions`.
