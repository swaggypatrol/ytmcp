import { existsSync, lstatSync, mkdirSync, readFileSync, realpathSync } from "node:fs";
import { homedir } from "node:os";
import { extname, join, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";
import type {
  AppSettings,
  AuthStatusInfo,
  BridgeLifecyclePayload,
  Envelope,
  PlayerObservedState,
  SessionMaterialPayload,
  StateDelta,
} from "@ytm-glass/protocol";
import { app, ipcMain, net, protocol, type Session } from "electron";
import {
  IPC_CHANNELS,
  MUSIC_ORIGIN,
  type HostRuntimeInfo,
  type IpcAckResult,
} from "../../preload/channels.js";
import {
  SessionCredentialBroker,
  querySafeStorageStatus,
  type SafeStorageStatus,
} from "../session/credential-broker.js";
import { TrackNotifier } from "../notifications/track-notifier.js";
import {
  createAppTray,
  probeStatusNotifierHost,
  watchStatusNotifierHost,
  type AppTray,
} from "../tray/tray.js";
import { WindowManager } from "../windows/window-manager.js";
import { loadAppIdentity, type AppIdentity } from "./identity.js";
import { validateElectronIpcEvent } from "./ipc-guard.js";
import { createIntegratedDesktopHooks } from "../rust-host/desktop-hooks.js";

// Used by the G1 runtime probe, which bundles this module as its entry.
export { validateRendererIpc } from "./ipc-guard.js";

const ASSET_ID_RE = /^[a-zA-Z0-9_-]{1,128}$/;
const MAX_CACHED_COVER_RESOLUTIONS = 2_000;

const MIME_TYPES: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
  ".mjs": "application/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".webp": "image/webp",
  ".woff2": "font/woff2",
  ".woff": "font/woff",
  ".json": "application/json; charset=utf-8",
};

export interface CoverAssetResolution {
  readonly filePath: string;
  readonly cacheRoot: string;
  readonly mimeType?: string;
}

export interface DesktopHostHooks {
  readonly onUiEnvelope?: (envelope: Envelope) => void | Promise<void>;
  readonly onBridgeEnvelope?: (envelope: Envelope) => void | Promise<void>;
  readonly onBridgeLifecycle?: (payload: BridgeLifecyclePayload) => void;
  readonly onSessionMaterial?: (payload: SessionMaterialPayload) => void;
  readonly onSessionCleared?: (reason: string) => void;
  readonly onVisibilityChanged?: (state: {
    mainVisible: boolean;
    mainFocused: boolean;
    playerVisible: boolean;
  }) => void;
  readonly resolveCoverAsset?: (assetId: string) => Promise<CoverAssetResolution | null>;
  /** The tray icon appeared or disappeared after startup (StatusNotifier host came or went). */
  readonly onTrayAvailabilityChanged?: (available: boolean) => void;
  /** Awaited during quit, after the player window is gone (e.g. stop ytm-daemon). */
  readonly onQuit?: () => Promise<void>;
  readonly log?: (level: "info" | "warn" | "error", event: string, detail?: Record<string, unknown>) => void;
}

export interface DesktopHostController {
  readonly identity: AppIdentity;
  readonly mockMode: boolean;
  readonly windowManager: WindowManager;
  readonly credentialBroker: SessionCredentialBroker;
  readonly safeStorageStatus: SafeStorageStatus;
  /** Whether a tray icon is currently shown by a StatusNotifierItem host (live). */
  readonly trayAvailable: boolean;
  /** Delivers a core envelope to the main UI; the host also learns settings/playback from it. */
  deliverCoreEnvelope: (envelope: Envelope) => void;
  shutdown: () => void;
}

/**
 * Close-to-tray only when the user asked for it AND a tray icon is really visible;
 * otherwise closing the main window quits the whole app.
 */
export function resolveMainWindowClose(
  settings: Pick<AppSettings, "closeToTray"> | null,
  trayAvailable: boolean
): "hide" | "quit" {
  return settings?.closeToTray === true && trayAvailable ? "hide" : "quit";
}

