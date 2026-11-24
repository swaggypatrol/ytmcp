// QA-only Electron main entry. It installs passive observers, opens a local control
// socket for the harness, then imports the production desktop main bundle unchanged
// and runs its exported startApp() (the exact path a normal launch autostarts:
// startDesktopShell + ytm-daemon), keeping the controller so app-initiated actions
// (player reload, blanking) can be exercised through the production WindowManager. Observers only copy what crosses the existing IPC channels;
// they never alter, block or originate envelopes. Never shipped in the package.
//
// Env: YTMCP_QA_SOCKET (required), YTMCP_QA_MAIN (default: ../../apps/desktop/dist/main.mjs)
import { createHash } from "node:crypto";
import { chmodSync, existsSync, unlinkSync } from "node:fs";
import { createServer } from "node:net";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { app, BrowserWindow, ipcMain, session, webContents } from "electron";

const SOCKET = process.env.YTMCP_QA_SOCKET;
if (!SOCKET) {
  process.stderr.write("qa-entry: YTMCP_QA_SOCKET is required\n");
  process.exit(2);
}
const MAIN = resolve(process.env.YTMCP_QA_MAIN ?? resolve(import.meta.dirname, "../../apps/desktop/dist/main.mjs"));

const CH = {
  UI_SEND: "ytmcp:ui:send",
  UI_EVENT: "ytmcp:ui:event",
  BRIDGE_SEND: "ytmcp:bridge:send",
  BRIDGE_EVENT: "ytmcp:bridge:event",
};

const hash = (v) => (v == null ? null : `h:${createHash("sha256").update(String(v)).digest("hex").slice(0, 12)}`);

// ------------------------------------------------------------------ passive observation
let seq = 0;
const events = [];
const MAX_EVENTS = 50_000;
function record(type, data) {
  seq += 1;
  events.push({ seq, t: Date.now(), type, ...data });
  if (events.length > MAX_EVENTS) events.splice(0, events.length - MAX_EVENTS);
}

let snapshot = null;
let snapshotCount = 0;
let lastBridgeReady = null;
/** The production shell controller returned by startApp(). */
let controller = null;
let lastBridgePlayback = null;
const receipts = new Map();
// Library playlist titles stay in this process; the harness may only ask for an exact match.
const playlistTitles = new Map();
let lastCatalogPlaylist = null;
const LYRIC_KINDS = new Set(["synced", "plain", "instrumental", "notFound", "unavailable", "disabled"]);
const QUERY_ERROR_CODES = new Set([
  "AUTH_REQUIRED", "AUTH_EXPIRED", "ACCOUNT_MISMATCH", "RATE_LIMITED", "NETWORK_OFFLINE",
  "TIMEOUT", "UNAVAILABLE_TRACK", "UNSUPPORTED_CAPABILITY", "BRIDGE_INCOMPATIBLE",
  "STALE_CONTEXT", "BUSY", "INVALID_PAYLOAD", "STORAGE_ERROR",
]);

function playlistCommandFacts(target) {
  const supplied = typeof target?.playlistId === "string" && target.playlistId.length > 0;
  return {
    supplied,
    matchesLastCatalogPlaylist: supplied && target.playlistId === lastCatalogPlaylist,
    usesBrowsePrefix: supplied && target.playlistId.startsWith("VL"),
    hasIndex: Number.isSafeInteger(target?.startIndex),
    startsAtFirstRow: target?.startIndex === 0,
  };
}

function stripAccountLabels(value) {
  if (Array.isArray(value)) return value.map(stripAccountLabels);
  if (value && typeof value === "object") {
    const out = {};
    for (const [k, v] of Object.entries(value)) {
      if (k === "channelLabel") continue;
      out[k] = stripAccountLabels(v);
    }
    return out;
  }
  return value;
}

function applyDelta(s, d) {
  if (!s) return s;
  switch (d.deltaType) {
    case "playerChanged":
      return { ...s, revision: d.revision, player: d.player, pending: d.pending };
    case "queueReplaced":
      return { ...s, revision: d.revision, queueSummary: d.queue_summary };
    case "authChanged":
      return { ...s, revision: d.revision, accountEpoch: d.auth_state?.accountEpoch ?? null, authState: d.auth_state };
    case "capabilitiesChanged":
      return { ...s, revision: d.revision, capabilities: d.capabilities };
    case "settingsChanged":
      return { ...s, revision: d.revision, settings: d.settings };
    case "hostHealthChanged":
      return { ...s, revision: d.revision, hostHealth: d.host_health };
    case "networkChanged":
      return { ...s, revision: d.revision, networkState: d.network_state };
    case "commandUpdated":
      return { ...s, revision: d.revision, pending: { ...s.pending, lastReceipt: d.receipt } };
    default:
      return { ...s, revision: d.revision ?? s.revision };
  }
}

