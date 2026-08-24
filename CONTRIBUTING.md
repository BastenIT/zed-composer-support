# Contributing

Small, focused pull requests are easiest to review. If a change affects package detection, installed metadata, version comparison, or LSP transport behavior, add a test that demonstrates it.

## Before opening a pull request

Run the complete local check:

```sh
cargo fmt --all --check
cargo test -p composer-language-server
cargo clippy -p composer-language-server --all-targets -- -D warnings
cargo check -p zed_composer_support --target wasm32-wasip2
```

Then run `cargo build -p composer-language-server`, install the repository as a Zed dev extension, and verify links and inlay hints in a real Composer project. Restart the Composer language server after rebuilding; reinstalling the extension alone may leave the existing server process running.

## Releasing

1. Add the release notes to `CHANGELOG.md`.
2. Set the same version in `extension.toml`, `Cargo.toml`, `server/Cargo.toml`, and `src/lib.rs`.
3. Run the full local check. The version-consistency test verifies the four version fields.
4. Commit and push the changes.
5. Tag the commit as `vX.Y.Z` and push the tag.

The release workflow creates a draft GitHub release, uploads native language-server binaries for all supported platforms, and publishes it after every build succeeds. Confirm the six assets are downloadable before submitting or updating the extension in `zed-industries/extensions`.