function resolveDesktopDistDir(): string {
  const dir = import.meta.dirname;
  if (dir.endsWith(`${sep}dist`) || dir === "dist") {
    return dir;
  }
  const siblingDist = resolve(dir, "../../dist");
  if (existsSync(siblingDist)) {
    return siblingDist;
  }
  return resolve(process.cwd(), "apps/desktop/dist");
}

function resolveRepoRoot(): string {
  const candidates = [
    resolve(import.meta.dirname, "../../.."),
    resolve(import.meta.dirname, "../../../.."),
    process.cwd(),
  ];
  for (const candidate of candidates) {
    if (existsSync(resolve(candidate, "config/app-identity.json"))) {
      return candidate;
    }
  }
  return process.cwd();
}

function resolveUiDistDir(repoRoot: string): string | null {
  const override = process.env.YTM_GLASS_UI_DIST_DIR;
  const candidates = [
    ...(override ? [resolve(override)] : []),
    ...(typeof process.resourcesPath === "string"
      ? [resolve(process.resourcesPath, "ui-dist")]
      : []),
    resolve(repoRoot, "ui/dist"),
  ];
  for (const dir of candidates) {
    if (existsSync(resolve(dir, "index.html"))) {
      return dir;
    }
  }
  return null;
}

function resolveGlassProbeHtml(repoRoot: string): string | null {
  const candidate = resolve(repoRoot, "tools/probes/runtime/glass-test.html");
  return existsSync(candidate) ? candidate : null;
}

function isContainedInside(rootRealPath: string, candidatePath: string): string | null {
  try {
    const resolvedReal = realpathSync(candidatePath);
    const stat = lstatSync(resolvedReal);
    if (!stat.isFile()) {
      return null;
    }
    const prefix = rootRealPath.endsWith(sep) ? rootRealPath : `${rootRealPath}${sep}`;
    if (resolvedReal === rootRealPath || resolvedReal.startsWith(prefix)) {
      return resolvedReal;
    }
    return null;
  } catch {
    return null;
  }
}

/** Real path of a resolved cover file, only if it is a regular file inside the cover cache root. */
function containedCoverFile(resolved: CoverAssetResolution): string | null {
  try {
    return isContainedInside(realpathSync(resolved.cacheRoot), resolved.filePath);
  } catch {
    return null;
  }
}

