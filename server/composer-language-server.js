"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { fileURLToPath } = require("node:url");

const DEPENDENCY_SECTIONS = new Set([
  "require",
  "require-dev",
  "conflict",
  "replace",
  "provide",
  "suggest",
]);
const UPDATE_SECTIONS = new Set(["require", "require-dev"]);
const PACKAGIST_CACHE_TTL_MS = 60 * 60 * 1000;
const PACKAGIST_ERROR_CACHE_TTL_MS = 5 * 60 * 1000;
const PACKAGIST_TIMEOUT_MS = 5000;
const MAX_CONCURRENT_REQUESTS = 10;
const MAX_CACHE_ENTRIES = 512;
const MAX_HEADER_BYTES = 8 * 1024;
const MAX_MESSAGE_BYTES = 16 * 1024 * 1024;
const MAX_INSTALLED_METADATA_BYTES = 64 * 1024 * 1024;
const SERVER_VERSION = "0.3.0";

const documents = new Map();
const latestVersionCache = new Map();
const fetchQueue = [];
let input = Buffer.alloc(0);
let shutdownRequested = false;
let activeFetches = 0;
let checkUpdates = true;
let inlayRefreshSupported = false;
let refreshTimer = null;
let nextServerRequestId = 1;

function tokenize(text) {
  const tokens = [];
  let offset = 0;

  while (offset < text.length) {
    const character = text[offset];

    if (/\s/.test(character)) {
      offset += 1;
      continue;
    }

    if ('{}[]:,'.includes(character)) {
      tokens.push({ type: character, start: offset, end: offset + 1 });
      offset += 1;
      continue;
    }

    if (character === '"') {
      const start = offset;
      offset += 1;
      let escaped = false;

      while (offset < text.length) {
        const current = text[offset];
        offset += 1;

        if (escaped) {
          escaped = false;
        } else if (current === "\\") {
          escaped = true;
        } else if (current === '"') {
          break;
        }
      }

      const raw = text.slice(start, offset);
      let value;
      try {
        value = JSON.parse(raw);
      } catch {
        value = raw.slice(1, raw.endsWith('"') ? -1 : undefined);
      }

      tokens.push({ type: "string", start, end: offset, value });
      continue;
    }

    const start = offset;
    while (offset < text.length && !/[\s{}\[\],:]/.test(text[offset])) {
      offset += 1;
    }
    tokens.push({ type: "literal", start, end: offset, value: text.slice(start, offset) });
  }

  return tokens;
}

function parseTree(text) {
  const tokens = tokenize(text);

  function parseValue(index) {
    const token = tokens[index];
    if (!token) return [null, index];

    if (token.type === "{") {
      const node = { type: "object", start: token.start, end: token.end, properties: [] };
      let cursor = index + 1;

      while (cursor < tokens.length && tokens[cursor].type !== "}") {
        if (tokens[cursor].type === ",") {
          cursor += 1;
          continue;
        }

        const key = tokens[cursor];
        if (key.type !== "string") {
          cursor += 1;
          continue;
        }

        cursor += 1;
        if (tokens[cursor]?.type === ":") cursor += 1;
        const [value, next] = parseValue(cursor);
        if (value) {
          node.properties.push({ key: key.value, keyToken: key, value });
          cursor = next;
        } else {
          cursor += 1;
        }

        if (tokens[cursor]?.type === ",") cursor += 1;
      }

      if (tokens[cursor]?.type === "}") {
        node.end = tokens[cursor].end;
        cursor += 1;
      }
      return [node, cursor];
    }

    if (token.type === "[") {
      const node = { type: "array", start: token.start, end: token.end, items: [] };
      let cursor = index + 1;

      while (cursor < tokens.length && tokens[cursor].type !== "]") {
        if (tokens[cursor].type === ",") {
          cursor += 1;
          continue;
        }
        const [value, next] = parseValue(cursor);
        if (!value || next === cursor) {
          cursor += 1;
          continue;
        }
        node.items.push(value);
        cursor = next;
      }

      if (tokens[cursor]?.type === "]") {
        node.end = tokens[cursor].end;
        cursor += 1;
      }
      return [node, cursor];
    }

    return [
      { type: token.type, start: token.start, end: token.end, value: token.value },
      index + 1,
    ];
  }

  return parseValue(0)[0];
}

