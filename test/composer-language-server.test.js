"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawn } = require("node:child_process");
const { once } = require("node:events");
const { pathToFileURL } = require("node:url");
const test = require("node:test");

const server = require("../server/composer-language-server");

const composerJson = `{
  "name": "example/project",
  "require": {
    "php": "^8.3",
    "ext-json": "*",
    "laravel/framework": "^12.0",
    "psr/log": "^3.0"
  },
  "require-dev": {
    "phpunit/phpunit": "^12.0"
  },
  "replace": {
    "legacy/package": "self.version"
  }
}`;

test("finds package entries in Composer dependency sections", () => {
  const entries = server.dependencyEntries(composerJson);

  assert.deepEqual(
    entries.map(({ section, name }) => ({ section, name })),
    [
      { section: "require", name: "laravel/framework" },
      { section: "require", name: "psr/log" },
      { section: "require-dev", name: "phpunit/phpunit" },
      { section: "replace", name: "legacy/package" },
    ],
  );
});

test("creates Packagist links over package names only", () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "composer-lsp-links-"));
  const composerPath = path.join(directory, "composer.json");
  const uri = pathToFileURL(composerPath).href;
  server.documents.set(uri, { text: composerJson, version: 1 });

  const links = server.documentLinks(uri);

  assert.equal(links.length, 4);
  assert.equal(links[0].target, "https://packagist.org/packages/laravel/framework");
  assert.deepEqual(links[0].range, {
    start: { line: 5, character: 5 },
    end: { line: 5, character: 22 },
  });
  assert.match(links[0].tooltip, /laravel\/framework/);

  server.documents.delete(uri);
  fs.rmSync(directory, { recursive: true, force: true });
});

test("shows installed versions immediately and refreshes from cached Packagist metadata", async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "composer-lsp-installed-"));
  const composerPath = path.join(directory, "composer.json");
  const installedDirectory = path.join(directory, "vendor", "composer");
  const installedPath = path.join(installedDirectory, "installed.json");
  const uri = pathToFileURL(composerPath).href;

  fs.mkdirSync(installedDirectory, { recursive: true });
  fs.writeFileSync(
    installedPath,
    JSON.stringify({
      packages: [{ name: "laravel/framework", version: "v12.4.1" }],
      dev: true,
    }),
  );
  fs.writeFileSync(composerPath, composerJson);
  server.documents.set(uri, { text: composerJson, version: 1 });

  const versions = server.installedVersions(uri);
  assert.equal(versions.get("laravel/framework"), "v12.4.1");

  server.clearLatestVersionCache();
  const fetchMetadata = async () => ({
    ok: true,
    json: async () => ({
      packages: {
        "laravel/framework": [
          { version: "v13.0.0-beta.1", version_normalized: "13.0.0.0-beta1" },
          { version: "v12.5.0", version_normalized: "12.5.0.0" },
          { version: "v12.4.1", version_normalized: "12.4.1.0" },
        ],
      },
    }),
  });

  const initialHints = server.inlayHints(uri, undefined, fetchMetadata);
  assert.deepEqual(initialHints.map(({ label }) => label), ["v12.4.1"]);

  await server.latestStableVersion("laravel/framework", fetchMetadata);
  const hints = server.inlayHints(uri, undefined, fetchMetadata);
  assert.deepEqual(hints.map(({ label }) => label), ["v12.4.1 → v12.5.0"]);
  assert.match(hints[0].tooltip, /newest stable release/);
  assert.deepEqual(hints[0].position, { line: 5, character: 32 });

  server.clearLatestVersionCache();
  const offlineFetch = async () => {
    throw new Error("offline");
  };
  server.inlayHints(uri, undefined, offlineFetch);
  await server.latestStableVersion("laravel/framework", offlineFetch);
  assert.deepEqual(
    server.inlayHints(uri, undefined, offlineFetch).map(({ label }) => label),
    ["v12.4.1"],
  );

  server.documents.delete(uri);
  fs.rmSync(directory, { recursive: true, force: true });
});

