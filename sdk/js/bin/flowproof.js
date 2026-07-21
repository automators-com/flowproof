#!/usr/bin/env node
// Thin launcher: the real `flowproof` CLI is a platform-native Rust binary
// shipped in a per-platform optional dependency; this resolves the right
// one and passes everything through untouched.
"use strict";

const { spawnSync } = require("node:child_process");
const path = require("node:path");

const PLATFORM_PACKAGES = {
  "linux-x64": "flowproof-cli-linux-x64",
  "darwin-x64": "flowproof-cli-darwin-x64",
  "darwin-arm64": "flowproof-cli-darwin-arm64",
  "win32-x64": "flowproof-cli-win32-x64",
};

function binaryPath() {
  const key = `${process.platform}-${process.arch}`;
  const pkg = PLATFORM_PACKAGES[key];
  if (!pkg) {
    console.error(
      `flowproof: no prebuilt binary for ${key}.\n` +
        `Supported: ${Object.keys(PLATFORM_PACKAGES).join(", ")}.\n` +
        `On other platforms, install via pip instead: pip install flowproof`
    );
    process.exit(2);
  }
  const file = process.platform === "win32" ? "flowproof.exe" : "flowproof";
  try {
    return path.join(path.dirname(require.resolve(`${pkg}/package.json`)), file);
  } catch {
    console.error(
      `flowproof: platform package ${pkg} is not installed.\n` +
        `Your package manager may have skipped optional dependencies — ` +
        `reinstall without --no-optional (or use: pip install flowproof).`
    );
    process.exit(2);
  }
}

const result = spawnSync(binaryPath(), process.argv.slice(2), {
  stdio: "inherit",
});
if (result.error) {
  console.error(`flowproof: failed to start the native binary: ${result.error.message}`);
  process.exit(2);
}
process.exit(result.status === null ? 2 : result.status);
