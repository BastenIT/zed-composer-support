<p align="center">
  <img src="assets/composer-support-logo.png" width="180" alt="Composer Support logo">
</p>

<h1 align="center">Composer Support for Zed</h1>

Composer Support makes `composer.json` a little easier to work with in Zed. Package names link to Packagist, and installed versions appear beside their constraints without running Composer.

## What it does

- Command-click a dependency to open its Packagist page.
- Show the version from `vendor/composer/installed.json` as an inlay hint.
- Highlight available stable updates as `installed → latest`.
- Support Composer 1 and Composer 2 metadata, including a custom `config.vendor-dir`.
- Keep Zed's built-in JSON formatting and validation unchanged.

Links are added in `require`, `require-dev`, `conflict`, `replace`, `provide`, and `suggest`. Platform requirements such as `php`, `ext-*`, and `lib-*` are left alone because they are not Packagist packages.

## Installation

Once the extension is published, install **Composer Support** from Zed's Extensions page.

For local development, open the command palette, run **zed: extensions**, choose **Install Dev Extension**, and select this repository. Rebuild the dev extension after changing the Rust launcher. Changes to the JavaScript server take effect after the language server restarts.

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

The extension consists of a small Rust launcher and a dependency-free Node.js language server. Zed provides the Node runtime in normal use.

Requirements:

- Node.js 18 or newer
- Rust stable with the `wasm32-wasip2` target

Run the checks locally:

```sh
npm run check
cargo fmt --check
cargo check --target wasm32-wasip2
```

Published builds download the matching language-server file from the extension's GitHub release. This follows Zed's extension packaging rules and keeps the WebAssembly launcher small. Before publishing version `X.Y.Z`, create the `vX.Y.Z` tag; the release workflow verifies every version field and uploads `composer-language-server.js`.

When an upgrade cannot download its matching server, the launcher temporarily falls back to a valid server left by an earlier extension version. It retries the versioned download on the next language-server start. A fresh installation still requires the matching GitHub release asset.

If a local dev build cannot use the server from the checkout, point Zed at Node and the script explicitly:

```json
{
  "lsp": {
    "composer-language-server": {
      "binary": {
        "path": "/absolute/path/to/node",
        "arguments": [
          "/absolute/path/to/zed-composer-support/server/composer-language-server.js"
        ]
      }
    }
  }
}
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the release checklist.

## License

[MIT](LICENSE)
