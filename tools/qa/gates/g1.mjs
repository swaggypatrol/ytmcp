// G1: native Wayland, GPU, renderer sandboxing, session isolation, IPC rejection,
// scale, frame interval, resize, window reopen and IME.
import { spawn } from "node:child_process";
import { join } from "node:path";
import { ELECTRON, PROD_MAIN, REPO } from "../lib/app.mjs";
import { check } from "../lib/evidence.mjs";
import {
  describeProcesses,
  kwinActivate,
  kwinActiveWindow,
  kwinWindowsFor,
  percentile,
  round,
  run,
  sleep,
} from "../lib/sys.mjs";
import { navigate } from "./common.mjs";

const APP_ID = "local.ytmcp.Desktop";

function parseSs(text) {
  return text
    .split("\n")
    .filter((l) => l.startsWith("u_str") || l.startsWith("u_seq"))
    .map((l) => {
      const t = l.trim().split(/\s+/);
      const users = l.includes("users:") ? l.slice(l.indexOf("users:")) : "";
      const pids = [...users.matchAll(/pid=(\d+)/g)].map((m) => Number(m[1]));
      return { local: t[4], localIno: t[5], peer: t[6], peerIno: t[7], pids };
    });
}

/** Unix-socket peer evidence: which app processes are connected to the compositor or Xwayland. */
/** Isolated world in which the player preload exposes ytmcpBridge (preload/channels.ts). */
const BRIDGE_WORLD = 1001;

export async function socketPeers(appPids) {
  const r = await run("ss", ["-xpn"]);
  const rows = parseSs(r.stdout);
  const wl = `${process.env.XDG_RUNTIME_DIR}/${process.env.WAYLAND_DISPLAY}`;
  const waylandServerInodes = new Set(rows.filter((x) => x.local === wl).map((x) => x.localIno));
  const x11ServerInodes = new Set(rows.filter((x) => /\.X11-unix\/X\d+$/.test(x.local)).map((x) => x.localIno));
  const pidSet = new Set(appPids);
  const mine = rows.filter((x) => x.pids.some((p) => pidSet.has(p)));
  const waylandClients = [...new Set(mine.filter((x) => waylandServerInodes.has(x.peerIno)).flatMap((x) => x.pids.filter((p) => pidSet.has(p))))];
  const x11Clients = [...new Set(mine.filter((x) => x11ServerInodes.has(x.peerIno)).flatMap((x) => x.pids.filter((p) => pidSet.has(p))))];
  const roles = new Map(describeProcesses(appPids).map((p) => [p.pid, p.role]));
  return {
    waylandSocket: wl.replace(process.env.XDG_RUNTIME_DIR, "$XDG_RUNTIME_DIR"),
    waylandServerSockets: waylandServerInodes.size,
    waylandClients: waylandClients.map((pid) => ({ pid, role: roles.get(pid) ?? "?" })),
    x11Clients: x11Clients.map((pid) => ({ pid, role: roles.get(pid) ?? "?" })),
  };
}

export async function waylandChecks(app) {
  const pids = app.pids();
  const peers = await socketPeers(pids);
  const kwin = await kwinWindowsFor(pids);
  const wins = kwin.results ?? [];
  const appWins = wins.filter((w) => w.pid === app.mainPid);
  const native = appWins.length > 0 && appWins.every((w) => w.cls.startsWith("KWin::XdgToplevelWindow") && !w.x11);
  const idOk = appWins.length > 0 && appWins.every((w) => w.desktopFileName === APP_ID || w.resourceClass === APP_ID);
  const measured = {
    socketPeers: { ...peers, browserProcessIsWaylandClient: peers.waylandClients.some((c) => c.pid === app.mainPid), browserProcessIsX11Client: peers.x11Clients.some((c) => c.pid === app.mainPid) },
    kwinWindows: appWins.map((w) => ({ cls: w.cls.replace(/\(0x[0-9a-f]+\)/, ""), resourceClass: w.resourceClass, resourceName: w.resourceName, desktopFileName: w.desktopFileName, x11: w.x11, normalWindow: w.normalWindow })),
    kwinScriptOk: kwin.ok,
  };
  return {
    measured,
    checks: [
      check("G1.waylandSocket", "the browser (window-owning) process is a client of the compositor's Wayland socket and not of Xwayland", {
        measured: measured.socketPeers,
        pass: measured.socketPeers.browserProcessIsWaylandClient && !measured.socketPeers.browserProcessIsX11Client,
      }),
      check("G1.x11HelperConnections", "helper processes connected to Xwayland (informational)", {
        measured: peers.x11Clients,
        pass: peers.x11Clients.length === 0,
        support: true,
        note: "a helper's X11 connection does not make the window Xwayland; the window protocol is G1.kwinNative",
      }),
      check("G1.kwinNative", "KWin reports the app window as an xdg-toplevel (native Wayland), not an X11/Xwayland window", {
        measured: measured.kwinWindows,
        pass: native,
        blocked: kwin.ok ? null : "KWin script could not run",
      }),
      check("G1.appId", `KWin desktopFileName/app_id equals ${APP_ID}`, {
        measured: measured.kwinWindows.map((w) => ({ desktopFileName: w.desktopFileName, resourceClass: w.resourceClass })),
        pass: idOk,
        blocked: kwin.ok ? null : "KWin script could not run",
      }),
    ],
  };
}