test("supports Composer 1 metadata and a custom vendor directory", async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "composer-lsp-custom-vendor-"));
  const composerPath = path.join(directory, "composer.json");
  const installedDirectory = path.join(directory, "dependencies", "composer");
  const uri = pathToFileURL(composerPath).href;
  const text = `{
    "config": { "vendor-dir": "dependencies" },
    "require": { "psr/log": "^3.0" }
  }`;

  fs.mkdirSync(installedDirectory, { recursive: true });
  fs.writeFileSync(
    path.join(installedDirectory, "installed.json"),
    JSON.stringify([{ name: "psr/log", version: "3.0.2" }]),
  );
  server.documents.set(uri, { text, version: 1 });

  assert.equal(server.installedVersions(uri).get("psr/log"), "3.0.2");
  server.clearLatestVersionCache();
  const hints = server.inlayHints(uri, undefined, async () => ({
    ok: true,
    json: async () => ({
      packages: {
        "psr/log": [{ version: "3.0.2", version_normalized: "3.0.2.0" }],
      },
    }),
  }));
  assert.deepEqual(hints.map(({ label }) => label), ["v3.0.2"]);

  server.documents.delete(uri);
  fs.rmSync(directory, { recursive: true, force: true });
});

test("ignores JSON files that are not composer.json", async () => {
  const uri = pathToFileURL(path.join(os.tmpdir(), "package.json")).href;
  server.documents.set(uri, { text: composerJson, version: 1 });

  assert.deepEqual(server.documentLinks(uri), []);
  assert.deepEqual(server.inlayHints(uri), []);

  server.documents.delete(uri);
});

test("uses UTF-16 LSP character offsets", () => {
  assert.deepEqual(server.offsetToPosition("😀x", 2), { line: 0, character: 2 });
  assert.deepEqual(server.offsetToPosition("😀\nx", 4), { line: 1, character: 1 });
});

test("formats installed versions as compact version labels", () => {
  assert.equal(server.versionLabel("3.2.1"), "v3.2.1");
  assert.equal(server.versionLabel("v3.2.1"), "v3.2.1");
  assert.equal(server.versionLabel("dev-main"), "dev-main");
});

test("selects the newest stable Packagist release", async () => {
  server.clearLatestVersionCache();
  let requests = 0;
  const latest = await server.latestStableVersion("vendor/package", async (url, options) => {
    requests += 1;
    assert.equal(url, "https://repo.packagist.org/p2/vendor/package.json");
    assert.match(options.headers["User-Agent"], /zed-composer-support/);
    return {
      ok: true,
      json: async () => ({
        packages: {
          "vendor/package": [
            { version: "v3.0.0-RC1", version_normalized: "3.0.0.0-RC1" },
            { version: "v2.4.0", version_normalized: "2.4.0.0" },
            { version: "v2.3.1", version_normalized: "2.3.1.0" },
          ],
        },
      }),
    };
  });

  assert.equal(latest, "v2.4.0");
  assert.equal(server.compareVersions(latest, "v2.3.1"), 1);

  await server.latestStableVersion("vendor/package", async () => {
    requests += 1;
    throw new Error("cached result should be used");
  });
  assert.equal(requests, 1);
});

test("deduplicates concurrent metadata requests", async () => {
  server.clearLatestVersionCache();
  let requests = 0;
  let finishRequest;
  const fetchMetadata = () => {
    requests += 1;
    return new Promise((resolve) => {
      finishRequest = () =>
        resolve({
          ok: true,
          json: async () => ({
            packages: {
              "vendor/package": [{ version: "v1.2.3", version_normalized: "1.2.3.0" }],
            },
          }),
        });
    });
  };

  const first = server.latestStableVersion("vendor/package", fetchMetadata);
  const second = server.latestStableVersion("VENDOR/PACKAGE", fetchMetadata);
  assert.equal(requests, 1);
  finishRequest();
  assert.deepEqual(await Promise.all([first, second]), ["v1.2.3", "v1.2.3"]);
});

