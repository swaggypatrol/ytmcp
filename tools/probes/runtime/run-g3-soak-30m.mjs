import { execFileSync, spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

if (process.env.YTMCP_ALLOW_LIVE !== "1") {
  console.error("Refusing to run G3 hidden soak without YTMCP_ALLOW_LIVE=1");
  process.exit(1);
}

const repoRoot = resolve(import.meta.dirname, "../../..");
const electronBin = resolve(repoRoot, "apps/desktop/node_modules/.bin/electron");
const esbuildBin = resolve(repoRoot, "apps/desktop/node_modules/esbuild/bin/esbuild");
const entryScript = resolve(import.meta.dirname, "live-g3-soak-entry.mjs");
const sourceDaemonBin = process.env.YTMCP_DAEMON_BIN || resolve(repoRoot, "target/release/ytm-daemon");
const soakSeconds = Number.parseInt(process.env.YTMCP_SOAK_SECONDS ?? "1800", 10);
const reportPath =
  process.env.YTMCP_SOAK_REPORT_PATH || resolve(repoRoot, "artifacts/m0/g3-30m-soak-result.json");
const samplesPath =
  process.env.YTMCP_SOAK_SAMPLES_PATH ||
  resolve(repoRoot, "artifacts/m0/g3-30m-soak-samples.jsonl");

if (!existsSync(electronBin)) {
  console.error(`Electron binary missing at ${electronBin}`);
  process.exit(1);
}
if (!existsSync(sourceDaemonBin)) {
  console.error(`Daemon binary missing at ${sourceDaemonBin}`);
  process.exit(1);
}

let commitSha = "unknown";
try {
  commitSha = execFileSync("git", ["rev-parse", "--short", "HEAD"], {
    cwd: repoRoot,
    encoding: "utf8",
  }).trim();
} catch {
  // Ignore
}

const tempRoot = mkdtempSync(join(tmpdir(), "ytmcp-g3-soak-"));
const mainBundle = join(tempRoot, "main.mjs");
const rustHostBundle = join(tempRoot, "rust-host.mjs");
const immutableDaemonBin = join(tempRoot, "ytm-daemon");

try {
  // Snapshot ytm-daemon binary so concurrent cargo rebuilds cannot ETXTBSY or perturb the soak.
  copyFileSync(sourceDaemonBin, immutableDaemonBin);
  chmodSync(immutableDaemonBin, 0o755);

  const banner =
    "--banner:js=import { createRequire as __ytmCreateRequire } from 'node:module'; const require = __ytmCreateRequire(import.meta.url);";
  for (const [src, out] of [
    [resolve(repoRoot, "apps/desktop/src/bootstrap/main.ts"), mainBundle],
    [resolve(repoRoot, "apps/desktop/src/rust-host/index.ts"), rustHostBundle],
  ]) {
    const res = spawnSync(
      esbuildBin,
      [src, "--bundle", "--platform=node", "--format=esm", "--external:electron", banner, `--outfile=${out}`],
      { cwd: resolve(repoRoot, "apps/desktop"), encoding: "utf8", timeout: 20_000 }
    );
    if (res.status !== 0) {
      console.error(`Failed to bundle ${src}:`, res.stderr);
      process.exit(res.status ?? 1);
    }
  }

  const childEnv = { ...process.env };
  delete childEnv.YTMCP_SHOW_PLAYER;
  const timeoutMs = Math.max(120_000, (soakSeconds + 180) * 1000);
  const res = spawnSync(electronBin, [entryScript], {
    cwd: repoRoot,
    env: {
      ...childEnv,
      YTMCP_ALLOW_LIVE: "1",
      YTMCP_NO_AUTOSTART: "1",
      YTMCP_SOAK_SECONDS: String(soakSeconds),
      YTMCP_DAEMON_BIN: immutableDaemonBin,
      YTMCP_M0_MAIN_MODULE: mainBundle,
      YTMCP_M0_ENTRY_MODULE: rustHostBundle,
      YTMCP_SOAK_REPORT_PATH: reportPath,
      YTMCP_SOAK_SAMPLES_PATH: samplesPath,
      YTMCP_COMMIT_SHA: commitSha,
    },
    encoding: "utf8",
    timeout: timeoutMs,
    stdio: "pipe",
  });

  if (res.stdout) process.stdout.write(res.stdout);
  if (res.stderr) process.stderr.write(res.stderr);

  if (existsSync(reportPath)) {
    console.log("=== G3 SOAK SUMMARY ===");
    console.log(readFileSync(reportPath, "utf8"));
  }

  process.exit(res.status ?? 1);
} finally {
  rmSync(tempRoot, { recursive: true, force: true });
}