export async function gpuChecks(app, snapshot) {
  const m = await app.call("metrics", { gpuInfo: "complete" }, 60_000);
  const fs = m.gpuFeatureStatus ?? {};
  const dev = m.gpuInfoBasic?.gpuDevice ?? [];
  const active = dev.find((d) => d.active) ?? dev[0] ?? null;
  const hw = (v) => typeof v === "string" && v.startsWith("enabled");
  const measured = {
    featureStatus: fs,
    activeGpu: active ? { vendorId: active.vendorId, deviceId: active.deviceId, driverVendor: active.driverVendor ?? null, driverVersion: active.driverVersion ?? null } : null,
    glRenderer: m.gpuInfoBasic?.auxAttributes?.glRenderer ?? null,
    glImplementationParts: m.gpuInfoBasic?.auxAttributes?.glImplementationParts ?? null,
    gpuInfoKeys: Object.keys(m.gpuInfoBasic ?? {}),
    auxAttributes: Object.fromEntries(Object.entries(m.gpuInfoBasic?.auxAttributes ?? {}).filter(([k]) => /gl|display|vulkan|ozone|angle|passthrough/i.test(k))),
    hostHealthGpuAccelerated: snapshot?.hostHealth?.gpuAccelerated ?? null,
    switches: m.switches,
    argv: m.argv,
  };
  const softwareRenderer = /swiftshader|llvmpipe|software/i.test(String(measured.glRenderer ?? ""));
  measured.interpretation = [
    `gpu_compositing=${fs.gpu_compositing ?? "?"}, rasterization=${fs.rasterization ?? "?"}, webgl=${fs.webgl ?? "?"}, video_decode=${fs.video_decode ?? "?"}, vulkan=${fs.vulkan ?? "?"}`,
    softwareRenderer ? "GL renderer string names a software rasterizer" : `GL renderer: ${measured.glRenderer ?? "unknown"}`,
    "Feature status is Chromium's own report; it does not by itself prove the compositor path is free of fallbacks. Frame timing below is the behavioural check.",
  ];
  return {
    measured,
    metrics: m,
    checks: [
      check("G1.gpu", "Chromium reports hardware GPU compositing and rasterization with a hardware GL renderer (no software fallback)", {
        measured: { gpu_compositing: fs.gpu_compositing, rasterization: fs.rasterization, glRenderer: measured.glRenderer },
        pass: hw(fs.gpu_compositing) && hw(fs.rasterization) && !softwareRenderer,
      }),
      check("G1.hostHealthGpu", "core hostHealth.gpuAccelerated agrees with Chromium's feature status", {
        measured: { hostHealth: measured.hostHealthGpuAccelerated, chromium: fs.gpu_compositing },
        pass: measured.hostHealthGpuAccelerated === hw(fs.gpu_compositing),
      }),
    ],
  };
}

