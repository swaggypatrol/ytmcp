// Live T09 bridge probe against music.youtube.com with the app's real (already logged-in)
// profile. Audio goes to a temporary PipeWire/Pulse null sink, never to the speakers.
//
//   YTMCP_ALLOW_LIVE=1 node tools/probes/bridge/run-live-bridge-probe.mjs [--entry queue-future|queue-tune]
//
// `--entry queue-future` runs the read-only look at the page's queue future instead
// (live-queue-future-entry.mjs); `--entry queue-tune` clicks Up Next filters during playback
// (live-queue-tune-entry.mjs) and changes only the session's play queue.
//
// It bundles the committed desktop shell, so it exercises the production path: the bridge in
// isolated world 1001 and only the page shim in the main world (see docs/architecture.md).
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

if (process.env.YTMCP_ALLOW_LIVE !== "1") {
  console.error("Refusing to run the live bridge probe without YTMCP_ALLOW_LIVE=1");
  process.exit(2);
}

const repoRoot = resolve(import.meta.dirname, "../../..");
const desktop = resolve(repoRoot, "apps/desktop");
const electronBin = resolve(desktop, "node_modules/.bin/electron");
const daemonBin = process.env.YTMCP_DAEMON_BIN || resolve(repoRoot, "target/release/ytm-daemon");
const bridgeDist = resolve(repoRoot, "bridge/dist");
const reportDir = resolve(repoRoot, "artifacts/live/bridge");
const entryArg = process.argv.indexOf("--entry");
const entryName = entryArg >= 0 ? process.argv[entryArg + 1] : "t09";
const ENTRIES = {
  t09: ["live-bridge-entry.mjs", "t09-live-isolated-world.json"],
  "queue-future": ["live-queue-future-entry.mjs", "queue-future.json"],
  "queue-tune": ["live-queue-tune-entry.mjs", "queue-tune.json"],
};
if (!ENTRIES[entryName]) {
  console.error(`Unknown --entry ${entryName}; expected one of ${Object.keys(ENTRIES).join(", ")}`);
  process.exit(2);
}
const [entryFile, reportFile_] = ENTRIES[entryName];
const reportPath = resolve(reportDir, reportFile_);

for (const [what, path] of [
  ["electron", electronBin],
  ["daemon (cargo build --release -p ytm-daemon)", daemonBin],
  ["bridge bundle (pnpm --filter @ytmcp/bridge run build)", resolve(bridgeDist, "bridge-probe.mjs")],
  ["ui bundle (pnpm --filter @ytmcp/ui run build)", resolve(repoRoot, "ui/dist/index.html")],
]) {
  if (!existsSync(path)) {
    console.error(`Missing ${what}: ${path}`);
    process.exit(2);
  }
}

// Chromium child processes of a running app carry the profile path in --user-data-dir.
// Other probes use throwaway profiles under /tmp and do not conflict.
const running = spawnSync("pgrep", ["-f", "--", "--user-data-dir=[^ ]*ytmcp/electron-profile"], { encoding: "utf8" });
if (running.stdout.trim() !== "") {
  console.error("Another ytmcp/Electron instance appears to be running; refusing to share the profile.");
  process.exit(2);
}

const esbuild = await import(pathToFileURL(resolve(desktop, "node_modules/esbuild/lib/main.js")).href);

const tempRoot = mkdtempSync(join(tmpdir(), "ytmcp-live-bridge-"));
// main.mjs resolves its preloads from its own directory when that directory is named dist.
const distDir = join(tempRoot, "dist");
let nullModule = null;
try {
  const banner = { js: "import { createRequire as __r } from 'node:module'; const require = __r(import.meta.url);" };
  const common = { bundle: true, external: ["electron"], logLevel: "warning", absWorkingDir: desktop };
  await esbuild.build({ ...common, entryPoints: [resolve(desktop, "src/bootstrap/main.ts")], platform: "node", format: "esm", banner, outfile: join(distDir, "main.mjs") });
  await esbuild.build({ ...common, entryPoints: [resolve(desktop, "src/rust-host/index.ts")], platform: "node", format: "esm", banner, outfile: join(distDir, "rust-host.mjs") });
  await esbuild.build({ ...common, entryPoints: [resolve(desktop, "preload/ui-preload.ts")], platform: "browser", format: "cjs", outfile: join(distDir, "ui-preload.cjs") });
  await esbuild.build({ ...common, entryPoints: [resolve(desktop, "preload/player-preload.ts")], platform: "browser", format: "cjs", outfile: join(distDir, "player-preload.cjs") });

  const load = spawnSync("pactl", ["load-module", "module-null-sink", "sink_name=ytmcp_null", "sink_properties=device.description=YTMGlassNull"], { encoding: "utf8" });
  if (load.status !== 0) {
    console.error("Could not create the null sink; refusing to play audio to real speakers.", load.stderr);
    process.exit(2);
  }
  nullModule = load.stdout.trim();

  const res = spawnSync(electronBin, [resolve(import.meta.dirname, entryFile)], {
    cwd: repoRoot,
    env: {
      ...process.env,
      PULSE_SINK: "ytmcp_null",
      YTMCP_ALLOW_LIVE: "1",
      YTMCP_NO_AUTOSTART: "1",
      YTMCP_DAEMON_BIN: daemonBin,
      YTMCP_BRIDGE_DIST_DIR: bridgeDist,
      YTMCP_LIVE_MAIN_MODULE: join(distDir, "main.mjs"),
      YTMCP_LIVE_HOST_MODULE: join(distDir, "rust-host.mjs"),
      YTMCP_LIVE_MODE: "isolated-world",
      YTMCP_LIVE_TUNE_VIA: process.env.YTMCP_LIVE_TUNE_VIA ?? "",
      YTMCP_LIVE_REPORT: join(tempRoot, "report.json"),
    },
    encoding: "utf8",
    timeout: 480_000,
    maxBuffer: 64 * 1024 * 1024,
  });
  const reportFile = join(tempRoot, "report.json");
  if (existsSync(reportFile)) {
    mkdirSync(reportDir, { recursive: true });
    writeFileSync(reportPath, readFileSync(reportFile, "utf8"));
    console.log(readFileSync(reportFile, "utf8"));
    console.log(`report: ${reportPath}`);
  } else {
    console.error("No report produced. Electron stderr tail:\n", (res.stderr ?? "").split("\n").slice(-30).join("\n"));
  }
  process.exitCode = res.status ?? 1;
} finally {
  if (nullModule) spawnSync("pactl", ["unload-module", nullModule]);
  rmSync(tempRoot, { recursive: true, force: true });
}