function noteReceipt(r, via) {
  if (!r?.commandId) return;
  const list = receipts.get(r.commandId) ?? [];
  list.push({ via, status: r.status, op: r.op, acceptedAtMs: r.acceptedAtMs, observedAtMs: r.observedAtMs ?? null, error: r.error?.code ?? null, recvAt: Date.now() });
  receipts.set(r.commandId, list);
}

function summarizeCatalog(p) {
  if (!p || typeof p !== "object") return {};
  const paged = p.items ? p : p.tracks?.items ? p.tracks : p.songs?.items ? p.songs : null;
  return {
    resultType: p.resultType ?? null,
    playlistId: p.resultType === "playlistTracks" ? hash(p.playlistId ?? null) : null,
    freshness: p.freshness ?? paged?.freshness ?? null,
    itemCount: paged?.items?.length ?? null,
    ids: paged?.items?.map((i) => i.videoId ?? i.playlistId ?? null) ?? null,
    counts: paged?.items?.map((i) => i.trackCount ?? null) ?? null,
    liked: paged?.items?.map((i) => (typeof i.isLiked === "boolean" ? i.isLiked : null)) ?? null,
    hasNextCursor: Boolean(paged?.nextCursor),
    nextCursorId: hash(paged?.nextCursor || null),
    providerVersion: paged?.providerVersion ?? null,
    accountScope: hash(paged?.accountScope ?? null),
    error: p.resultType === "error" ? { code: p.code, retriable: p.retriable } : null,
    playlistCount: Array.isArray(p.playlists) ? p.playlists.length : undefined,
  };
}

function observeCoreToUi(env) {
  if (!env || typeof env !== "object") return;
  const base = { kind: env.kind, requestId: env.requestId ?? null, coreEpoch: env.coreEpoch ?? null };
  if (env.kind === "core.snapshot") {
    snapshot = stripAccountLabels(env.payload);
    snapshotCount += 1;
    record("core->ui", { ...base, revision: snapshot?.revision });
  } else if (env.kind === "core.delta") {
    const d = stripAccountLabels(env.payload);
    snapshot = applyDelta(snapshot, d);
    if (d.deltaType === "commandUpdated") noteReceipt(d.receipt, "delta");
    if (d.deltaType === "playerChanged" && d.pending?.lastReceipt) noteReceipt(d.pending.lastReceipt, "playerChanged");
    record("core->ui", {
      ...base,
      deltaType: d.deltaType,
      revision: d.revision,
      playerConnection: d.player?.connection,
      playback: d.player?.playback,
      videoId: d.player?.currentTrack?.videoId,
      trackInstanceId: d.player?.trackInstanceId ?? undefined,
      positionMs: d.player?.timeAnchor?.positionMs,
      durationMs: d.player?.timeAnchor?.durationMs,
      networkState: d.network_state,
      receipt: d.receipt ? { commandId: d.receipt.commandId, status: d.receipt.status, op: d.receipt.op } : undefined,
    });
  } else if (env.kind === "core.commandReceipt") {
    noteReceipt(env.payload, "receipt");
    record("core->ui", { ...base, receipt: { commandId: env.payload?.commandId, status: env.payload?.status, op: env.payload?.op } });
  } else if (env.kind === "core.catalogResponse") {
    if (env.payload?.resultType === "libraryPlaylists") {
      for (const pl of env.payload.items ?? []) playlistTitles.set(pl.playlistId, pl.title);
    }
    record("core->ui", { ...base, catalog: summarizeCatalog(env.payload) });
  } else if (env.kind === "core.error") {
    record("core->ui", { ...base, error: { code: env.payload?.code, safeMessage: env.payload?.safeMessage } });
  } else if (env.kind === "core.mutationResponse") {
    record("core->ui", { ...base, mutation: { outcome: env.payload?.outcome, error: env.payload?.error?.code ?? null } });
  } else if (env.kind === "core.lyricsResponse") {
    const payload = env.payload;
    const error = payload?.error;
    const http = /^LRCLIB returned HTTP ([1-5]\d{2})$/.exec(error?.safeMessage ?? "");
    record("core->ui", { ...base, lyrics: {
      kind: LYRIC_KINDS.has(payload?.kind) ? payload.kind : null,
      errorCode: QUERY_ERROR_CODES.has(error?.code) ? error.code : null,
      retriable: error?.retriable === true,
      httpStatus: http ? Number(http[1]) : null,
    } });
  } else {
    record("core->ui", base);
  }
}

