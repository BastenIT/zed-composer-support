<p align="center">
  <img src="assets/composer-support-logo.png" width="180" alt="Composer Support logo">
</p>

<h1 align="center">Composer Support for Zed</h1>

Composer Support adds package navigation and version information to `composer.json` files in Zed. It uses a native Rust language server and does not require Composer or a JavaScript runtime to be running in the background.

## Features

- Command-click a dependency to open its Packagist page.
- Show the installed version next to its constraint.
- Show when a newer stable release is available.
- Read Composer 1 and Composer 2 installation metadata.
- Respect a custom `config.vendor-dir`.
- Leave Zed's built-in JSON formatting and validation untouched.

Package links work in `require`, `require-dev`, `conflict`, `replace`, `provide`, and `suggest`. Platform requirements such as `php`, `ext-*`, and `lib-*` are ignored because they are not Packagist packages.

## Installation

Install **Composer Support** from Zed's Extensions page.

Package links work without additional configuration. Use Command-click on macOS or Control-click on Linux and Windows.

### Version hints

Zed disables inlay hints by default. Enable them in your settings:

```json
{
  "inlay_hints": {
    "enabled": true,
    "show_background": false
  }
}
```

An installed package is shown as `v3.2.1`. If an update is available, the hint becomes `v3.2.1 → v3.3.0`.

Versions are read from `vendor/composer/installed.json`. If the file is missing or cannot be read, the hint is simply omitted.

### Update checks

Update checks are enabled by default and run in the background. Installed versions are displayed immediately; a slow connection, Packagist error, or offline session does not prevent local version hints from appearing.

To disable Packagist requests:

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

The server checks the newest stable release published on Packagist. It does not resolve Composer constraints or query private repositories, so an update may require a constraint change before Composer can install it.

## How it works

The Zed extension is a small WebAssembly launcher written in Rust. It downloads the matching native `composer-language-server` binary for the current operating system and CPU architecture.

The language server reuses parsed documents and unchanged Composer metadata. Network requests are cached, deduplicated, limited to four concurrent requests and capped at 256 requests per hour. Successful lookups are cached for six hours and failed lookups for fifteen minutes.

Release binaries are provided for Intel and ARM systems on macOS, Linux, and Windows. Linux binaries are statically linked with musl and do not depend on the host distribution's glibc version.

## Development

Requirements:

- Rust stable
- The `wasm32-wasip2` Rust target

Build the language server before installing the repository as a Zed dev extension:

```sh
cargo build -p composer-language-server
```

In Zed, open the Extensions page, choose **Install Dev Extension**, and select this repository. Restart the Composer language server after rebuilding it.

Run the project checks with:

```sh
cargo fmt --all --check
cargo test -p composer-language-server
cargo clippy -p composer-language-server --all-targets -- -D warnings
cargo check -p zed_composer_support --target wasm32-wasip2
```

If Zed cannot find a local server build, set its path explicitly:

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

## Development disclosure

AI tools helped with implementation and review. The project is directed and maintained by a human at BastenIT, who chooses the features, tests the extension, reviews changes, and approves releases.

## BastenIT

<p>
  <img src="assets/bastenit-logo.png" width="96" alt="BastenIT logo">
</p>

Built and maintained by BastenIT.

## License

[MIT](LICENSE)