export async function sandboxChecks(app) {
  const procs = describeProcesses(app.pids());
  const browser = procs.find((p) => p.role === "browser(main)");
  const renderers = procs.filter((p) => p.role === "renderer");
  const wins = await app.call("windows");
  const mainProbe = await app.evalMain(`({
    preloadSandboxed: window.ytmcpUi ? window.ytmcpUi.getSandboxStatus().sandboxed : null,
    pageHasProcess: typeof process !== "undefined",
    pageHasRequire: typeof require !== "undefined",
    uiApiKeys: window.ytmcpUi ? Object.keys(window.ytmcpUi).sort() : null
  })`);
  const playerProbe = await app.evalPlayer(`({
    pageHasProcess: typeof process !== "undefined",
    pageHasRequire: typeof require !== "undefined"
  })`);
  playerProbe.bridgeWorld = await app.evalPlayer(`({
    hasProcess: typeof process !== "undefined",
    hasRequire: typeof require !== "undefined",
    bridgeApiKeys: window.ytmcpBridge ? Object.keys(window.ytmcpBridge).sort() : null
  })`, { worldId: BRIDGE_WORLD });
  const perRenderer = renderers.map((r) => ({
    pid: r.pid,
    seccomp: r.seccomp,
    noNewPrivs: r.noNewPrivs,
    noSandboxFlag: r.noSandboxFlag,
    userNsDiffersFromBrowser: r.userNs !== browser?.userNs,
    pidNsDiffersFromBrowser: r.pidNs !== browser?.pidNs,
    netNsDiffersFromBrowser: r.netNs !== browser?.netNs,
    window: wins.find((w) => w.rendererPid === r.pid)?.role ?? "no-window (spare/worker)",
  }));
  const renderersOk = perRenderer.length > 0 && perRenderer.every((r) => r.seccomp === "2" && r.noNewPrivs === "1" && !r.noSandboxFlag && r.userNsDiffersFromBrowser && r.pidNsDiffersFromBrowser);
  const prefsOk = wins.every((w) => w.webPreferences.sandbox === true && w.webPreferences.contextIsolation === true && w.webPreferences.nodeIntegration === false && w.webPreferences.webSecurity !== false);
  const otherProcs = procs.filter((p) => !["renderer", "browser(main)"].includes(p.role)).map((p) => ({ role: p.role, seccomp: p.seccomp, noNewPrivs: p.noNewPrivs, noSandboxFlag: p.noSandboxFlag }));
  return {
    measured: { perRenderer, windows: wins.map((w) => ({ role: w.role, webPreferences: w.webPreferences })), mainProbe, playerProbe, otherProcesses: otherProcs },
    checks: [
      check("G1.rendererSandbox", "every renderer runs with seccomp-bpf (Seccomp: 2), NoNewPrivs, own user+pid namespace and no --no-sandbox", {
        measured: perRenderer,
        pass: renderersOk,
      }),
      check("G1.webPreferences", "both windows: sandbox, contextIsolation, no nodeIntegration, webSecurity", {
        measured: wins.map((w) => ({ role: w.role, ...w.webPreferences })),
        pass: prefsOk && wins.some((w) => w.role === "main") && wins.some((w) => w.role === "player"),
      }),
      check("G1.processSandboxed", "main UI preload reports process.sandboxed and page worlds have no Node globals", {
        measured: { mainProbe, playerProbe },
        pass: mainProbe.preloadSandboxed === true && !mainProbe.pageHasProcess && !mainProbe.pageHasRequire && !playerProbe.pageHasProcess && !playerProbe.pageHasRequire && !playerProbe.bridgeWorld?.hasProcess && !playerProbe.bridgeWorld?.hasRequire,
        note: "the player preload exposes no sandbox probe; its renderer's OS sandbox is covered by G1.rendererSandbox",
      }),
      check("G1.noNoSandboxAnywhere", "no app process was started with --no-sandbox", {
        measured: procs.map((p) => ({ role: p.role, noSandboxFlag: p.noSandboxFlag })),
        pass: procs.every((p) => !p.noSandboxFlag),
      }),
    ],
  };
}

export async function sessionChecks(app) {
  const s = await app.call("sessions");
  const uiApi = await app.evalMain(`window.ytmcpUi ? Object.keys(window.ytmcpUi).sort() : null`);
  const mainWc = s.webContents.filter((w) => w.origin === "null" || w.origin.startsWith("app"));
  const playerWc = s.webContents.filter((w) => w.origin === "https://music.youtube.com");
  return {
    measured: { ...s, uiApi },
    checks: [
      check("G1.sessions", "UI and player use distinct sessions: player persistent partition, UI ephemeral without cookies", {
        measured: s,
        pass: s.distinctObjects && s.playerPersistent && !s.uiPersistent && s.uiCookieCount === 0 && mainWc.every((w) => w.sessionIs === "ui") && playerWc.every((w) => w.sessionIs === "player"),
      }),
      check("G1.uiNoCookieEntry", "UI preload API exposes no cookie/credential/eval entry point", {
        measured: uiApi,
        pass: Array.isArray(uiApi) && uiApi.every((k) => ["getHostInfo", "getSandboxStatus", "onEnvelope", "sendEnvelope"].includes(k)),
      }),
    ],
  };
}

