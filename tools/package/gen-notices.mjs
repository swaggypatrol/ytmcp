#!/usr/bin/env node
// Prints THIRD_PARTY_NOTICES.md for everything shipped in the package:
// Rust crates linked into the release binaries and npm production dependencies bundled
// into the UI/desktop/bridge dist files. License files are embedded verbatim.
// Usage: node tools/package/gen-notices.mjs > THIRD_PARTY_NOTICES.md
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

const repo = resolve(import.meta.dirname, "../..");
const LICENSE_FILE = /^(licen[cs]e|copying|notice)([-_.].*)?$/i;
const RUST_BINARY_PACKAGES = ["ytm-daemon", "ytm-glass-mcp"];
// These locked crates omit the license file from the registry tarball.
// The checked-in copies come from the crate's published VCS revision.
const upstreamLicenseFiles = new Map([
  ["alloc-stdlib@0.3.0", "alloc-stdlib-0.3.0.txt"],
  ["json-crawler@0.1.1", "youtui-c489567.txt"],
  ["ytmapi-rs@0.3.4", "youtui-c489567.txt"],
]);

function licenseTexts(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((f) => LICENSE_FILE.test(f))
    .sort()
    .map((f) => ({ file: f, text: readFileSync(join(dir, f), "utf8").trim() }));
}

function crateLicenseTexts(pkg) {
  const vendored = upstreamLicenseFiles.get(`${pkg.name}@${pkg.version}`);
  return vendored
    ? [{ file: vendored, text: readFileSync(join(repo, "packaging/licenses", vendored), "utf8").trim() }]
    : licenseTexts(dirname(pkg.manifest_path));
}

function rustCrates() {
  const meta = JSON.parse(
    execFileSync(
      "cargo",
      ["metadata", "--format-version", "1", "--locked", "--offline", "--filter-platform", "x86_64-unknown-linux-gnu"],
      { cwd: repo, maxBuffer: 256 * 1024 * 1024, encoding: "utf8" },
    ),
  );
  const byId = new Map(meta.packages.map((p) => [p.id, p]));
  const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]));
  const seen = new Set();
  const stack = RUST_BINARY_PACKAGES.map((name) => {
    const pkg = meta.packages.find((p) => p.name === name && p.source === null);
    if (!pkg) throw new Error(`release binary package missing from cargo metadata: ${name}`);
    return pkg.id;
  });
  while (stack.length) {
    const id = stack.pop();
    if (seen.has(id)) continue;
    seen.add(id);
    for (const dep of nodes.get(id)?.deps ?? []) {
      // Only normal (linked) dependencies ship in the binary.
      if (dep.dep_kinds.some((k) => k.kind === null)) stack.push(dep.pkg);
    }
  }
  return [...seen]
    .map((id) => byId.get(id))
    .filter((p) => p.source) // workspace crates are covered by the app license
    .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version))
    .map((p) => ({
      name: p.name,
      version: p.version,
      license: p.license ?? p.license_file ?? "UNKNOWN",
      url: p.repository ?? p.homepage ?? "",
      texts: crateLicenseTexts(p),
    }));
}

function npmPackages() {
  const raw = JSON.parse(
    execFileSync("pnpm", ["licenses", "list", "--prod", "--json"], {
      cwd: repo,
      maxBuffer: 64 * 1024 * 1024,
      encoding: "utf8",
    }),
  );
  const out = [];
  for (const [license, pkgs] of Object.entries(raw)) {
    for (const p of pkgs) {
      if (p.name.startsWith("@ytm-glass/")) continue;
      out.push({
        name: p.name,
        version: p.versions.join(", "),
        license,
        url: p.homepage ?? "",
        texts: licenseTexts(p.paths?.[0] ?? ""),
      });
    }
  }
  // Tailwind is a dev dependency, but its generated stylesheet ships.
  const tailwindDir = join(repo, "ui/node_modules/tailwindcss");
  const tailwind = JSON.parse(readFileSync(join(tailwindDir, "package.json"), "utf8"));
  out.push({
    name: tailwind.name,
    version: tailwind.version,
    license: tailwind.license,
    url: tailwind.homepage ?? "",
    texts: licenseTexts(tailwindDir),
  });
  return out.sort((a, b) => a.name.localeCompare(b.name));
}

function section(title, items) {
  const lines = [`## ${title}`, ""];
  for (const it of items) {
    lines.push(`### ${it.name} ${it.version}`, "", `License: ${it.license}${it.url ? `  \nSource: ${it.url}` : ""}`, "");
    if (it.texts.length === 0) {
      lines.push("The upstream package declares the license above and supplies no standalone license file.", "");
    }
    for (const t of it.texts) lines.push(`<details><summary>${t.file}</summary>`, "", "```text", t.text, "```", "", "</details>", "");
  }
  return lines.join("\n");
}

const header = [
  "# YTM Glass third-party notices",
  "",
  "YTM Glass is MIT licensed. The Electron/Chromium runtime notices are installed next to this file under electron/.",
  "",
].join("\n");

process.stdout.write(
  [
    header,
    section("npm dependency graph for the UI, desktop shell and bridge (includes compiler transitive dependencies)", npmPackages()),
    section(`Rust crates linked into ${RUST_BINARY_PACKAGES.join(" and ")}`, rustCrates()),
  ].join("\n"),
);