function observeUiToHost(env, ack) {
  const p = env?.payload ?? {};
  if (env?.kind === "ui.catalogQuery" && p.kind === "playlistTracks" && ack?.ok) {
    lastCatalogPlaylist = typeof p.playlistId === "string" ? p.playlistId : null;
  }
  record("ui->host", {
    kind: env?.kind,
    requestId: env?.requestId ?? null,
    commandId: p.commandId ?? null,
    op: p.op ?? null,
    args: p.args
      ? {
          positionMs: p.args.positionMs ?? undefined,
          volume: p.args.volume ?? undefined,
          muted: p.args.muted ?? undefined,
          videoId: p.args.target?.videoId ?? undefined,
          ...(p.op === "playTarget" ? { playlistContext: playlistCommandFacts(p.args.target) } : {}),
        }
      : undefined,
    query: env?.kind === "ui.catalogQuery" ? {
      kind: p.kind, cursor: Boolean(p.cursor), cursorId: hash(p.cursor || null),
      playlistId: hash(p.playlistId ?? null), q: p.query ?? undefined,
    } : undefined,
    mutation: env?.kind === "ui.mutation" ? { kind: p.kind, videoId: p.videoId, liked: p.liked ?? null, playlistId: hash(p.playlistId ?? null) } : undefined,
    ack: ack ? { ok: ack.ok, code: ack.code ?? null } : null,
  });
}

function observeBridgeToHost(env, ack) {
  const p = env?.payload ?? {};
  if (env?.kind === "bridge.ready") {
    lastBridgeReady = { bridgeEpoch: p.bridgeEpoch, loggedInObserved: p.loggedInObserved, accountHint: hash(p.accountHint ?? null), bridgeVersion: p.bridgeVersion, at: Date.now() };
    record("bridge->host", { kind: env.kind, ...lastBridgeReady, ack: ack?.ok ?? null });
  } else if (env?.kind === "bridge.playbackObserved") {
    lastBridgePlayback = { at: Date.now(), bridgeEpoch: env.bridgeEpoch ?? null, summary: Object.keys(p) };
  } else {
    record("bridge->host", { kind: env?.kind, bridgeEpoch: env?.bridgeEpoch ?? null, commandId: p.commandId ?? null, dispatched: p.dispatched ?? null, ack: ack ? { ok: ack.ok, code: ack.code ?? null } : null });
  }
}

const originalHandle = ipcMain.handle.bind(ipcMain);
ipcMain.handle = (channel, handler) => {
  if (channel !== CH.UI_SEND && channel !== CH.BRIDGE_SEND) return originalHandle(channel, handler);
  return originalHandle(channel, async (event, raw, ...rest) => {
    const ack = await handler(event, raw, ...rest);
    try {
      if (channel === CH.UI_SEND) observeUiToHost(raw, ack);
      else observeBridgeToHost(raw, ack);
    } catch {
      // observation must never affect the app
    }
    return ack;
  });
};

app.on("web-contents-created", (_e, wc) => {
  const originalSend = wc.send.bind(wc);
  wc.send = (channel, ...args) => {
    try {
      if (channel === CH.UI_EVENT) observeCoreToUi(args[0]);
      else if (channel === CH.BRIDGE_EVENT) {
        const env = args[0];
        record("core->bridge", {
          kind: env?.kind, commandId: env?.payload?.commandId ?? null, op: env?.payload?.op ?? null,
          ...(env?.payload?.op === "playTarget" ? { playlistContext: playlistCommandFacts(env.payload.args?.target) } : {}),
        });
      }
    } catch {
      // ignore
    }
    return originalSend(channel, ...args);
  };
  wc.on("render-process-gone", (_ev, details) => record("renderer-gone", { role: classify(wc), url: originOf(wc.getURL()), reason: details.reason, exitCode: details.exitCode }));
  wc.on("did-finish-load", () => record("did-finish-load", { wcId: wc.id, url: originOf(wc.getURL()) }));
  // observed only: not calling preventDefault keeps Electron's default (the unload stays blocked)
  wc.on("will-prevent-unload", () => record("will-prevent-unload", { wcId: wc.id, url: originOf(wc.getURL()) }));
});

function originOf(url) {
  try {
    return new URL(url).origin;
  } catch {
    return String(url).slice(0, 32);
  }
}

// ------------------------------------------------------------------ window lookup
/** Window role by session: the UI uses the ephemeral UI partition, the player (and its auth popups) the persistent one. */
function classify(wc) {
  if (wc.session === session.fromPartition("ytm-ui-ephemeral")) return "main";
  if (wc.session === session.fromPartition("persist:player-default")) {
    return wc.hostWebContents || BrowserWindow.fromWebContents(wc)?.getParentWindow() ? "popup" : "player";
  }
  return "other";
}