function dependencyEntries(text) {
  let root;
  try {
    root = parseTree(text);
  } catch {
    return [];
  }
  if (!root || root.type !== "object") return [];

  const entries = [];
  for (const section of root.properties) {
    if (!DEPENDENCY_SECTIONS.has(section.key) || section.value.type !== "object") continue;

    for (const dependency of section.value.properties) {
      if (!isComposerPackage(dependency.key)) continue;
      entries.push({
        section: section.key,
        name: dependency.key,
        keyToken: dependency.keyToken,
        value: dependency.value,
      });
    }
  }
  return entries;
}

function isComposerPackage(name) {
  return typeof name === "string" && /^[a-z0-9_.-]+\/[a-z0-9_.-]+$/i.test(name);
}

function offsetToPosition(text, offset) {
  let line = 0;
  let lineStart = 0;

  for (let index = 0; index < offset; index += 1) {
    if (text.charCodeAt(index) === 10) {
      line += 1;
      lineStart = index + 1;
    }
  }

  return { line, character: offset - lineStart };
}

function rangeForToken(text, token, excludeQuotes = false) {
  const inset = excludeQuotes && token.type === "string" ? 1 : 0;
  const endInset = excludeQuotes && token.type === "string" && token.end > token.start + 1 ? 1 : 0;
  return {
    start: offsetToPosition(text, token.start + inset),
    end: offsetToPosition(text, token.end - endInset),
  };
}

function comparePositions(left, right) {
  return left.line === right.line ? left.character - right.character : left.line - right.line;
}

function positionInRange(position, range) {
  return comparePositions(position, range.start) >= 0 && comparePositions(position, range.end) <= 0;
}

function isComposerJson(uri) {
  try {
    return path.basename(fileURLToPath(uri)).toLowerCase() === "composer.json";
  } catch {
    return false;
  }
}

function installedVersions(uri) {
  let projectDirectory;
  try {
    projectDirectory = path.dirname(fileURLToPath(uri));
  } catch {
    return new Map();
  }

  let vendorDirectory = "vendor";
  const document = documents.get(uri);
  if (document) {
    try {
      const composer = JSON.parse(document.text);
      const configuredVendorDirectory = composer?.config?.["vendor-dir"];
      if (typeof configuredVendorDirectory === "string" && configuredVendorDirectory.trim()) {
        vendorDirectory = configuredVendorDirectory;
      }
    } catch {
      // Keep the conventional vendor directory while composer.json is being edited.
    }
  }

  const installedPath = path.resolve(projectDirectory, vendorDirectory, "composer", "installed.json");

  try {
    if (fs.statSync(installedPath).size > MAX_INSTALLED_METADATA_BYTES) return new Map();
    const installed = JSON.parse(fs.readFileSync(installedPath, "utf8"));
    const packages = Array.isArray(installed) ? installed : installed.packages;
    const versions = new Map();
    if (!Array.isArray(packages)) return versions;

    for (const dependency of packages) {
      if (typeof dependency?.name !== "string" || typeof dependency?.version !== "string") continue;
      const version =
        typeof dependency.pretty_version === "string"
          ? dependency.pretty_version
          : dependency.version;
      versions.set(dependency.name.toLowerCase(), version);
    }
    return versions;
  } catch {
    return new Map();
  }
}

function versionLabel(version) {
  const label = version.trim();
  return /^\d/.test(label) ? `v${label}` : label;
}