test("can disable Packagist update checks", () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "composer-lsp-no-updates-"));
  const uri = pathToFileURL(path.join(directory, "composer.json")).href;
  const installedDirectory = path.join(directory, "vendor", "composer");
  fs.mkdirSync(installedDirectory, { recursive: true });
  fs.writeFileSync(
    path.join(installedDirectory, "installed.json"),
    JSON.stringify({ packages: [{ name: "laravel/framework", version: "v12.4.1" }] }),
  );
  server.documents.set(uri, { text: composerJson, version: 1 });
  server.clearLatestVersionCache();
  server.configure({ check_updates: false });

  let requests = 0;
  const hints = server.inlayHints(uri, undefined, async () => {
    requests += 1;
    throw new Error("update checks are disabled");
  });

  assert.deepEqual(hints.map(({ label }) => label), ["v12.4.1"]);
  assert.equal(requests, 0);

  server.configure({ check_updates: true });
  server.documents.delete(uri);
  fs.rmSync(directory, { recursive: true, force: true });
});

test("fails quietly when installed metadata is missing or malformed", () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "composer-lsp-malformed-"));
  const uri = pathToFileURL(path.join(directory, "composer.json")).href;
  const installedDirectory = path.join(directory, "vendor", "composer");
  fs.mkdirSync(installedDirectory, { recursive: true });
  fs.writeFileSync(path.join(installedDirectory, "installed.json"), "not json");
  server.documents.set(uri, { text: composerJson, version: 1 });

  assert.deepEqual([...server.installedVersions(uri)], []);
  assert.deepEqual(server.inlayHints(uri), []);

  server.documents.delete(uri);
  fs.rmSync(directory, { recursive: true, force: true });
});

test("compares stable Composer versions without treating prereleases as updates", () => {
  assert.equal(server.compareVersions("v2.0.0", "1.9.9"), 1);
  assert.equal(server.compareVersions("1.2.3", "v1.2.3"), 0);
  assert.equal(server.compareVersions("v2.0.0-RC1", "1.9.9"), null);
});

test("serves document links over the LSP stdio transport", async () => {
  const child = spawn(process.execPath, [
    path.join(__dirname, "..", "server", "composer-language-server.js"),
  ]);
  const pending = new Map();
  let output = Buffer.alloc(0);

  child.stdout.on("data", (chunk) => {
    output = Buffer.concat([output, chunk]);
    while (true) {
      const headerEnd = output.indexOf("\r\n\r\n");
      if (headerEnd === -1) return;
      const match = /Content-Length:\s*(\d+)/i.exec(output.subarray(0, headerEnd).toString("ascii"));
      if (!match) throw new Error("language server response omitted Content-Length");
      const messageEnd = headerEnd + 4 + Number(match[1]);
      if (output.length < messageEnd) return;
      const message = JSON.parse(output.subarray(headerEnd + 4, messageEnd).toString("utf8"));
      output = output.subarray(messageEnd);
      pending.get(message.id)?.(message);
      pending.delete(message.id);
    }
  });

  function send(message) {
    const json = JSON.stringify({ jsonrpc: "2.0", ...message });
    child.stdin.write(`Content-Length: ${Buffer.byteLength(json)}\r\n\r\n${json}`);
  }

  function request(id, method, params = {}) {
    const response = new Promise((resolve) => pending.set(id, resolve));
    send({ id, method, params });
    return response;
  }

  const initialized = await request(1, "initialize");
  assert.equal(initialized.result.capabilities.documentLinkProvider.resolveProvider, false);
  assert.equal(initialized.result.capabilities.inlayHintProvider, true);
  assert.equal(initialized.result.serverInfo.version, "0.1.0");

  const uri = pathToFileURL(path.join(os.tmpdir(), "composer.json")).href;
  send({
    method: "textDocument/didOpen",
    params: { textDocument: { uri, languageId: "json", version: 1, text: composerJson } },
  });

  const links = await request(2, "textDocument/documentLink", { textDocument: { uri } });
  assert.equal(links.result.length, 4);
  assert.equal(links.result[1].target, "https://packagist.org/packages/psr/log");

  await request(3, "shutdown");
  send({ method: "exit" });
  const [exitCode] = await once(child, "exit");
  assert.equal(exitCode, 0);
  assert.equal(child.stderr.read(), null);
});
