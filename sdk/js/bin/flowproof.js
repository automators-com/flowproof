#!/usr/bin/env node
// Thin launcher: the real `flowproof` CLI is a platform-native Rust binary
// shipped in a per-platform optional dependency; this resolves the right
// one and passes everything through untouched.
"use strict";

const { spawnSync } = require("node:child_process");
const path = require("node:path");
const fs = require("node:fs");

// Scoped names: the unscoped ones tripped npm's spam heuristic for new
// packages and could not be published at all. Keep these in step with
// package.json's optionalDependencies - test/launcher.test.js fails if they
// drift, because a mismatch here is an "unsupported platform" error for a
// platform that is actually supported.
const PLATFORM_PACKAGES = {
  "linux-x64": "@automators/flowproof-cli-linux-x64",
  "darwin-x64": "@automators/flowproof-cli-darwin-x64",
  "darwin-arm64": "@automators/flowproof-cli-darwin-arm64",
  "win32-x64": "@automators/flowproof-cli-win32-x64",
};

function binaryPath() {
  // An explicit binary wins over the resolved platform package. This is
  // what makes a tight adopter loop possible: a team hitting a gap can
  // point at a build from a branch or a CI artifact and keep testing,
  // instead of waiting for a release for every fix. It is a development
  // handle, so it is deliberately loud - a silently swapped engine would
  // make a green run mean nothing.
  const override = process.env.FLOWPROOF_BIN;
  if (override) {
    if (!fs.existsSync(override)) {
      console.error(
        `flowproof: FLOWPROOF_BIN points at ${override}, which does not exist.`
      );
      process.exit(2);
    }
    console.error(`flowproof: using FLOWPROOF_BIN=${override}`);
    return override;
  }

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