function stableVersionParts(version) {
  const label = version.trim();
  if (/(?:^|[.\-_])(dev|alpha|beta|rc)\d*/i.test(label)) return null;

  const match = /^v?(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:\.(\d+))?/i.exec(label);
  if (!match) return null;

  const patchMatch = /(?:-|\.)p(?:atch)?(\d+)$/i.exec(label);
  return [
    Number(match[1]),
    Number(match[2] || 0),
    Number(match[3] || 0),
    Number(match[4] || 0),
    Number(patchMatch?.[1] || 0),
  ];
}

function compareVersions(left, right) {
  const leftParts = stableVersionParts(left);
  const rightParts = stableVersionParts(right);
  if (!leftParts || !rightParts) return null;

  for (let index = 0; index < leftParts.length; index += 1) {
    if (leftParts[index] !== rightParts[index]) return leftParts[index] - rightParts[index];
  }
  return 0;
}

function runWithFetchLimit(task) {
  return new Promise((resolve, reject) => {
    const run = async () => {
      activeFetches += 1;
      try {
        resolve(await task());
      } catch (error) {
        reject(error);
      } finally {
        activeFetches -= 1;
        fetchQueue.shift()?.();
      }
    };

    if (activeFetches < MAX_CONCURRENT_REQUESTS) run();
    else fetchQueue.push(run);
  });
}

async function requestPackageMetadata(packageName, fetchImplementation) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), PACKAGIST_TIMEOUT_MS);
  timeout.unref?.();

  try {
    const response = await fetchImplementation(
      `https://repo.packagist.org/p2/${encodeURI(packageName)}.json`,
      {
        headers: {
          Accept: "application/json",
          "User-Agent": `zed-composer-support/${SERVER_VERSION} (+https://github.com/bastenit/zed-composer-support)`,
        },
        signal: controller.signal,
      },
    );
    if (!response.ok) return null;
    return response.json();
  } finally {
    clearTimeout(timeout);
  }
}

function newestStableVersion(metadata, packageName) {
  const packages = metadata?.packages;
  if (!packages || typeof packages !== "object") return null;

  const versions = packages[packageName] || Object.values(packages)[0];
  if (!Array.isArray(versions)) return null;

  let newest = null;
  for (const release of versions) {
    if (typeof release?.version !== "string") continue;
    const comparableVersion = release.version_normalized || release.version;
    if (!stableVersionParts(comparableVersion)) continue;

    if (!newest || compareVersions(comparableVersion, newest.comparableVersion) > 0) {
      newest = { version: release.version, comparableVersion };
    }
  }

  return newest?.version || null;
}

function clearLatestVersionCache() {
  latestVersionCache.clear();
}

function pruneLatestVersionCache(now = Date.now()) {
  for (const [key, entry] of latestVersionCache) {
    if (entry.expiresAt <= now && entry.settled) latestVersionCache.delete(key);
  }

  while (latestVersionCache.size >= MAX_CACHE_ENTRIES) {
    const oldestSettled = [...latestVersionCache].find(([, entry]) => entry.settled);
    if (!oldestSettled) break;
    latestVersionCache.delete(oldestSettled[0]);
  }
}

function cachedLatestStableVersion(packageName) {
  const cached = latestVersionCache.get(packageName.toLowerCase());
  if (!cached || !cached.settled || cached.expiresAt <= Date.now()) return undefined;
  return cached.value;
}

function scheduleInlayRefresh() {
  if (!inlayRefreshSupported || refreshTimer) return;

  refreshTimer = setTimeout(() => {
    refreshTimer = null;
    send({
      jsonrpc: "2.0",
      id: `composer-refresh-${nextServerRequestId++}`,
      method: "workspace/inlayHint/refresh",
    });
  }, 100);
  refreshTimer.unref?.();
}

