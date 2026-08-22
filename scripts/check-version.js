"use strict";

const fs = require("node:fs");
const path = require("node:path");

const expected = process.argv[2];
if (!/^\d+\.\d+\.\d+$/.test(expected || "")) {
  process.stderr.write("usage: node scripts/check-version.js X.Y.Z\n");
  process.exitCode = 2;
  return;
}

const root = path.resolve(__dirname, "..");
const checks = [
  ["extension.toml", /^version = "([^"]+)"/m],
  ["Cargo.toml", /^version = "([^"]+)"/m],
  ["package.json", /"version":\s*"([^"]+)"/],
  ["src/lib.rs", /const SERVER_VERSION: &str = "([^"]+)"/],
  ["src/lib.rs", /releases\/download\/v([^/]+)\/composer-language-server\.js/],
  ["server/composer-language-server.js", /const SERVER_VERSION = "([^"]+)"/],
];

let failed = false;
for (const [file, pattern] of checks) {
  const contents = fs.readFileSync(path.join(root, file), "utf8");
  const actual = pattern.exec(contents)?.[1];
  if (actual !== expected) {
    failed = true;
    process.stderr.write(`${file}: expected ${expected}, found ${actual || "no version"}\n`);
  }
}

if (failed) process.exitCode = 1;
else process.stdout.write(`all version fields match ${expected}\n`);
