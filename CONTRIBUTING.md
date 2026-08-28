# Contributing

Small, focused pull requests are easiest to review. If a change affects package detection, installed metadata, version comparison, or LSP transport behavior, add a test that demonstrates it.

## Before opening a pull request

Run the complete local check:

```sh
cargo fmt --all --check
cargo test -p composer-language-server
cargo clippy -p composer-language-server --all-targets -- -D warnings
cargo check -p zed_composer_support --target wasm32-wasip2
nvim --headless -u NONE -l scripts/check-neovim.lua
cargo build -p composer-language-server
nvim --headless -u NONE -l scripts/check-neovim-lsp.lua
```

Then run `cargo build -p composer-language-server`, install the repository as a Zed dev extension, and set `lsp.composer-language-server.binary.path` to the absolute path of `target/debug/composer-language-server`. Verify links and inlay hints in a real Composer project. Restart the Composer language server after rebuilding; reinstalling the extension alone may leave the existing server process running.

For Neovim, point `server_path` at `target/debug/composer-language-server`, open a real `composer.json`, and verify inlay hints and `gx`. The committed Lua check requires Neovim 0.10 or newer.

## Releasing

1. Add the release notes to `CHANGELOG.md`.
2. Set the same version in `extension.toml`, `Cargo.toml`, `server/Cargo.toml`, `src/lib.rs`, and `lua/composer_support/init.lua`.
3. Run the full local check. The version-consistency test verifies all version fields.
4. Commit and push the changes.
5. Tag the commit as `vX.Y.Z` and push the tag.

The release workflow creates a draft GitHub release, uploads native language-server binaries for all supported platforms, and publishes it after every build succeeds. Confirm the six assets are downloadable before submitting or updating the extension in `zed-industries/extensions`.