function latestStableVersion(packageName, fetchImplementation = globalThis.fetch) {
  if (typeof fetchImplementation !== "function") return Promise.resolve(null);

  const cacheKey = packageName.toLowerCase();
  const now = Date.now();
  const cached = latestVersionCache.get(cacheKey);
  if (cached && cached.expiresAt > now) return cached.promise;

  pruneLatestVersionCache(now);
  if (latestVersionCache.size >= MAX_CACHE_ENTRIES) return Promise.resolve(null);

  const cacheEntry = {
    expiresAt: now + PACKAGIST_CACHE_TTL_MS,
    promise: null,
    settled: false,
    value: null,
  };
  cacheEntry.promise = runWithFetchLimit(async () => {
    try {
      const metadata = await requestPackageMetadata(cacheKey, fetchImplementation);
      const latest = newestStableVersion(metadata, cacheKey);
      if (!latest) cacheEntry.expiresAt = Date.now() + PACKAGIST_ERROR_CACHE_TTL_MS;
      cacheEntry.value = latest;
      return latest;
    } catch {
      cacheEntry.expiresAt = Date.now() + PACKAGIST_ERROR_CACHE_TTL_MS;
      cacheEntry.value = null;
      return null;
    } finally {
      cacheEntry.settled = true;
      if (cacheEntry.value) scheduleInlayRefresh();
    }
  });
  latestVersionCache.set(cacheKey, cacheEntry);
  return cacheEntry.promise;
}

function documentLinks(uri) {
  const document = documents.get(uri);
  if (!document || !isComposerJson(uri)) return [];

  return dependencyEntries(document.text).map((dependency) => ({
    range: rangeForToken(document.text, dependency.keyToken, true),
    target: `https://packagist.org/packages/${encodeURI(dependency.name)}`,
    tooltip: `Open ${dependency.name} on Packagist`,
  }));
}

function inlayHints(uri, requestedRange, fetchImplementation = globalThis.fetch) {
  const document = documents.get(uri);
  if (!document || !isComposerJson(uri)) return [];

  const versions = installedVersions(uri);
  const candidates = [];

  for (const dependency of dependencyEntries(document.text)) {
    const version = versions.get(dependency.name.toLowerCase());
    if (!version) continue;

    const position = offsetToPosition(document.text, dependency.value.end);
    if (requestedRange && !positionInRange(position, requestedRange)) continue;

    candidates.push({ dependency, position, version });
  }

  return candidates.map(({ dependency, position, version }) => {
    let latestVersion = null;
    if (checkUpdates && UPDATE_SECTIONS.has(dependency.section)) {
      latestVersion = cachedLatestStableVersion(dependency.name);
      if (latestVersion === undefined) {
        void latestStableVersion(dependency.name, fetchImplementation);
        latestVersion = null;
      }
    }
    const updateAvailable = latestVersion && compareVersions(latestVersion, version) > 0;
    const installedLabel = versionLabel(version);

    return {
      position,
      label: updateAvailable
        ? `${installedLabel} → ${versionLabel(latestVersion)}`
        : installedLabel,
      paddingLeft: true,
      tooltip: updateAvailable
        ? `${dependency.name}: ${installedLabel} is installed; ${versionLabel(latestVersion)} is the newest stable release on Packagist`
        : `Version currently installed for ${dependency.name}`,
    };
  });
}

function configure(options = {}) {
  checkUpdates = options?.check_updates !== false;
}

function send(message) {
  const json = JSON.stringify(message);
  process.stdout.write(`Content-Length: ${Buffer.byteLength(json, "utf8")}\r\n\r\n${json}`);
}

function respond(id, result) {
  send({ jsonrpc: "2.0", id, result });
}