const envelope = (kind, payload = {}) =>
  `({ protocolVersion: 1, kind: ${JSON.stringify(kind)}, coreEpoch: null, accountEpoch: null, bridgeEpoch: null, requestId: "qa-reject-" + Date.now(), seq: 1, payload: ${JSON.stringify(payload)} })`;

/** Sends deliberately invalid envelopes through the real preloads into the host IPC guard. */
export async function ipcRejectionChecks(app) {
  const logStart = app.logs().length;
  const sendUi = (expr) => app.evalMain(`window.ytmcpUi.sendEnvelope(${expr}).then((r) => r, (e) => ({ thrown: String(e) }))`);
  const results = {
    uiSendsBridgeOnlyKind: await sendUi(`Object.assign(${envelope("player.snapshotRequest")}, { payload: null })`),
    uiSendsUnknownKind: await sendUi(envelope("ui.evalScript", { js: "1" })),
    uiSendsOversized: await sendUi(envelope("ui.ping", { pad: "x".repeat(2 * 1024 * 1024) })),
    uiSendsBadVersion: await sendUi(`Object.assign(${envelope("ui.ping")}, { protocolVersion: 99 })`),
    // The player preload exposes its API only in the bridge's isolated world (id 1001).
    playerSendsUiKind: await app.evalPlayer(`window.ytmcpBridge ? window.ytmcpBridge.sendEnvelope(${envelope("ui.ping")}).then((r) => r, (e) => ({ thrown: String(e) })) : { unavailable: true }`, { worldId: BRIDGE_WORLD }),
    playerSendsHostKind: await app.evalPlayer(`window.ytmcpBridge ? window.ytmcpBridge.sendEnvelope(Object.assign(${envelope("host.powerResume")}, { payload: null })).then((r) => r, (e) => ({ thrown: String(e) })) : { unavailable: true }`, { worldId: BRIDGE_WORLD }),
    playerMainWorldHasNoApi: await app.evalPlayer(`typeof window.ytmcpBridge === "undefined"`),
  };
  // Subframes never receive the preload APIs (nodeIntegrationInSubFrames=false + top-frame checks).
  results.uiSubframeApi = await app.evalMain(`new Promise((resolve) => {
    const f = document.createElement("iframe");
    f.srcdoc = "<p>qa</p>";
    f.style.display = "none";
    f.onload = () => { const has = typeof f.contentWindow.ytmcpUi !== "undefined"; f.remove(); resolve({ subframeHasUiApi: has }); };
    document.body.appendChild(f);
    setTimeout(() => resolve({ timeout: true }), 4000);
  })`);
  results.playerInjectedSubframe = await app.evalPlayer(`new Promise((resolve) => {
    const f = document.createElement("iframe");
    f.src = "about:blank";
    f.style.display = "none";
    f.onload = () => { let has = null; try { has = typeof f.contentWindow.ytmcpBridge !== "undefined"; } catch (e) { has = "cross-origin"; } f.remove(); resolve({ subframeHasBridgeApi: has }); };
    document.body.appendChild(f);
    setTimeout(() => resolve({ timeout: true }), 4000);
  })`);
  results.playerSubframes = await app.call("evalFrames", { target: "player", js: `({ hasBridge: typeof window.ytmcpBridge !== "undefined", hasUi: typeof window.ytmcpUi !== "undefined" })` });
  await sleep(300);
  const rejectionLogs = app
    .logs()
    .slice(logStart)
    .map((l) => l.line)
    .filter((l) => /ipc_(ui|bridge)_rejected/.test(l))
    .map((l) => {
      try {
        const o = JSON.parse(l);
        return `${o.event}:${o.detail?.reason}:${o.detail?.code}`;
      } catch {
        return l.slice(0, 120);
      }
    });
  // each probe must be rejected for its own reason, not for an incidental schema error
  const expect = {
    uiSendsBridgeOnlyKind: /not allowed for role/,
    uiSendsUnknownKind: /kind must be equal to one of the allowed values/,
    uiSendsOversized: /exceeds \d+ byte limit/,
    uiSendsBadVersion: /protocolVersion/,
    playerSendsUiKind: /not allowed for role/,
    playerSendsHostKind: /not allowed for role/,
  };
  const verdicts = Object.fromEntries(Object.entries(expect).map(([k, re]) => [k, results[k]?.ok === false && re.test(String(results[k]?.message ?? ""))]));
  const allRejected = Object.values(verdicts).every(Boolean);
  const subframesClean =
    results.playerMainWorldHasNoApi === true &&
    results.uiSubframeApi?.subframeHasUiApi === false &&
    results.playerInjectedSubframe?.subframeHasBridgeApi === false &&
    results.playerSubframes.every((f) => f.error || (!f.result?.hasBridge && !f.result?.hasUi));
  return {
    measured: { results: Object.fromEntries(Object.entries(results).map(([k, v]) => [k, v])), rejectionLogs },
    checks: [
      check("G1.ipcRejects", "host IPC guard rejects wrong-role kinds, unknown kinds, oversized frames and wrong protocol versions from both renderers", {
        measured: { results, verdicts, rejectionLogs },
        pass: allRejected,
      }),
      check("G1.subframesNoApi", "subframes in the UI and the player page, and the player page's own main world, get no preload API", {
        measured: { playerMainWorldHasNoApi: results.playerMainWorldHasNoApi, ui: results.uiSubframeApi, playerInjected: results.playerInjectedSubframe, existingPlayerFrames: results.playerSubframes },
        pass: subframesClean,
      }),
      check("G1.wrongSenderFrame", "IPC from a subframe / other origin is rejected by sender-frame checks", {
        measured: "unit tests apps/desktop/src/windows/__tests__/security-guards.test.ts",
        pass: false,
        notRun: "not reachable live: preload APIs do not exist in subframes or other origins, so no renderer can send such IPC; covered only by PASS_MOCK unit tests",
        support: true,
      }),
    ],
  };
}

