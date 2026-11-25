#!/usr/bin/env node
// Opens the wheel log window in the repo's pinned Electron with a throwaway profile.
// See wheel-log.mjs. Usage: node tools/probes/input/run-wheel-log.mjs [seconds]
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const here = import.meta.dirname;
const electron = resolve(here, "../../../apps/desktop/node_modules/.bin/electron");
const profile = mkdtempSync(join(tmpdir(), "ytmcp-wheel-"));
const out = resolve(here, "out/wheel.json");
const result = spawnSync(electron, ["--mute-audio", `--user-data-dir=${profile}`, join(here, "wheel-log.mjs")], {
  stdio: "inherit",
  env: { ...process.env, WHEEL_LOG_OUT: out, WHEEL_LOG_SECONDS: process.argv[2] ?? "20" },
});
rmSync(profile, { recursive: true, force: true });
console.log(result.status === 0 ? `wrote ${out}` : "the wheel log did not finish");
process.exit(result.status ?? 1);