async function handleMessage(message) {
  const { id, method, params } = message;

  // Responses to server-initiated requests do not need a response of their own.
  if (method === undefined) return;

  if (method === "initialize") {
    configure(params?.initializationOptions);
    inlayRefreshSupported = params?.capabilities?.workspace?.inlayHint?.refreshSupport === true;
    respond(id, {
      capabilities: {
        textDocumentSync: 1,
        documentLinkProvider: { resolveProvider: false },
        inlayHintProvider: true,
      },
      serverInfo: { name: "composer-language-server", version: SERVER_VERSION },
    });
    return;
  }

  if (method === "shutdown") {
    shutdownRequested = true;
    respond(id, null);
    return;
  }

  if (method === "exit") {
    process.exit(shutdownRequested ? 0 : 1);
  }

  if (method === "textDocument/didOpen") {
    const textDocument = params?.textDocument;
    if (typeof textDocument?.uri === "string" && typeof textDocument.text === "string") {
      documents.set(textDocument.uri, {
        text: textDocument.text,
        version: textDocument.version,
      });
    }
    return;
  }

  if (method === "textDocument/didChange") {
    const textDocument = params?.textDocument;
    const change = params?.contentChanges?.at(-1);
    if (typeof textDocument?.uri === "string" && change && typeof change.text === "string") {
      documents.set(textDocument.uri, {
        text: change.text,
        version: textDocument.version,
      });
    }
    return;
  }

  if (method === "textDocument/didClose") {
    if (typeof params?.textDocument?.uri === "string") {
      documents.delete(params.textDocument.uri);
    }
    return;
  }

  if (method === "textDocument/documentLink") {
    const uri = params?.textDocument?.uri;
    respond(id, typeof uri === "string" ? documentLinks(uri) : []);
    return;
  }

  if (method === "textDocument/inlayHint") {
    const uri = params?.textDocument?.uri;
    respond(id, typeof uri === "string" ? inlayHints(uri, params?.range) : []);
    return;
  }

  if (id !== undefined) respond(id, null);
}

function consumeInput(chunk) {
  input = Buffer.concat([input, chunk]);

  while (true) {
    const headerEnd = input.indexOf("\r\n\r\n");
    if (headerEnd === -1) {
      if (input.length > MAX_HEADER_BYTES) {
        process.stderr.write("composer-language-server: discarded an oversized LSP header\n");
        input = Buffer.alloc(0);
      }
      return;
    }

    if (headerEnd > MAX_HEADER_BYTES) {
      process.stderr.write("composer-language-server: discarded an oversized LSP header\n");
      input = input.subarray(headerEnd + 4);
      continue;
    }

    const headers = input.subarray(0, headerEnd).toString("ascii");
    const match = /(?:^|\r\n)Content-Length:\s*(\d+)/i.exec(headers);
    if (!match) {
      input = input.subarray(headerEnd + 4);
      continue;
    }

    const contentLength = Number(match[1]);
    if (!Number.isSafeInteger(contentLength) || contentLength > MAX_MESSAGE_BYTES) {
      process.stderr.write("composer-language-server: discarded an oversized LSP message\n");
      input = Buffer.alloc(0);
      return;
    }
    const messageEnd = headerEnd + 4 + contentLength;
    if (input.length < messageEnd) return;

    const body = input.subarray(headerEnd + 4, messageEnd).toString("utf8");
    input = input.subarray(messageEnd);

    try {
      const message = JSON.parse(body);
      void handleMessage(message).catch((error) => {
        process.stderr.write(`composer-language-server: ${error.stack || error}\n`);
      });
    } catch (error) {
      process.stderr.write(`composer-language-server: ${error.stack || error}\n`);
    }
  }
}

if (require.main === module) {
  process.stdin.on("data", consumeInput);
  process.stdin.resume();
}

module.exports = {
  cachedLatestStableVersion,
  clearLatestVersionCache,
  compareVersions,
  configure,
  dependencyEntries,
  documentLinks,
  documents,
  inlayHints,
  installedVersions,
  isComposerPackage,
  latestStableVersion,
  newestStableVersion,
  offsetToPosition,
  parseTree,
  tokenize,
  versionLabel,
};