/**
 * Navigates the real player to an allowlisted non-music origin (consent page) and back:
 * the bridge API must be absent there and bridge lifecycle must report a non-music origin.
 */
export async function nonMusicOriginCheck(app) {
  const since = (await app.state()).seq;
  const epochBefore = (await app.state()).lastBridgeReady?.bridgeEpoch ?? null;
  // A playing YouTube Music document can veto raw navigation. Use the
  // app's normal player teardown first, without clearing its session.
  await app.call("winop", { target: "player", op: "appBlankPlayer" });
  await app.call("winop", { target: "player", op: "loadURL", args: ["https://consent.youtube.com/"] }, 30_000);
  await sleep(2500);
  const there = await app.evalPlayer(`({ origin: location.origin, hasBridge: typeof window.ytmcpBridge !== "undefined" })`, { worldId: BRIDGE_WORLD }).catch((e) => ({ error: String(e) }));
  const stateThere = await app.snapshot();
  const toastThere = await app.evalMain(`(() => {
    const toast = document.querySelector('[data-testid="toast-bridge-disconnected"]');
    const action = toast?.querySelector('[data-testid="btn-open-official-player"]:enabled');
    return { shown: Boolean(toast), actionPresent: Boolean(action), action: action?.textContent?.trim() ?? null };
  })()`).catch((e) => ({ error: String(e) }));
  await app.call("winop", { target: "player", op: "loadURL", args: ["https://music.youtube.com/"] }, 45_000);
  const back = await app.waitFor((s, st) => st.lastBridgeReady && st.lastBridgeReady.bridgeEpoch !== epochBefore && s?.player?.connection === "ready", { timeoutMs: 45_000 });
  const evs = await app.events(since);
  const onConsent = ["https://consent.youtube.com", "https://consent.google.com"].includes(there.origin);
  return {
    measured: {
      there,
      connectionWhileAway: stateThere?.player?.connection ?? null,
      disconnectedToastWhileAway: toastThere,
      bridgeReadyAfterReturn: back.ok,
      renewedBridgeEpoch: evs.filter((e) => e.type === "bridge->host" && e.kind === "bridge.ready").length,
    },
    checks: [
      check("G1.authDomainNoBridge", "on a non-music allowlisted origin (consent.youtube.com) the player has no bridge API", {
        measured: { there, connectionWhileAway: stateThere?.player?.connection ?? null },
        pass: onConsent && there.hasBridge === false,
      }),
      check("G1.awayNotReady", "on a non-music origin core reports the bridge disconnected and the UI offers Open player in the toast", {
        measured: { connectionWhileAway: stateThere?.player?.connection ?? null, disconnectedToastWhileAway: toastThere },
        pass: onConsent && stateThere?.player?.connection === "disconnected" && toastThere?.shown && toastThere?.actionPresent && Boolean(toastThere.action),
      }),
      check("G1.bridgeReturns", "returning to music.youtube.com re-handshakes the bridge with a new bridgeEpoch", {
        measured: { ok: back.ok, waitedMs: back.waitedMs },
        pass: back.ok,
      }),
    ],
  };
}