function findWindow(target) {
  const wins = BrowserWindow.getAllWindows().filter((w) => !w.isDestroyed() && classify(w.webContents) === target);
  // the player window is the oldest persistent-session window; auth popups come later
  return wins.sort((a, b) => a.id - b.id)[0] ?? null;
}

function windowInfo(win) {
  const wc = win.webContents;
  const prefs = typeof wc.getLastWebPreferences === "function" ? wc.getLastWebPreferences() : {};
  return {
    id: win.id,
    role: classify(wc),
    origin: originOf(wc.getURL()),
    visible: win.isVisible(),
    minimized: win.isMinimized(),
    focused: win.isFocused(),
    bounds: win.getBounds(),
    contentBounds: win.getContentBounds(),
    rendererPid: wc.getOSProcessId(),
    audible: wc.isCurrentlyAudible(),
    audioMuted: wc.isAudioMuted(),
    webPreferences: {
      sandbox: prefs?.sandbox ?? null,
      contextIsolation: prefs?.contextIsolation ?? null,
      nodeIntegration: prefs?.nodeIntegration ?? null,
      nodeIntegrationInSubFrames: prefs?.nodeIntegrationInSubFrames ?? null,
      webSecurity: prefs?.webSecurity ?? null,
      partition: prefs?.partition ?? null,
      hasPreload: Boolean(prefs?.preload),
    },
  };
}

// ------------------------------------------------------------------ control socket ops
const ops = {
  async ping() {
    return { pid: process.pid, ready: app.isReady(), versions: { electron: process.versions.electron, chrome: process.versions.chrome, node: process.versions.node, v8: process.versions.v8 } };
  },
  async windows() {
    return BrowserWindow.getAllWindows().filter((w) => !w.isDestroyed()).map(windowInfo);
  },
  async state() {
    return { snapshot, snapshotCount, lastBridgeReady, lastBridgePlayback, seq };
  },
  async events({ since = 0, types = null } = {}) {
    return events.filter((e) => e.seq > since && (!types || types.includes(e.type)));
  },
  async findPlaylistByTitle({ title, ids }) {
    return [...new Set(ids)].filter((id) => playlistTitles.get(id) === title);
  },
  async receipts({ commandId }) {
    return receipts.get(commandId) ?? [];
  },
  async eval({ target, js, userGesture = false, worldId = null }) {
    const win = findWindow(target);
    if (!win) throw new Error(`no ${target} window`);
    // worldId: an isolated world, e.g. 1001 where the player preload exposes ytmcpBridge.
    if (worldId != null) return win.webContents.executeJavaScriptInIsolatedWorld(worldId, [{ code: js }], userGesture);
    return win.webContents.executeJavaScript(js, userGesture);
  },
  async evalFrames({ target, js }) {
    const win = findWindow(target);
    if (!win) throw new Error(`no ${target} window`);
    const out = [];
    for (const frame of win.webContents.mainFrame.framesInSubtree) {
      if (frame === win.webContents.mainFrame) continue;
      try {
        out.push({ origin: frame.origin, result: await frame.executeJavaScript(js) });
      } catch (err) {
        out.push({ origin: frame.origin, error: String(err?.message ?? err) });
      }
    }
    return out;
  },
  async winop({ target, op, args = [] }) {
    const win = findWindow(target);
    if (!win) throw new Error(`no ${target} window`);
    switch (op) {
      case "hide": win.hide(); break;
      case "show": win.show(); break;
      case "showInactive": win.showInactive(); break;
      case "minimize": win.minimize(); break;
      case "restore": win.restore(); break;
      case "focus": win.focus(); break;
      case "close": win.close(); break;
      case "setSize": win.setSize(args[0], args[1]); break;
      case "reload": win.webContents.reload(); break;
      case "loadURL": await win.webContents.loadURL(args[0]).catch((e) => record("qa-load-error", { message: String(e?.message ?? e) })); break;
      case "crashRenderer": {
        // forcefullyCrashRenderer() is a no-op for this hidden window in Electron 45; then the
        // renderer process is killed from outside, which is what a real renderer crash looks like.
        const wc = win.webContents;
        const pid = wc.getOSProcessId();
        wc.forcefullyCrashRenderer();
        await new Promise((r) => setTimeout(r, 1500));
        let method = "forcefullyCrashRenderer";
        if (!wc.isCrashed()) {
          method = "SIGKILL";
          try {
            process.kill(pid, "SIGKILL");
          } catch {
            method = "SIGKILL-failed";
          }
        }
        record("qa-crash", { method });
        break;
      }
      // App-initiated player actions through the production WindowManager.
      case "appReloadPlayer": controller.windowManager.reloadPlayer(); break;
      case "appBlankPlayer": await controller.windowManager.blankPlayer(); break;
      case "appOpenPlayer": controller.windowManager.openPlayerWindow(); break;
      default: throw new Error(`unknown winop ${op}`);
    }
    return windowInfo(win);
  },
  async sendInput({ target, events: evs }) {
    const win = findWindow(target);
    if (!win) throw new Error(`no ${target} window`);
    for (const ev of evs) win.webContents.sendInputEvent(ev);
    return { sent: evs.length };
  },
  async metrics({ gpuInfo = "basic" } = {}) {
    return {
      appMetrics: app.getAppMetrics().map((m) => ({ pid: m.pid, type: m.type, serviceName: m.serviceName ?? null, sandboxed: m.sandboxed ?? null, integrityLevel: m.integrityLevel ?? null, cpuPercent: m.cpu?.percentCPUUsage ?? null })),
      gpuFeatureStatus: app.getGPUFeatureStatus(),
      gpuInfoBasic: await app.getGPUInfo(gpuInfo).catch((e) => ({ error: String(e) })),
      argv: process.argv.filter((a) => a.startsWith("--")),
      switches: {
        disableFeatures: app.commandLine.getSwitchValue("disable-features"),
        ozonePlatform: app.commandLine.getSwitchValue("ozone-platform"),
        ozonePlatformHint: app.commandLine.getSwitchValue("ozone-platform-hint"),
        enableWaylandIme: app.commandLine.hasSwitch("enable-wayland-ime"),
        forceDeviceScaleFactor: app.commandLine.getSwitchValue("force-device-scale-factor"),
      },
      isPackaged: app.isPackaged,
      userDataIsXdg: app.getPath("userData").endsWith("/ytmcp/electron-profile"),
      versions: process.versions,
    };
  },
  async sessions() {
    const ui = session.fromPartition("ytm-ui-ephemeral");
    const player = session.fromPartition("persist:player-default");
    // Only the UI session's cookie COUNT is read (it must be empty); the player session is never read.
    const uiCookieCount = (await ui.cookies.get({})).length;
    return {
      distinctObjects: ui !== player,
      uiPersistent: ui.isPersistent(),
      playerPersistent: player.isPersistent(),
      uiStoragePathSet: ui.getStoragePath() != null,
      playerStoragePathSet: player.getStoragePath() != null,
      uiCookieCount,
      webContents: webContents.getAllWebContents().map((wc) => ({ id: wc.id, origin: originOf(wc.getURL()), sessionIs: wc.session === ui ? "ui" : wc.session === player ? "player" : "other" })),
    };
  },
  async quit() {
    setTimeout(() => app.quit(), 50);
    return { quitting: true };
  },
};

