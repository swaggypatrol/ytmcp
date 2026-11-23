import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const repoRoot = resolve(import.meta.dirname, "../../../..");
const electronBin = resolve(repoRoot, "apps/desktop/node_modules/.bin/electron");
const esbuildBin = resolve(repoRoot, "apps/desktop/node_modules/esbuild/bin/esbuild");
const entryScript = resolve(import.meta.dirname, "m0-electron-entry.mjs");

if (!existsSync(electronBin)) {
  console.error(`Electron binary missing at ${electronBin}`);
  process.exit(1);
}

const tempRoot = mkdtempSync(join(tmpdir(), "ytmcp-m0-ui-"));
const mainBundle = join(tempRoot, "main.mjs");
const rustHostBundle = join(tempRoot, "rust-host.mjs");
const reportPath = join(tempRoot, "m0-ui-report.json");

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
      { cwd: resolve(repoRoot, "apps/desktop"), encoding: "utf8", timeout: 15_000 }
    );
    if (res.status !== 0) {
      console.error(`Failed to bundle ${src}:`, res.stderr);
      process.exit(res.status ?? 1);
    }
  }

  const probeExit = process.argv.includes("--probe-exit") ? "1" : "0";
  const res = spawnSync(
    electronBin,
    [`--user-data-dir=${join(tempRoot, "user-data")}`, entryScript],
    {
      cwd: repoRoot,
      env: {
        ...process.env,
        YTMCP_NO_AUTOSTART: "1",
        YTMCP_DATA_DIR: join(tempRoot, "xdg"),
        XDG_CONFIG_HOME: join(tempRoot, "xdg/config"),
        XDG_DATA_HOME: join(tempRoot, "xdg/data"),
        XDG_CACHE_HOME: join(tempRoot, "xdg/cache"),
        YTMCP_M0_MAIN_MODULE: mainBundle,
        YTMCP_M0_ENTRY_MODULE: rustHostBundle,
        YTMCP_M0_REPORT_PATH: reportPath,
        YTMCP_M0_PROBE_EXIT: probeExit,
      },
      encoding: "utf8",
      timeout: probeExit === "1" ? 30_000 : 0,
      stdio: probeExit === "1" ? "pipe" : "inherit",
    }
  );

  if (probeExit === "1") {
    if (res.stdout) process.stdout.write(res.stdout);
    if (res.stderr) process.stderr.write(res.stderr);
    if (res.status !== 0) {
      process.exit(res.status ?? 1);
    }
    if (existsSync(reportPath)) {
      console.log(readFileSync(reportPath, "utf8"));
    }
  } else if (res.status !== 0) {
    process.exit(res.status ?? 1);
  }
} finally {
  rmSync(tempRoot, { recursive: true, force: true });
}