/** rAF frame-interval sample in the real UI while scrolling the main region. */
export async function frameSample(app, { seconds = 5, scroll = true, scrollSelector = '[data-testid="main-scroll-region"]' } = {}) {
  const js = `new Promise((resolve) => {
    const region = document.querySelector(${JSON.stringify(scrollSelector)});
    const out = []; let last = performance.now(); const end = last + ${seconds * 1000};
    let dir = 1;
    let scrollDistancePx = 0;
    function tick(now) {
      out.push(now - last); last = now;
      if (${scroll} && region) {
        const previous = region.scrollTop;
        region.scrollTop += 6 * dir;
        scrollDistancePx += Math.abs(region.scrollTop - previous);
        if (region.scrollTop + region.clientHeight >= region.scrollHeight - 2 || region.scrollTop <= 0) dir = -dir;
      }
      if (now < end) requestAnimationFrame(tick); else resolve({ intervals: out.slice(1), dpr: devicePixelRatio, w: innerWidth, h: innerHeight, scrollable: region ? region.scrollHeight > region.clientHeight : false, scrollDistancePx, hidden: document.hidden });
    }
    requestAnimationFrame(tick);
  })`;
  const r = await app.evalMain(js, (seconds + 10) * 1000);
  const iv = r.intervals;
  return {
    frames: iv.length,
    dpr: r.dpr,
    viewport: { w: r.w, h: r.h },
    scrollable: r.scrollable,
    scrollDistancePx: r.scrollDistancePx,
    documentHidden: r.hidden,
    p50Ms: round(percentile(iv, 50), 2),
    p95Ms: round(percentile(iv, 95), 2),
    p99Ms: round(percentile(iv, 99), 2),
    maxMs: round(Math.max(...iv), 2),
    meanHz: round(1000 / (iv.reduce((a, b) => a + b, 0) / iv.length), 1),
    intervals: iv.map((v) => round(v, 2)),
  };
}

export function frameCheck(id, sample, refreshHz) {
  const budget = 1000 / refreshHz;
  const over2x = sample.intervals.filter((v) => v > 2 * budget).length / Math.max(1, sample.intervals.length);
  const { intervals, ...rest } = sample;
  const measured = { ...rest, refreshHz, over2xBudgetRatio: round(over2x, 4), histogram: histogram(intervals, budget) };
  return check(id, `frame intervals at ${round(refreshHz, 2)} Hz: p50 within 1.5x budget and <5% frames over 2x budget`, {
    measured,
    pass: sample.frames > 50 && sample.p50Ms <= budget * 1.5 && over2x < 0.05,
  });
}

function histogram(values, budget) {
  const bins = { "<=1.2x": 0, "1.2-2x": 0, "2-3x": 0, ">3x": 0 };
  for (const v of values) {
    if (v <= budget * 1.2) bins["<=1.2x"] += 1;
    else if (v <= budget * 2) bins["1.2-2x"] += 1;
    else if (v <= budget * 3) bins["2-3x"] += 1;
    else bins[">3x"] += 1;
  }
  return bins;
}

const LAYOUT_JS = `(() => {
  const bar = document.querySelector('[data-testid="player-bar"]')?.getBoundingClientRect();
  const region = document.querySelector('[data-testid="main-scroll-region"]');
  const pad = region ? parseFloat(getComputedStyle(region).paddingBottom) : null;
  return {
    viewport: { w: innerWidth, h: innerHeight },
    docOverflowX: document.documentElement.scrollWidth > innerWidth + 1,
    playerBarInView: bar ? bar.bottom <= innerHeight + 1 && bar.top >= 0 && bar.right <= innerWidth + 1 : false,
    playerBarHeight: bar ? Math.round(bar.height) : null,
    contentBottomPadding: pad,
    paddingCoversBar: bar && pad != null ? pad >= bar.height : false
  };
})()`;

