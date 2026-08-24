<p align="center">
  <img src="assets/composer-support-logo.png" width="180" alt="Composer Support logo">
</p>

<h1 align="center">Composer Support for Zed</h1>

Composer Support is an ultra-high-performance language server for `composer.json`, written entirely in Rust. Package names link to Packagist, and installed versions appear beside their constraints without running Composer or a JavaScript runtime.

## What it does

- Command-click a dependency to open its Packagist page.
- Show the version from `vendor/composer/installed.json` as an inlay hint.
- Highlight available stable updates as `installed → latest`.
- Support Composer 1 and Composer 2 metadata, including a custom `config.vendor-dir`.
- Keep Zed's built-in JSON formatting and validation unchanged.

Links are added in `require`, `require-dev`, `conflict`, `replace`, `provide`, and `suggest`. Platform requirements such as `php`, `ext-*`, and `lib-*` are left alone because they are not Packagist packages.

## Performance by design

The language server is distributed as an optimized native binary with link-time optimization enabled. Its event loop stays lightweight, filesystem and network work runs off the main protocol path, Packagist requests are deduplicated and limited to ten at a time, and the update cache is bounded. Locally installed versions are always returned first; update checks never delay or hide them.

## Installation

Once the extension is published, install **Composer Support** from Zed's Extensions page.

For local development, build the native server first with `cargo build -p composer-language-server`. Then open the command palette, run **zed: extensions**, choose **Install Dev Extension**, and select this repository. Restart the Composer language server after rebuilding it.

Document links are enabled in Zed by default. Use Command-click on macOS or Control-click on Linux and Windows.

## Showing versions

Zed disables inlay hints by default. Enable them in your settings:

```json
{
  "inlay_hints": {
    "enabled": true,
    "show_background": false
  }
}
```

The label is deliberately compact: `v3.2.1`, or `v3.2.1 → v3.3.0` when a newer stable release is available. Zed controls the presentation of inlay hints, so the extension cannot assign a custom pill shape or color to an individual label.

If `vendor/composer/installed.json` is absent, invalid, or does not contain a package, the extension simply omits that hint.

## Update checks

Update checks are enabled by default. They query Packagist's Composer 2 metadata endpoint for packages in `require` and `require-dev`. Results are cached for one hour, requests are limited to ten at a time, and each request times out after five seconds.

Installed versions are shown immediately. Packagist requests happen in the background; a slow connection, rate limit, invalid response, or offline session never hides the locally installed version.

To disable all Packagist requests:

```json
{
  "lsp": {
    "composer-language-server": {
      "initialization_options": {
        "check_updates": false
      }
    }
  }
}
```

The comparison uses the newest stable tag published on Packagist. It does not resolve Composer constraints and does not query private Composer repositories, so an update shown by the extension may require a constraint change.

## Development

The extension is entirely Rust. A small WebAssembly launcher integrates with Zed and downloads a native `composer-language-server` executable for the current platform. The server uses a range-aware JSON scanner so links and hints stay aligned with the source text, including UTF-16 LSP positions.

Requirements:

- Rust stable with the `wasm32-wasip2` target

Run the checks locally:

```sh
cargo fmt --all --check
cargo test -p composer-language-server
cargo clippy -p composer-language-server --all-targets -- -D warnings
cargo check -p zed_composer_support --target wasm32-wasip2
```

Published builds download the matching native language server from the extension's GitHub release. Releases include binaries for Intel and ARM systems on macOS, Linux, and Windows. Linux releases are statically linked with musl, so they do not depend on the host distribution's glibc version. The macOS builds declare deployment targets matching Zed's supported Intel and Apple Silicon systems, while the Windows builds use Rust's native MSVC targets.

Before publishing version `X.Y.Z`, set the matching project versions and push the `vX.Y.Z` tag. The release workflow tests the server and extension, builds all six executables on native GitHub runners, verifies that Linux binaries have no shared-library dependency, and publishes the release only after every build succeeds. Runner labels such as `ubuntu-22.04` and `windows-2025` identify GitHub's temporary build machines; they are not runtime requirements for extension users.

When an upgrade cannot download its matching server, the launcher temporarily falls back to a valid server left by an earlier extension version. It retries the versioned download on the next language-server start. A fresh installation still requires the matching GitHub release asset.

If a local dev build cannot find the server from `target/debug`, point Zed at the executable explicitly:

```json
{
  "lsp": {
    "composer-language-server": {
      "binary": {
        "path": "/absolute/path/to/zed-composer-support/target/debug/composer-language-server"
      }
    }
  }
}
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the release checklist.

## BastenIT

<p>
  <img src="assets/bastenit-logo.png" width="96" alt="BastenIT logo">
</p>

Built and maintained by BastenIT.

## License

[MIT](LICENSE)