function startControlServer() {
  if (existsSync(SOCKET)) unlinkSync(SOCKET);
  const server = createServer((sock) => {
    let buf = "";
    sock.setEncoding("utf8");
    sock.on("data", async (chunk) => {
      buf += chunk;
      let nl;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl);
        buf = buf.slice(nl + 1);
        let req;
        try {
          req = JSON.parse(line);
        } catch {
          continue;
        }
        const fn = ops[req.op];
        let reply;
        try {
          if (!fn) throw new Error(`unknown op ${req.op}`);
          reply = { id: req.id, ok: true, result: await fn(req.args ?? {}) };
        } catch (err) {
          reply = { id: req.id, ok: false, error: String(err?.message ?? err) };
        }
        if (!sock.destroyed) sock.write(`${JSON.stringify(reply)}\n`);
      }
    });
    sock.on("error", () => {});
  });
  server.listen(SOCKET, () => chmodSync(SOCKET, 0o600));
  app.on("will-quit", () => {
    server.close();
    try {
      unlinkSync(SOCKET);
    } catch {
      // gone
    }
  });
}

startControlServer();
record("qa-entry", { main: MAIN.split("/").slice(-3).join("/") });
process.env.YTMCP_NO_AUTOSTART = "1";
const mainModule = await import(pathToFileURL(MAIN).href);
delete process.env.YTMCP_NO_AUTOSTART;
if (typeof mainModule.startApp !== "function") {
  process.stderr.write("qa-entry: production main has no startApp export\n");
  app.exit(2);
} else {
  // Not awaited: Electron emits "ready" only after this entry module has finished evaluating.
  void mainModule.startApp().then((c) => {
    controller = c;
  });
}