export async function resizeChecks(app) {
  const sizes = [[900, 600], [1280, 800]];
  const out = [];
  for (const [w, h] of sizes) {
    await app.call("winop", { target: "main", op: "setSize", args: [w, h] });
    await sleep(900);
    const win = (await app.call("windows")).find((x) => x.role === "main");
    out.push({ requested: { w, h }, contentBounds: win?.contentBounds, layout: await app.evalMain(LAYOUT_JS) });
  }
  return {
    measured: out,
    checks: [
      check("G1.resize", "UI usable at 900x600 and 1280x800: no horizontal overflow, player bar in view and not covering the list end", {
        measured: out,
        pass: out.every((o) => !o.layout.docOverflowX && o.layout.playerBarInView && o.layout.paddingCoversBar),
      }),
    ],
  };
}

/**
 * Window reopen: closing the player hides it (not destroyed); closing the main window with
 * close-to-tray hides it, and a second launch focuses the existing instance.
 */
export async function reopenChecks(app, snapshot) {
  const before = await app.call("windows");
  await app.call("winop", { target: "player", op: "close" });
  await sleep(600);
  const afterPlayerClose = (await app.call("windows")).find((w) => w.role === "player");
  await app.call("winop", { target: "main", op: "close" });
  await sleep(800);
  const afterMainClose = (await app.call("windows")).find((w) => w.role === "main");
  // A second instance with the same profile must hand over to the running one and exit.
  const t0 = Date.now();
  const second = await new Promise((resolve) => {
    const env = { ...process.env, PULSE_SINK: "ytmcp_null" };
    delete env.YTMCP_QA_SOCKET;
    const child = spawn(ELECTRON, [PROD_MAIN], { cwd: REPO, env, stdio: "ignore" });
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      resolve({ exited: false });
    }, 15_000);
    child.on("exit", (code) => {
      clearTimeout(timer);
      resolve({ exited: true, code, ms: Date.now() - t0 });
    });
  });
  await sleep(800);
  const afterSecond = (await app.call("windows")).find((w) => w.role === "main");
  const closeToTray = snapshot?.settings?.closeToTray ?? null;
  const trayAvailable = snapshot?.hostHealth?.trayAvailable ?? null;
  const measured = {
    closeToTray,
    trayAvailable,
    before: before.map((w) => ({ role: w.role, visible: w.visible })),
    playerAfterClose: afterPlayerClose ? { exists: true, visible: afterPlayerClose.visible } : { exists: false },
    mainAfterClose: afterMainClose ? { exists: true, visible: afterMainClose.visible } : { exists: false },
    secondInstance: second,
    mainAfterSecondLaunch: afterSecond ? { visible: afterSecond.visible, minimized: afterSecond.minimized } : null,
  };
  return {
    measured,
    checks: [
      check("G1.playerCloseHides", "closing the player window hides it; the page keeps running", {
        measured: measured.playerAfterClose,
        pass: measured.playerAfterClose.exists && measured.playerAfterClose.visible === false,
      }),
      check("G1.mainReopen", "main window closed to tray is shown again by a second launch, which exits", {
        measured: { mainAfterClose: measured.mainAfterClose, secondInstance: second, mainAfterSecondLaunch: measured.mainAfterSecondLaunch },
        pass: closeToTray && trayAvailable ? measured.mainAfterClose.visible === false && second.exited && measured.mainAfterSecondLaunch?.visible === true : false,
        blocked: closeToTray && trayAvailable ? null : "closeToTray off or no tray: closing the main window quits (covered by G2 restart)",
      }),
    ],
  };
}

/**
 * IME: types pinyin through a temporary uinput keyboard into the real search box with
 * fcitx5 active. Composition must not trigger a search or a shortcut. The previous
 * fcitx5 state is restored. Aborts (BLOCKED) unless the app window is the active one.
 */
