import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const repoRoot = resolve(import.meta.dirname, "../../..");
const electronBin = resolve(repoRoot, "apps/desktop/node_modules/.bin/electron");
const esbuildBin = resolve(repoRoot, "apps/desktop/node_modules/esbuild/bin/esbuild");
const entryScript = resolve(import.meta.dirname, "g1-electron-entry.mjs");
const reportPath = resolve(repoRoot, "artifacts/m0/g1-probe-result.json");

if (!existsSync(electronBin)) {
  console.error(`Electron binary missing at ${electronBin}`);
  process.exit(1);
}

const tempUserDataDir = mkdtempSync(join(tmpdir(), "ytmcp-g1-probe-"));
const probeMainBundle = join(tempUserDataDir, "main-probe.mjs");

try {
  // Bundle main.ts with workspace @ytmcp/protocol inlined so Electron ESM can import it directly.
  const bundleRes = spawnSync(
    esbuildBin,
    [
      resolve(repoRoot, "apps/desktop/src/bootstrap/main.ts"),
      "--bundle",
      "--platform=node",
      "--format=esm",
      "--external:electron",
      "--banner:js=import { createRequire as __ytmCreateRequire } from 'node:module'; const require = __ytmCreateRequire(import.meta.url);",
      `--outfile=${probeMainBundle}`,
    ],
    {
      cwd: resolve(repoRoot, "apps/desktop"),
      encoding: "utf8",
      timeout: 15_000,
    }
  );
  if (bundleRes.status !== 0) {
    console.error("Failed to bundle main.ts for G1 probe:", bundleRes.stderr);
    process.exit(bundleRes.status ?? 1);
  }

  const result = spawnSync(
    electronBin,
    [`--user-data-dir=${tempUserDataDir}`, entryScript],
    {
      cwd: repoRoot,
      env: {
        ...process.env,
        YTMCP_NO_AUTOSTART: "1",
        YTMCP_MODE: "mock",
        YTMCP_PROBE_HTML: "1",
        YTMCP_G1_MAIN_MODULE: probeMainBundle,
        YTMCP_G1_REPORT_PATH: reportPath,
        NODE_PATH: [
          resolve(repoRoot, "packages/protocol/node_modules"),
          resolve(repoRoot, "node_modules"),
        ].join(":"),
      },
      encoding: "utf8",
      timeout: 45_000,
    }
  );

  if (result.stdout) {
    process.stdout.write(result.stdout);
  }
  if (result.stderr) {
    process.stderr.write(result.stderr);
  }

  if (result.status !== 0) {
    console.error(`G1 probe Electron exited with code ${result.status}`);
    process.exit(result.status ?? 1);
  }

  if (!existsSync(reportPath)) {
    console.error(`G1 probe did not write ${reportPath}`);
    process.exit(1);
  }

  const parsed = JSON.parse(readFileSync(reportPath, "utf8"));
  const checks = {
    mainPreloadSandboxed: parsed.sandbox?.mainPreloadSandboxed === true,
    noSandboxFlagAbsent: parsed.sandbox?.noSandboxFlagPresent === false,
    pageWorldIsolated: parsed.sandbox?.mainPageWorldNoNode === true && parsed.sandbox?.playerPageWorldNoNode === true,
    bridgeHiddenOnNonMusicOrigin: parsed.sandbox?.playerBridgeHiddenOutsideMusicOrigin === true,
    sessionIsolated:
      parsed.sessionIsolation?.distinctSessionObjects === true &&
      parsed.sessionIsolation?.playerSawSyntheticCookie === true &&
      parsed.sessionIsolation?.uiMusicCookieCount === 0,
    ipcNegativeSecurityPassed:
      parsed.ipcNegativeSecurity?.rendererCrossRoleSecretRejected === true &&
      parsed.ipcNegativeSecurity?.rendererCrossRoleBridgeRejected === true &&
      parsed.ipcNegativeSecurity?.rendererBadVersionRejected === true &&
      parsed.ipcNegativeSecurity?.rendererValidPingAccepted === true &&
      parsed.ipcNegativeSecurity?.guardWrongOriginBridgeRejected === true &&
      parsed.ipcNegativeSecurity?.guardSubframeRejected === true,
    glassRenderedAndScrolled:
      parsed.glassPage?.heroImageLoaded === true &&
      String(parsed.glassPage?.backdropFilter ?? "").includes("blur(") &&
      (parsed.glassPage?.scrollFrameIntervals?.frameCount ?? 0) > 10,
  };

  console.log(JSON.stringify({ reportPath, checks, safeStorage: parsed.safeStorage, wayland: parsed.wayland }, null, 2));

  const failed = Object.entries(checks).filter(([, ok]) => !ok).map(([k]) => k);
  if (failed.length > 0) {
    console.error(`G1 probe failed assertions: ${failed.join(", ")}`);
    process.exit(1);
  }
} finally {
  rmSync(tempUserDataDir, { recursive: true, force: true });
}