function missingUiHtml(identity: AppIdentity): string {
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <title>${identity.displayName}</title>
  <style>
    body { margin: 0; font-family: system-ui, sans-serif; background: #0f1115; color: #f4f6fb; display: grid; place-items: center; min-height: 100vh; }
    main { max-width: 560px; padding: 24px; border-radius: 16px; background: #191d24; border: 1px solid rgba(255,255,255,0.1); }
    h1 { margin: 0 0 8px; font-size: 20px; }
    p { margin: 0; color: #b5bdd0; line-height: 1.5; }
  </style>
</head>
<body>
  <main>
    <h1>${identity.displayName} could not load its interface</h1>
    <p>The built interface files (ui/dist) were not found. Reinstall the app, or build the UI before starting it.</p>
  </main>
</body>
</html>`;
}

function buildCspHeader(appScheme: string): string {
  return [
    "default-src 'self'",
    "script-src 'self'",
    "style-src 'self' 'unsafe-inline'",
    `img-src 'self' ${appScheme}: data: blob:`,
    "font-src 'self' data:",
    "connect-src 'self'",
    "object-src 'none'",
    "base-uri 'none'",
    "frame-ancestors 'none'",
  ].join("; ");
}

function registerAppProtocolHandler(
  uiSession: Session,
  identity: AppIdentity,
  repoRoot: string,
  hooks: DesktopHostHooks
): void {
  // The G1 probe page is served only on explicit request in an unpackaged run.
  const probeHtml =
    !app.isPackaged && process.env.YTM_GLASS_PROBE_HTML === "1" ? resolveGlassProbeHtml(repoRoot) : null;
  const uiDistDir = probeHtml ? null : resolveUiDistDir(repoRoot);
  const uiDistReal = uiDistDir ? realpathSync(uiDistDir) : null;
  const cspHeader = buildCspHeader(identity.appScheme);
  const coverResolutions = new Map<string, Promise<CoverAssetResolution | null>>();

  const resolveCover = async (
    assetId: string
  ): Promise<{ filePath: string; mimeType?: string } | null> => {
    const resolveFn = hooks.resolveCoverAsset;
    if (!resolveFn) {
      return null;
    }
    for (let attempt = 0; attempt < 2; attempt += 1) {
      let pending = coverResolutions.get(assetId);
      const cached = pending !== undefined;
      if (!pending) {
        pending = resolveFn(assetId).catch(() => null);
        coverResolutions.set(assetId, pending);
        if (coverResolutions.size > MAX_CACHED_COVER_RESOLUTIONS) {
          const oldest = coverResolutions.keys().next().value;
          if (oldest !== undefined) {
            coverResolutions.delete(oldest);
          }
        }
      }
      const resolved = await pending;
      const safeFile = resolved ? containedCoverFile(resolved) : null;
      if (resolved && safeFile) {
        return { filePath: safeFile, mimeType: resolved.mimeType };
      }
      if (coverResolutions.get(assetId) === pending) {
        coverResolutions.delete(assetId);
      }
      if (!cached) {
        return null;
      }
      // A cached path may have been evicted from the cover cache since; ask core once more.
    }
    return null;
  };

  uiSession.protocol.handle(identity.appScheme, async (request) => {
    let url: URL;
    let cleanPath: string;
    try {
      url = new URL(request.url);
      cleanPath = decodeURIComponent(url.pathname).replace(/^\/+/, "");
    } catch {
      return new Response("Bad Request", { status: 400 });
    }

    const host = url.hostname.toLowerCase();
    if (host === "ui") {
      if (uiDistReal) {
        const relPath = cleanPath === "" ? "index.html" : cleanPath;
        const candidate = resolve(uiDistReal, relPath);
        const safeFile =
          isContainedInside(uiDistReal, candidate) ??
          (extname(relPath) === "" ? isContainedInside(uiDistReal, resolve(uiDistReal, "index.html")) : null);

        if (!safeFile) {
          return new Response("Not Found", { status: 404 });
        }

        const resp = await net.fetch(pathToFileURL(safeFile).toString());
        const headers = new Headers(resp.headers);
        const mime = MIME_TYPES[extname(safeFile).toLowerCase()];
        if (mime) {
          headers.set("Content-Type", mime);
        }
        headers.set("Content-Security-Policy", cspHeader);
        headers.set("X-Content-Type-Options", "nosniff");
        return new Response(resp.body, { status: resp.status, headers });
      }

      if (probeHtml) {
        const html = readFileSync(probeHtml, "utf8");
        return new Response(html, {
          status: 200,
          headers: {
            "Content-Type": "text/html; charset=utf-8",
            "Content-Security-Policy": cspHeader,
            "X-Content-Type-Options": "nosniff",
          },
        });
      }

      return new Response(missingUiHtml(identity), {
        status: 200,
        headers: {
          "Content-Type": "text/html; charset=utf-8",
          "Content-Security-Policy": cspHeader,
          "X-Content-Type-Options": "nosniff",
        },
      });
    }

    if (host === "covers") {
      const assetId = cleanPath;
      if (!ASSET_ID_RE.test(assetId)) {
        return new Response("Invalid asset ID", { status: 400 });
      }
      const cover = await resolveCover(assetId);
      if (!cover) {
        return new Response("Not Found", { status: 404 });
      }
      const resp = await net.fetch(pathToFileURL(cover.filePath).toString());
      const headers = new Headers(resp.headers);
      const mime = cover.mimeType ?? MIME_TYPES[extname(cover.filePath).toLowerCase()];
      if (mime) {
        headers.set("Content-Type", mime);
      }
      headers.set("X-Content-Type-Options", "nosniff");
      return new Response(resp.body, { status: resp.status, headers });
    }

    return new Response("Not Found", { status: 404 });
  });
}

/**
 * Dev-only UI server override: honoured only in an unpackaged run with YTM_GLASS_DEV=1,
 * and only for a loopback http(s) origin.
 */
function parseDevUiUrl(raw: string | undefined): { url: string; origin: string } | null {
  if (app.isPackaged || process.env.YTM_GLASS_DEV !== "1" || !raw || raw.trim() === "") {
    return null;
  }
  try {
    const parsed = new URL(raw);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      return null;
    }
    if (!["localhost", "127.0.0.1", "[::1]"].includes(parsed.hostname)) {
      return null;
    }
    return { url: parsed.toString(), origin: parsed.origin };
  } catch {
    return null;
  }
}

function registerPrivilegedAppScheme(identity: AppIdentity): void {
  protocol.registerSchemesAsPrivileged([
    {
      scheme: identity.appScheme,
      privileges: {
        standard: true,
        secure: true,
        supportFetchAPI: true,
        corsEnabled: true,
        stream: true,
      },
    },
  ]);
}

export async function startDesktopShell(
  hooks: DesktopHostHooks = {}
): Promise<DesktopHostController | null> {
  const identity = loadAppIdentity();
  const mockMode = process.env.YTM_GLASS_MODE === "mock";
  const launchIntent = process.argv.includes("--background") || process.argv.includes("--no-activate")
    ? "background"
    : "foreground";
  const devUi = parseDevUiUrl(process.env.YTM_GLASS_UI_URL);
  const log = hooks.log ?? ((level, event, detail) => {
    const line = JSON.stringify({
      ts: new Date().toISOString(),
      component: "desktop-host",
      level,
      event,
      ...(detail ? { detail } : {}),
    });
    process.stderr.write(`${line}\n`);
  });

  if (!app.isReady()) {
    if (app.commandLine && typeof app.commandLine.appendSwitch === "function") {
      const existingDisabled =
        typeof app.commandLine.getSwitchValue === "function"
          ? app.commandLine.getSwitchValue("disable-features")
          : "";
      const mergedDisabled = Array.from(
        new Set(
          existingDisabled
            .split(",")
            .map((s) => s.trim())
            .filter(Boolean)
            .concat(["HardwareMediaKeyHandling", "MediaSessionService"])
        )
      ).join(",");
      app.commandLine.appendSwitch("disable-features", mergedDisabled);
    }
    registerPrivilegedAppScheme(identity);
  }

  const hasExplicitUserDataDir = process.argv.some((arg) => arg.startsWith("--user-data-dir="));
  if (!hasExplicitUserDataDir) {
    const xdgDataHome = process.env.XDG_DATA_HOME?.trim() || join(homedir(), ".local/share");
    const profileDir = join(xdgDataHome, identity.xdgDirName, "electron-profile");
    mkdirSync(profileDir, { recursive: true, mode: 0o700 });
    app.setPath("userData", profileDir);
  }

  const gotSingleInstanceLock = app.requestSingleInstanceLock({ launchIntent });
  if (!gotSingleInstanceLock) {
    app.quit();
    return null;
  }

  // Electron can deliver a relaunch after ready while the tray probe is still pending.
  let showMainOnStart = launchIntent === "foreground";
  let activateMainWindow = (): void => { showMainOnStart = true; };
  app.on("second-instance", (_event, _argv, _directory, data) => {
    if (typeof data === "object" && data !== null && "launchIntent" in data && data.launchIntent === "background") {
      return;
    }
    log("info", "single_instance_activation_requested");
    activateMainWindow();
  });

  // Probe the tray host while Electron starts up. It is settled before any window or the
  // credential broker starts, so nothing is emitted before the caller attaches ytm-daemon.
  const trayProbe = probeStatusNotifierHost();

  app.setName(identity.displayName);
  if (typeof app.setDesktopName === "function") {
    app.setDesktopName(`${identity.desktopEntry}.desktop`);
  }

  await app.whenReady();

  const repoRoot = resolveRepoRoot();
  const distDir = resolveDesktopDistDir();
  const allowedUiOrigins = new Set<string>([`${identity.appScheme}://ui`]);
  if (devUi) {
    allowedUiOrigins.add(devUi.origin);
  }

  // Settings and playback live in the Rust core; the host only mirrors what it needs
  // (closeToTray, UI scale, track notifications, tray play/pause label) from the
  // snapshots/deltas it forwards to the UI.
  let coreSettings: AppSettings | null = null;
  let trayAvailable = false;
  let tray: AppTray | null = null;
  let trayPlaying = false;
  let stopTrayWatch: (() => void) | null = null;

  const windowManager = new WindowManager({
    identity,
    distDir,
    mockMode,
    devUiUrl: devUi?.url ?? null,
    onBridgeLifecycle: (payload) => {
      hooks.onBridgeLifecycle?.(payload);
    },
    onVisibilityChanged: (state) => {
      hooks.onVisibilityChanged?.(state);
    },
    onSecurityRejection: (detail) => {
      log("warn", "security_navigation_rejected", { ...detail });
    },
    onPlayerRendererGone: (reason) => {
      log("warn", "player_renderer_gone", { reason });
    },
    onMainWindowClose: () => resolveMainWindowClose(coreSettings, trayAvailable),
  });

  windowManager.hardenSessions();
  registerAppProtocolHandler(windowManager.getUiSession(), identity, repoRoot, hooks);

  const safeStorageStatus = querySafeStorageStatus();
  if (safeStorageStatus.basicTextWarning) {
    log("warn", "safe_storage_basic_text", {
      selectedBackend: safeStorageStatus.selectedBackend,
      secureStorageActive: safeStorageStatus.secureStorageActive,
    });
  }

  const credentialBroker = new SessionCredentialBroker({
    playerSession: windowManager.getPlayerSession(),
    onSessionMaterial: (payload) => {
      hooks.onSessionMaterial?.(payload);
    },
    onSessionCleared: (reason) => {
      hooks.onSessionCleared?.(reason);
    },
    onSummaryLog: (summary) => {
      log("info", "session_broker_update", summary as unknown as Record<string, unknown>);
    },
  });

  const trackNotifier = new TrackNotifier({
    isMainWindowFocused: () => windowManager.getMainWindow()?.isFocused() ?? false,
  });

  const observeSettings = (settings: AppSettings): void => {
    coreSettings = settings;
    windowManager.setMainUiScalePercent(settings.uiScalePercent);
    trackNotifier.setEnabled(settings.trackNotifications === true);
  };

  const observePlayer = (player: PlayerObservedState): void => {
    trayPlaying = player.playback === "playing" || player.playback === "buffering";
    tray?.setPlaying(trayPlaying);
    trackNotifier.observePlayer(player);
  };

  const deliverCoreEnvelope = (envelope: Envelope): void => {
    if (envelope.kind === "core.snapshot") {
      const snapshot = envelope.payload as {
        settings: AppSettings;
        player: PlayerObservedState;
        authState?: AuthStatusInfo;
      };
      observeSettings(snapshot.settings);
      trackNotifier.setAuthState(snapshot.authState?.state);
      observePlayer(snapshot.player);
    } else if (envelope.kind === "core.delta") {
      const delta = envelope.payload as StateDelta;
      if (delta.deltaType === "settingsChanged") {
        observeSettings(delta.settings);
      } else if (delta.deltaType === "playerChanged") {
        observePlayer(delta.player);
      } else if (delta.deltaType === "authChanged") {
        trackNotifier.setAuthState(delta.auth_state?.state);
      }
    }
    windowManager.sendToMainUi(envelope);
  };

  let pendingLogout: Promise<void> | null = null;
  /**
   * Real logout: stop credential forwarding, unload the old account's page (so it can
   * neither rewrite storage nor send bridge frames), let core drop auth, wipe the player
   * partition (cookies, local/IndexedDB/cache storage, HTTP cache), tell core the
   * session is gone, then send the player to the official sign-in entry.
   */
  const logOut = async (envelope: Envelope): Promise<void> => {
    credentialBroker.stop();
    await windowManager.blankPlayer();
    await hooks.onUiEnvelope?.(envelope);
    const playerSession = windowManager.getPlayerSession();
    try {
      await playerSession.clearStorageData();
      await playerSession.clearCache();
      await playerSession.clearAuthCache();
    } catch (err) {
      log("error", "logout_clear_storage_failed", {
        message: err instanceof Error ? err.message : String(err),
      });
    }
    credentialBroker.reset();
    hooks.onSessionCleared?.("logout");
    windowManager.loadPlayerSignIn();
    if (!mockMode) {
      credentialBroker.start();
    }
  };

  ipcMain.handle(IPC_CHANNELS.UI_HOST_INFO, (event): HostRuntimeInfo => {
    const mainWin = windowManager.getMainWindow();
    const expectedWc = mainWin?.webContents ?? null;
    const expectedFrame = expectedWc?.mainFrame ?? null;
    const frameOrigin = event.senderFrame?.origin ?? "";
    if (
      !expectedWc ||
      expectedWc.isDestroyed() ||
      event.sender !== expectedWc ||
      !event.senderFrame ||
      event.senderFrame !== expectedFrame ||
      !allowedUiOrigins.has(frameOrigin)
    ) {
      throw new Error("Rejected UI host info request from unauthorized sender");
    }
    return {
      appId: identity.appId,
      appScheme: identity.appScheme,
      displayName: identity.displayName,
      mockMode,
      protocolVersion: identity.protocolVersion,
      version: identity.version,
    };
  });

  ipcMain.handle(IPC_CHANNELS.UI_SEND, async (event, rawPayload: unknown): Promise<IpcAckResult> => {
    const result = validateElectronIpcEvent(
      event,
      {
        role: "mainUi",
        allowedOrigins: allowedUiOrigins,
        getWebContents: () => windowManager.getMainWindow()?.webContents ?? null,
      },
      rawPayload
    );

    if (!result.ok) {
      log("warn", "ipc_ui_rejected", {
        code: result.code,
        reason: result.reason,
      });
      return { ok: false, code: result.code, message: result.message };
    }

    if (result.envelope.kind === "ui.logout") {
      pendingLogout ??= logOut(result.envelope).finally(() => {
        pendingLogout = null;
      });
      await pendingLogout;
      return { ok: true };
    }

    if (result.envelope.kind === "ui.openPlayerWindow") {
      windowManager.openPlayerWindow();
    }

    await hooks.onUiEnvelope?.(result.envelope);
    return { ok: true };
  });

  ipcMain.handle(IPC_CHANNELS.BRIDGE_SEND, async (event, rawPayload: unknown): Promise<IpcAckResult> => {
    const result = validateElectronIpcEvent(
      event,
      {
        role: "playerBridge",
        allowedOrigins: new Set([MUSIC_ORIGIN]),
        getWebContents: () => windowManager.getPlayerWindow()?.webContents ?? null,
        isBridgeOriginActive: () => windowManager.isBridgeOriginActive(),
      },
      rawPayload
    );

    if (!result.ok) {
      log("warn", "ipc_bridge_rejected", {
        code: result.code,
        reason: result.reason,
      });
      return { ok: false, code: result.code, message: result.message };
    }

    await hooks.onBridgeEnvelope?.(result.envelope);
    return { ok: true };
  });

  const shutdown = (): void => {
    credentialBroker.stop();
    stopTrayWatch?.();
    stopTrayWatch = null;
    trackNotifier.dispose();
    tray?.destroy();
    tray = null;
    trayAvailable = false;
    windowManager.prepareForQuit();
  };

  // Quit is deferred once so the player window is destroyed (audio stops) and the
  // daemon is stopped before Electron exits; no orphan player or sidecar remains.
  let quitCleanup: "idle" | "running" | "done" = "idle";
  app.on("before-quit", (event) => {
    if (quitCleanup === "done") {
      return;
    }
    event.preventDefault();
    if (quitCleanup === "running") {
      return;
    }
    quitCleanup = "running";
    shutdown();
    void Promise.resolve(hooks.onQuit?.())
      .catch((err: unknown) => {
        log("error", "quit_cleanup_failed", {
          message: err instanceof Error ? err.message : String(err),
        });
      })
      .finally(() => {
        quitCleanup = "done";
        app.quit();
      });
  });
  app.on("window-all-closed", () => {
    app.quit();
  });

  /** Shows the tray for a registered host, or removes it; returns whether one is shown. */
  const syncTray = (hostOwner: string | null): boolean => {
    tray?.destroy();
    tray = null;
    if (hostOwner === null) {
      return false;
    }
    try {
      tray = createAppTray(identity.displayName, {
        toggleMainWindow: () => windowManager.toggleMainWindow(),
        playerCommand: (op) => {
          if (!windowManager.sendTrayCommand(op)) {
            log("warn", "tray_command_without_main_ui", { op });
          }
        },
        openPlayerWindow: () => windowManager.openPlayerWindow(),
        quit: () => app.quit(),
      });
      tray.setPlaying(trayPlaying);
      return true;
    } catch (err) {
      tray = null;
      log("warn", "tray_unavailable", { message: err instanceof Error ? err.message : String(err) });
      return false;
    }
  };

  const initialProbe = await trayProbe;
  const initialTrayOwner = initialProbe.kind === "available" ? initialProbe.owner : null;
  trayAvailable = syncTray(initialTrayOwner);
  if (initialTrayOwner === null) {
    log("info", "tray_unavailable", { reason: "no_status_notifier_host" });
  }
  // plasmashell may start after us or restart: follow the host so the tray reappears, and so
  // close-to-tray (which reads trayAvailable) never hides the window without a tray.
  stopTrayWatch = watchStatusNotifierHost(initialTrayOwner, (owner) => {
    if (quitCleanup !== "idle") {
      return;
    }
    const available = syncTray(owner);
    log("info", "tray_host_changed", { available });
    if (available !== trayAvailable) {
      trayAvailable = available;
      if (!available && windowManager.revealHiddenMainWindowWithoutFocus()) {
        // Hidden to a tray that is gone: make it reachable, but never take focus (fullscreen games).
        log("info", "main_window_revealed_tray_lost");
      }
      hooks.onTrayAvailabilityChanged?.(available);
    }
  });

  // No await between here and the return: the caller attaches ytm-daemon (status
  // "starting", early frames queued) before the broker's first cookie read resolves.
  windowManager.createMainWindow({ show: showMainOnStart });
  activateMainWindow = () => windowManager.showMainWindow();
  if (!mockMode) {
    const showPlayer = launchIntent === "foreground" && process.env.YTM_GLASS_SHOW_PLAYER === "1";
    windowManager.createPlayerWindow({ show: showPlayer, loadMusicUrl: true });
    credentialBroker.start();
  }

  return {
    identity,
    mockMode,
    windowManager,
    credentialBroker,
    safeStorageStatus,
    get trayAvailable() {
      return trayAvailable;
    },
    deliverCoreEnvelope,
    shutdown,
  };
}

function logStartupError(event: "startup_failed" | "startup_cleanup_failed", err: unknown): void {
  const msg = err instanceof Error ? err.message : String(err);
  process.stderr.write(
    `${JSON.stringify({ ts: new Date().toISOString(), component: "desktop-host", level: "error", event, msg })}\n`
  );
}

/**
 * The packaged app's startup: the desktop shell plus, outside UI-only mock mode, ytm-daemon.
 * Resolves with the shell controller (null when another instance owns the app); a failure is
 * logged before clean exit. Runs automatically unless YTM_GLASS_NO_AUTOSTART=1, which
 * lets an acceptance entry start the very same path and keep the controller.
 */
export async function startApp(): Promise<DesktopHostController | null> {
  let integrated: ReturnType<typeof createIntegratedDesktopHooks> | undefined;
  let controller: DesktopHostController | null = null;
  try {
    if (process.env.YTM_GLASS_MODE === "mock" && process.env.YTM_GLASS_WITH_DAEMON !== "1") {
      return await startDesktopShell();
    }
    integrated = createIntegratedDesktopHooks();
    controller = await startDesktopShell(integrated.hooks);
    if (controller) {
      await integrated.attachController(controller);
    }
    return controller;
  } catch (err: unknown) {
    logStartupError("startup_failed", err);
    try {
      // app.exit skips before-quit, so stop the player and sidecar first.
      controller?.shutdown();
      await integrated?.shutdown();
    } catch (cleanupError) {
      logStartupError("startup_cleanup_failed", cleanupError);
    } finally {
      app.exit(1);
    }
    return null;
  }
}

const isElectronMainEntry =
  typeof process.versions.electron === "string" &&
  process.env.YTM_GLASS_NO_AUTOSTART !== "1";

if (isElectronMainEntry) {
  void startApp();
}