export async function imeCheck(app) {
  const manual = "manual: check IME composition by hand";
  const imBefore = { state: (await run("fcitx5-remote", [])).stdout.trim(), im: (await run("fcitx5-remote", ["-n"])).stdout.trim() };
  const metrics = await app.call("metrics");
  await app.call("winop", { target: "main", op: "show" });
  await navigate(app, "search");
  await app.setInput('[data-testid="search-input"]', "", { change: false });
  await app.evalMain(`(() => {
    window.__qaIme = { events: [] };
    const el = document.querySelector('[data-testid="search-input"]');
    for (const t of ["compositionstart", "compositionupdate", "compositionend"]) el.addEventListener(t, (e) => window.__qaIme.events.push({ t, data: e.data }));
    el.addEventListener("keydown", (e) => window.__qaIme.events.push({ t: "keydown", key: e.key, composing: e.isComposing }));
    el.focus();
    return true;
  })()`);
  await kwinActivate(app.mainPid, "YouTube");
  await sleep(500);
  const active = await kwinActiveWindow();
  const activePid = active.results?.[0]?.pid ?? null;
  const focused = await app.evalMain(`document.activeElement?.getAttribute("data-testid") ?? null`);
  if (activePid !== app.mainPid || focused !== "search-input") {
    return {
      measured: { imBefore, activePid: activePid === app.mainPid ? "app" : "other", focused, enableWaylandIme: metrics.switches.enableWaylandIme },
      checks: [check("G1.ime", "fcitx5 pinyin composition in the search box fires no search or shortcut", { measured: { activeIsApp: activePid === app.mainPid, focused }, pass: false, blocked: `app window not active/focused; refusing to type. ${manual}` })],
    };
  }
  const since = (await app.state()).seq;
  const playbackBefore = (await app.snapshot())?.player?.playback;
  await run("fcitx5-remote", ["-s", "pinyin"]);
  await run("fcitx5-remote", ["-o"]);
  const imDuring = (await run("fcitx5-remote", ["-n"])).stdout.trim();
  const typer = join(REPO, "tools/qa/uinput-type.py");
  const t1 = await run("python3", [typer, "nihao"], { timeoutMs: 15_000 });
  await sleep(1200);
  const midDom = await app.evalMain(`({ value: document.querySelector('[data-testid="search-input"]').value, hint: /composing/i.test(document.body.textContent), events: window.__qaIme.events.slice() })`);
  const midEvents = await app.events(since, ["ui->host"]);
  const t2 = await run("python3", [typer, "<space>"], { timeoutMs: 15_000 });
  await sleep(1500);
  const endDom = await app.evalMain(`({ value: document.querySelector('[data-testid="search-input"]').value, events: window.__qaIme.events.slice() })`);
  const endEvents = await app.events(since, ["ui->host"]);
  // restore the user's input method state
  if (imBefore.im) await run("fcitx5-remote", ["-s", imBefore.im]);
  if (imBefore.state === "1") await run("fcitx5-remote", ["-c"]);
  const imAfter = { state: (await run("fcitx5-remote", [])).stdout.trim(), im: (await run("fcitx5-remote", ["-n"])).stdout.trim() };
  const playbackAfter = (await app.snapshot())?.player?.playback;
  const compositionSeen = midDom.events.some((e) => e.t === "compositionstart");
  const searchesDuring = midEvents.filter((e) => e.kind === "ui.catalogQuery" && e.query?.kind === "searchSongs").length;
  const commandsDuring = endEvents.filter((e) => e.kind === "ui.playerCommand").length;
  const committedSearch = endEvents.filter((e) => e.kind === "ui.catalogQuery" && e.query?.kind === "searchSongs").map((e) => e.query.q);
  const measured = {
    enableWaylandIme: metrics.switches.enableWaylandIme,
    imBefore,
    imDuring,
    imAfter,
    typerExit: [t1.code, t2.code],
    duringComposition: { value: midDom.value, hintShown: midDom.hint, compositionEvents: midDom.events.filter((e) => e.t.startsWith("composition")).length, searchesSent: searchesDuring },
    afterCommit: { value: endDom.value, compositionEnd: endDom.events.some((e) => e.t === "compositionend"), searchesSent: committedSearch },
    playerCommandsSent: commandsDuring,
    playback: { before: playbackBefore, after: playbackAfter },
  };
  await app.setInput('[data-testid="search-input"]', "", { change: false });
  const typedOk = t1.ok && t2.ok;
  return {
    measured,
    checks: [
      check("G1.ime", "fcitx5 pinyin composition in the search box: composition events seen, no search during composition, commit searches the composed text, no shortcut fires", {
        measured,
        // The committed pinyin must have produced CJK ideographs (U+4E00..U+9FFF).
        pass: typedOk && compositionSeen && searchesDuring === 0 && commandsDuring === 0 && playbackBefore === playbackAfter && /[一-鿿]/.test(endDom.value),
        blocked: typedOk ? null : `uinput typing failed (${t1.stderr.trim().slice(0, 120)}); ${manual}`,
      }),
    ],
  };
}
