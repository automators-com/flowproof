// The launcher is what `npx flowproof` runs, so these are the failures that
// would reach a user first: a platform map that disagrees with the packages
// actually declared, or a version that has drifted from the Rust crate.
// Run with: node --test sdk/js/test
"use strict";

const test = require("node:test");
const assert = require("node:assert");
const fs = require("node:fs");
const path = require("node:path");

const here = path.join(__dirname, "..");
const pkg = require(path.join(here, "package.json"));
const launcher = fs.readFileSync(path.join(here, "bin", "flowproof.js"), "utf8");

/// The names the launcher will try to resolve at runtime, read out of the
/// source so the test cannot drift from it by construction.
function launcherPackages() {
  const block = launcher.match(/const PLATFORM_PACKAGES = \{([\s\S]*?)\};/);
  assert.ok(block, "bin/flowproof.js must declare PLATFORM_PACKAGES");
  const names = {};
  for (const line of block[1].split("\n")) {
    const m = line.match(/"([^"]+)":\s*"([^"]+)"/);
    if (m) names[m[1]] = m[2];
  }
  return names;
}

test("every platform the launcher supports is an actual dependency", () => {
  const declared = Object.keys(pkg.optionalDependencies || {}).sort();
  const resolved = Object.values(launcherPackages()).sort();
  // A name in the launcher but not in optionalDependencies is never
  // installed, so a supported platform reports "not installed" and the CLI
  // is unusable there. The reverse is dead weight in every install.
  assert.deepStrictEqual(resolved, declared);
});

test("the platform packages are scoped", () => {
  // Unscoped names could not be published: npm answered 403 "Package name
  // triggered spam detection" for the new unscoped ones, which is what left
  // npm stuck on a placeholder while PyPI shipped. Keep them scoped.
  for (const name of Object.values(launcherPackages())) {
    assert.ok(
      name.startsWith("@automators/"),
      `${name} must be scoped, or publishing will be blocked`
    );
  }
});

test("every platform package is pinned to this package's own version", () => {
  for (const [name, range] of Object.entries(pkg.optionalDependencies || {})) {
    // An exact pin, not a range: the launcher and the binary it starts are
    // one artifact, and a floating range would let them separate.
    assert.strictEqual(range, pkg.version, `${name} must be pinned to ${pkg.version}`);
  }
});

test("the npm version matches the Rust crate version", () => {
  const cargo = fs.readFileSync(path.join(here, "..", "..", "Cargo.toml"), "utf8");
  const version = cargo.match(/^version = "([^"]+)"/m);
  assert.ok(version, "Cargo.toml must declare a version");
  // npm and PyPI both publish this same binary, so a skew here is how the
  // registries drift apart.
  assert.strictEqual(pkg.version, version[1]);
});

test("the launcher declares a bin and ships the files it needs", () => {
  assert.strictEqual(pkg.bin.flowproof, "bin/flowproof.js");
  for (const needed of ["bin/flowproof.js", "index.js"]) {
    assert.ok(pkg.files.includes(needed), `${needed} must be published`);
  }
});

test("FLOWPROOF_BIN is honoured, announced, and validated", () => {
  // The handle that makes an adopter loop possible: point at a build from a
  // branch or a CI artifact and keep testing, instead of waiting for a
  // release per fix.
  assert.match(launcher, /process\.env\.FLOWPROOF_BIN/);
  // It must be LOUD. A silently swapped engine would make a green run mean
  // nothing, which is the opposite of what this tool is for.
  assert.match(launcher, /using FLOWPROOF_BIN/);
  // And it must not fail obscurely when the path is wrong.
  assert.match(launcher, /does not exist/);
  // The notice goes to stderr, so `--json` keeps its stdout contract.
  const noticeLine = launcher
    .split("\n")
    .find((l) => l.includes("using FLOWPROOF_BIN"));
  assert.ok(
    noticeLine.includes("console.error"),
    "the override notice must go to stderr, not stdout"
  );
});
