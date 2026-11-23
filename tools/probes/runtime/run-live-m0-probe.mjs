import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

if (process.env.YTMCP_ALLOW_LIVE !== "1") {
  console.error("Refusing to run live M0 probe without YTMCP_ALLOW_LIVE=1");
  process.exit(1);
}

const repoRoot = resolve(import.meta.dirname, "../../..");
const electronBin = resolve(repoRoot, "apps/desktop/node_modules/.bin/electron");
const esbuildBin = resolve(repoRoot, "apps/desktop/node_modules/esbuild/bin/esbuild");
const entryScript = resolve(import.meta.dirname, "live-m0-electron-entry.mjs");
const daemonBin = process.env.YTMCP_DAEMON_BIN || resolve(repoRoot, "target/release/ytm-daemon");
const reportPath = resolve(repoRoot, "artifacts/m0/live-g2-g4-g3-g5-probe.json");
const mprisReportPath = resolve(repoRoot, "artifacts/m0/g5-live-mpris-check.json");

if (!existsSync(electronBin)) {
  console.error(`Electron binary missing at ${electronBin}`);
  process.exit(1);
}
if (!existsSync(daemonBin)) {
  console.error(`Daemon binary missing at ${daemonBin}`);
  process.exit(1);
}

const tempRoot = mkdtempSync(join(tmpdir(), "ytmcp-live-m0-"));
const mainBundle = join(tempRoot, "main.mjs");
const rustHostBundle = join(tempRoot, "rust-host.mjs");

try {
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

  // Note: Do NOT pass --user-data-dir so it uses the persisted $XDG_DATA_HOME/ytmcp/electron-profile.
  // Do NOT set YTMCP_SHOW_PLAYER=1 so player window stays HIDDEN.
  const childEnv = { ...process.env };
  delete childEnv.YTMCP_SHOW_PLAYER;
  const res = spawnSync(electronBin, [entryScript], {
    cwd: repoRoot,
    env: {
      ...childEnv,
      YTMCP_ALLOW_LIVE: "1",
      YTMCP_NO_AUTOSTART: "1",
      YTMCP_DAEMON_BIN: daemonBin,
      YTMCP_M0_MAIN_MODULE: mainBundle,
      YTMCP_M0_ENTRY_MODULE: rustHostBundle,
      YTMCP_LIVE_REPORT_PATH: reportPath,
      YTMCP_LIVE_MPRIS_PATH: mprisReportPath,
    },
    encoding: "utf8",
    timeout: 180_000,
    stdio: "pipe",
  });

  if (res.stdout) process.stdout.write(res.stdout);
  if (res.stderr) process.stderr.write(res.stderr);

  if (existsSync(reportPath)) {
    console.log("=== LIVE M0 REPORT ===");
    console.log(readFileSync(reportPath, "utf8"));
  }

  process.exit(res.status ?? 1);
} finally {
  rmSync(tempRoot, { recursive: true, force: true });
}
