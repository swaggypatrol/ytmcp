import { existsSync, mkdirSync, readlinkSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import { app, screen } from "electron";

const mainModuleUrl = process.env.YTM_GLASS_G1_MAIN_MODULE
  ? pathToFileURL(process.env.YTM_GLASS_G1_MAIN_MODULE).href
  : new URL("../../../apps/desktop/dist/main.mjs", import.meta.url).href;

const { startDesktopShell, validateRendererIpc } = await import(mainModuleUrl);

function percentile(sorted, p) {
  if (sorted.length === 0) return 0;
  const idx = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return Number(sorted[idx].toFixed(3));
}

function summarizeIntervals(intervals, durationMs, displays) {
  const sorted = [...intervals].sort((a, b) => a - b);
  const sum = sorted.reduce((acc, v) => acc + v, 0);
  const meanMs = sorted.length > 0 ? sum / sorted.length : 0;
  const primaryDisplay = displays[0];
  const targetHz = primaryDisplay?.displayFrequency && primaryDisplay.displayFrequency > 0
    ? primaryDisplay.displayFrequency
    : 60;
  const budgetMs = 1000 / targetHz;
  const over2xBudgetCount = sorted.filter((dt) => dt > budgetMs * 2).length;
  const over33msCount = sorted.filter((dt) => dt > 33.33).length;

  return {
    frameCount: sorted.length,
    durationMs: Number(durationMs.toFixed(2)),
    targetRefreshHz: targetHz,
    frameBudgetMs: Number(budgetMs.toFixed(3)),
    minMs: sorted.length > 0 ? Number(sorted[0].toFixed(3)) : 0,
    p50Ms: percentile(sorted, 50),
    p95Ms: percentile(sorted, 95),
    p99Ms: percentile(sorted, 99),
    maxMs: sorted.length > 0 ? Number(sorted[sorted.length - 1].toFixed(3)) : 0,
    meanMs: Number(meanMs.toFixed(3)),
    estimatedObservedFps: meanMs > 0 ? Number((1000 / meanMs).toFixed(2)) : 0,
    over2xBudgetCount,
    over2xBudgetRatio: sorted.length > 0 ? Number((over2xBudgetCount / sorted.length).toFixed(4)) : 0,
    over33msCount,
  };
}

function checkWaylandEvidence() {
  const xdgRuntimeDir = process.env.XDG_RUNTIME_DIR ?? "";
  const waylandDisplay = process.env.WAYLAND_DISPLAY ?? "";
  const socketPath = xdgRuntimeDir && waylandDisplay ? join(xdgRuntimeDir, waylandDisplay) : null;
  const socketExists = socketPath ? existsSync(socketPath) : false;

  const waylandPeerInodes = new Set();
  const procSocketInodes = new Set();
  let matchedWaylandSocketInode = null;
  let waylandPeerObserved = false;

  try {
    const fds = readdirSync("/proc/self/fd");
    for (const fd of fds) {
      const target = readlinkSync(`/proc/self/fd/${fd}`);
      const m = /^socket:\[(\d+)\]$/.exec(target);
      if (m) {
        procSocketInodes.add(m[1]);
      }
    }
  } catch {
    // ignore
  }

  try {
    const ssOut = execFileSync("ss", ["-xp"], { encoding: "utf8", timeout: 2000 });
    const pidTag = `pid=${process.pid},`;
    for (const line of ssOut.split("\n")) {
      if (socketPath && line.includes(socketPath)) {
        // Format: u_str ESTAB 0 0 /run/user/<uid>/wayland-1 <localInode> * <peerInode> ...
        const tokens = line.trim().split(/\s+/);
        const starIdx = tokens.lastIndexOf("*");
        if (starIdx >= 0 && tokens[starIdx + 1]) {
          waylandPeerInodes.add(tokens[starIdx + 1]);
        }
      }
      if (line.includes(pidTag)) {
        const tokens = line.trim().split(/\s+/);
        const starIdx = tokens.lastIndexOf("*");
        if (starIdx >= 2 && tokens[starIdx - 1]) {
          procSocketInodes.add(tokens[starIdx - 1]);
        }
      }
    }
    for (const inode of procSocketInodes) {
      if (waylandPeerInodes.has(inode)) {
        waylandPeerObserved = true;
        matchedWaylandSocketInode = inode;
        break;
      }
    }
  } catch {
    // ss unavailable or restricted
  }

  return {
    xdgSessionType: process.env.XDG_SESSION_TYPE ?? null,
    waylandDisplay: waylandDisplay || null,
    xdgCurrentDesktop: process.env.XDG_CURRENT_DESKTOP ?? null,
    displayEnv: process.env.DISPLAY ?? null,
    waylandSocketPath: socketPath,
    waylandSocketPathExists: socketExists,
    waylandPeerObservedViaSs: waylandPeerObserved,
    matchedWaylandSocketInode,
  };
}

async function run() {
  const reportPath = resolve(
    process.env.YTM_GLASS_G1_REPORT_PATH ?? "artifacts/m0/g1-probe-result.json"
  );

  process.env.YTM_GLASS_MODE = "mock";
  process.env.YTM_GLASS_PROBE_HTML = "1";

  const controller = await startDesktopShell({});
  if (!controller) {
    throw new Error("Failed to acquire single-instance lock during G1 probe");
  }

  const { identity, windowManager, safeStorageStatus, shutdown } = controller;
  const mainWin = windowManager.getMainWindow();
  if (!mainWin) {
    throw new Error("Main window not created");
  }

  await new Promise((resolveLoad, rejectLoad) => {
    const timer = setTimeout(() => rejectLoad(new Error("Timed out loading glass-test.html")), 15000);
    mainWin.webContents.once("did-finish-load", () => {
      clearTimeout(timer);
      resolveLoad();
    });
    if (!mainWin.webContents.isLoadingMainFrame() && mainWin.webContents.getURL().startsWith("app://")) {
      clearTimeout(timer);
      resolveLoad();
    }
  });

  // Create isolated player window in mock mode (without opening music.youtube.com)
  const playerWin = windowManager.createPlayerWindow({ show: false, loadMusicUrl: false });
  await new Promise((resolveLoad) => {
    if (!playerWin.webContents.isLoadingMainFrame()) {
      resolveLoad();
      return;
    }
    playerWin.webContents.once("did-finish-load", () => resolveLoad());
  });

  // Check page-world isolation & preload sandbox status in mainWindow
  const mainRendererProbe = await mainWin.webContents.executeJavaScript(`
    (async () => {
      const heroImg = document.getElementById("probe-hero-img");
      const playerBar = document.querySelector(".player-bar");
      const backdropFilter = playerBar ? getComputedStyle(playerBar).backdropFilter : "";
      const sandboxStatus = window.ytmGlassUi ? window.ytmGlassUi.getSandboxStatus() : { sandboxed: false };
      const hostInfo = window.ytmGlassUi ? await window.ytmGlassUi.getHostInfo() : null;

      // Real IPC negative security probes from renderer
      const crossRoleSecret = window.ytmGlassUi
        ? await window.ytmGlassUi.sendEnvelope({
            protocolVersion: 1,
            kind: "host.sessionMaterial",
            seq: 1,
            payload: { accountEpoch: "a", credentialRevision: "c", cookiePairs: { SAPISID: "synthetic" } }
          })
        : null;

      const crossRoleBridge = window.ytmGlassUi
        ? await window.ytmGlassUi.sendEnvelope({
            protocolVersion: 1,
            kind: "bridge.ready",
            bridgeEpoch: "b1",
            seq: 2,
            payload: { bridgeEpoch: "b1", bridgeVersion: "0.1", loggedInObserved: false, capabilities: {} }
          })
        : null;

      const badVersion = window.ytmGlassUi
        ? await window.ytmGlassUi.sendEnvelope({
            protocolVersion: 99,
            kind: "ui.ping",
            seq: 3,
            payload: {}
          })
        : null;

      const validPing = window.ytmGlassUi
        ? await window.ytmGlassUi.sendEnvelope({
            protocolVersion: 1,
            kind: "ui.ping",
            seq: 4,
            payload: {}
          })
        : null;

      return {
        url: window.location.href,
        origin: window.location.origin,
        sandboxStatus,
        hostInfo,
        pageWorldNoProcess: typeof window.process === "undefined",
        pageWorldNoRequire: typeof window.require === "undefined",
        pageWorldNoIpcRenderer: typeof window.ipcRenderer === "undefined",
        heroImageLoaded: Boolean(heroImg && heroImg.complete && heroImg.naturalWidth > 0),
        trackRowCount: document.querySelectorAll(".track-row").length,
        backdropFilter,
        ipcProbes: {
          crossRoleSecret,
          crossRoleBridge,
          badVersion,
          validPing,
        },
      };
    })()
  `);

  // Measure 3s scroll rAF frame intervals on glass-test.html
  const scrollTiming = await mainWin.webContents.executeJavaScript(`
    new Promise((resolveScroll) => {
      const container = document.getElementById("scroll-container");
      const intervals = [];
      let start = null;
      let prev = null;
      const durationTargetMs = 3000;

      function step(ts) {
        if (start === null) {
          start = ts;
          prev = ts;
          requestAnimationFrame(step);
          return;
        }
        intervals.push(ts - prev);
        prev = ts;

        const elapsed = ts - start;
        if (container) {
          const maxScroll = Math.max(1, container.scrollHeight - container.clientHeight);
          const progress = Math.min(1, elapsed / durationTargetMs);
          container.scrollTop = Math.round(maxScroll * progress);
        }

        if (elapsed < durationTargetMs) {
          requestAnimationFrame(step);
        } else {
          resolveScroll({ intervals, elapsedMs: elapsed });
        }
      }

      requestAnimationFrame(step);
    })
  `);

  // Player window page-world isolation check (about:blank must NOT have ytmGlassBridge)
  const playerRendererProbe = await playerWin.webContents.executeJavaScript(`
    (() => ({
      url: window.location.href,
      origin: window.location.origin,
      pageWorldNoProcess: typeof window.process === "undefined",
      pageWorldNoRequire: typeof window.require === "undefined",
      bridgeExposedOnNonMusicOrigin: typeof window.ytmGlassBridge !== "undefined",
    }))()
  `);

  // Session isolation proof using synthetic temporary cookie in playerSession
  const uiSess = windowManager.getUiSession();
  const playerSess = windowManager.getPlayerSession();
  await playerSess.cookies.set({
    url: "https://music.youtube.com",
    name: "ytm_probe_isolation_check",
    value: "synthetic_probe_value",
    secure: true,
    httpOnly: true,
  });
  const playerMusicCookies = await playerSess.cookies.get({ url: "https://music.youtube.com" });
  const uiMusicCookies = await uiSess.cookies.get({ url: "https://music.youtube.com" });
  const uiAllCookies = await uiSess.cookies.get({});
  await playerSess.cookies.remove("https://music.youtube.com", "ytm_probe_isolation_check");

  // Additional host-guard negative checks (wrong origin / subframe / playerBridge cross-role)
  const wrongOriginGuard = validateRendererIpc(
    {
      role: "playerBridge",
      allowedOrigins: new Set(["https://music.youtube.com"]),
      senderMatchesExpected: true,
      senderFrame: { isMainFrame: true, origin: "https://accounts.google.com" },
      bridgeOriginActive: false,
    },
    {
      protocolVersion: 1,
      kind: "bridge.health",
      seq: 1,
      payload: {
        bridgeEpoch: "b1",
        controlsBound: true,
        healthy: true,
        videoElementPresent: true,
      },
    }
  );
  const subframeGuard = validateRendererIpc(
    {
      role: "mainUi",
      allowedOrigins: new Set(["app://ui"]),
      senderMatchesExpected: true,
      senderFrame: { isMainFrame: false, origin: "app://ui" },
    },
    { protocolVersion: 1, kind: "ui.ping", seq: 1, payload: {} }
  );

  const displays = screen.getAllDisplays().map((d) => ({
    id: d.id,
    bounds: d.bounds,
    scaleFactor: d.scaleFactor,
    displayFrequency: d.displayFrequency,
  }));
  const gpuFeatureStatus = app.getGPUFeatureStatus();
  const gpuInfoComplete = await app.getGPUInfo("complete");
  const waylandEvidence = checkWaylandEvidence();
  const frameIntervals = summarizeIntervals(scrollTiming.intervals, scrollTiming.elapsedMs, displays);

  const report = {
    probe: "G1",
    timestamp: new Date().toISOString(),
    identity: {
      appId: identity.appId,
      codename: identity.codename,
      uiSessionPartition: identity.uiSessionPartition,
      playerSessionPartition: identity.playerSessionPartition,
    },
    versions: {
      electron: process.versions.electron,
      chrome: process.versions.chrome,
      node: process.versions.node,
      v8: process.versions.v8,
    },
    sandbox: {
      noSandboxFlagPresent: process.argv.includes("--no-sandbox"),
      mainPreloadSandboxed: mainRendererProbe.sandboxStatus?.sandboxed ?? false,
      mainPageWorldNoNode: mainRendererProbe.pageWorldNoProcess && mainRendererProbe.pageWorldNoRequire,
      mainPageWorldNoIpcRenderer: mainRendererProbe.pageWorldNoIpcRenderer,
      playerPageWorldNoNode: playerRendererProbe.pageWorldNoProcess && playerRendererProbe.pageWorldNoRequire,
      playerBridgeHiddenOutsideMusicOrigin: !playerRendererProbe.bridgeExposedOnNonMusicOrigin,
    },
    wayland: waylandEvidence,
    displays,
    gpu: {
      featureStatus: gpuFeatureStatus,
      infoComplete: gpuInfoComplete,
    },
    glassPage: {
      url: mainRendererProbe.url,
      origin: mainRendererProbe.origin,
      heroImageLoaded: mainRendererProbe.heroImageLoaded,
      trackRowCount: mainRendererProbe.trackRowCount,
      backdropFilter: mainRendererProbe.backdropFilter,
      scrollFrameIntervals: frameIntervals,
    },
    sessionIsolation: {
      distinctSessionObjects: uiSess !== playerSess,
      uiSessionPersistent: uiSess.isPersistent(),
      playerSessionPersistent: playerSess.isPersistent(),
      playerSawSyntheticCookie: playerMusicCookies.some((c) => c.name === "ytm_probe_isolation_check"),
      uiMusicCookieCount: uiMusicCookies.length,
      uiTotalCookieCount: uiAllCookies.length,
    },
    ipcNegativeSecurity: {
      rendererCrossRoleSecretRejected: mainRendererProbe.ipcProbes.crossRoleSecret?.ok === false,
      rendererCrossRoleSecretCode: mainRendererProbe.ipcProbes.crossRoleSecret?.code ?? null,
      rendererCrossRoleBridgeRejected: mainRendererProbe.ipcProbes.crossRoleBridge?.ok === false,
      rendererCrossRoleBridgeCode: mainRendererProbe.ipcProbes.crossRoleBridge?.code ?? null,
      rendererBadVersionRejected: mainRendererProbe.ipcProbes.badVersion?.ok === false,
      rendererBadVersionCode: mainRendererProbe.ipcProbes.badVersion?.code ?? null,
      rendererValidPingAccepted: mainRendererProbe.ipcProbes.validPing?.ok === true,
      guardWrongOriginBridgeRejected: !wrongOriginGuard.ok,
      guardWrongOriginReason: !wrongOriginGuard.ok ? wrongOriginGuard.reason : null,
      guardSubframeRejected: !subframeGuard.ok,
      guardSubframeReason: !subframeGuard.ok ? subframeGuard.reason : null,
    },
    safeStorage: safeStorageStatus,
  };

  mkdirSync(dirname(reportPath), { recursive: true });
  writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`, "utf8");

  shutdown();
  app.exit(0);
}

run().catch((err) => {
  console.error("G1 probe failed:", err);
  app.exit(1);
});
