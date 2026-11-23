import { resolve } from "node:path";
import type { BridgeLifecyclePayload, Envelope } from "@ytm-glass/protocol";
import {
  BrowserWindow,
  app,
  session,
  shell,
  type BrowserWindowConstructorOptions,
  type Session,
} from "electron";
import type { AppIdentity } from "../bootstrap/identity.js";
import { IPC_CHANNELS, MUSIC_ORIGIN, type TrayPlayerOp } from "../../preload/channels.js";

/**
 * Player-window navigation allowlist: host -> allowed path prefixes (`null` = any path).
 * Only music.youtube.com plus the normal Google sign-in / 2FA redirect chain:
 * accounts.google.com (sign-in, challenges) -> accounts.youtube.com/accounts/SetSID
 * (cross-domain cookie hand-off) -> www.youtube.com/signin?action_handle_signin=...
 * (post-login hand-off back to music.youtube.com). Consent and the post-login
 * recovery-info interstitial (gds.google.com/web/...) can appear inside that chain.
 */
export const PLAYER_NAVIGATION_RULES: ReadonlyMap<string, readonly string[] | null> = new Map<
  string,
  readonly string[] | null
>([
  ["music.youtube.com", null],
  ["accounts.google.com", null],
  ["accounts.youtube.com", ["/accounts/"]],
  ["www.youtube.com", ["/signin"]],
  ["www.google.com", ["/accounts/"]],
  ["consent.youtube.com", null],
  ["consent.google.com", null],
  ["gds.google.com", ["/web/"]],
]);

/**
 * Official YouTube Music sign-in entry (the page's own "Sign in" target): Google
 * ServiceLogin continuing through www.youtube.com/signin back to music.youtube.com.
 */
export const MUSIC_SIGN_IN_URL =
  "https://accounts.google.com/ServiceLogin?ltmpl=music&service=youtube&passive=true&continue=" +
  encodeURIComponent(
    `https://www.youtube.com/signin?action_handle_signin=true&app=desktop&next=${encodeURIComponent(`${MUSIC_ORIGIN}/`)}`
  );

/** How long an app-initiated player navigation may take to start before the bypass lapses. */
const APP_UNLOAD_BYPASS_MS = 10_000;

const ALLOWED_PLAYER_PERMISSIONS: ReadonlySet<string> = new Set([
  "mediaKeySystem",
]);

/**
 * Checks whether a URL is strictly allowed for navigation or auth popup handling
 * inside the isolated player window.
 */
export function isAllowedPlayerNavigationUrl(rawUrl: string): boolean {
  if (!rawUrl || typeof rawUrl !== "string") {
    return false;
  }
  let parsed: URL;
  try {
    parsed = new URL(rawUrl);
  } catch {
    return false;
  }

  if (parsed.protocol !== "https:") {
    return false;
  }
  if (parsed.username !== "" || parsed.password !== "") {
    return false;
  }
  if (parsed.port !== "") {
    return false;
  }
  const prefixes = PLAYER_NAVIGATION_RULES.get(parsed.hostname.toLowerCase());
  if (prefixes === undefined) {
    return false;
  }
  return prefixes === null || prefixes.some((prefix) => parsed.pathname.startsWith(prefix));
}

/**
 * URLs outside the player allowlist may only leave the app as plain https links
 * handed to the system browser (never file:, custom schemes, or credentials).
 */
export function isExternalOpenableUrl(rawUrl: string): boolean {
  try {
    const parsed = new URL(rawUrl);
    return parsed.protocol === "https:" && parsed.username === "" && parsed.password === "";
  } catch {
    return false;
  }
}

/**
 * Checks whether a URL belongs to the exact official YouTube Music origin.
 * Bridge IPC is NEVER enabled for any other origin (including Google auth pages).
 */
export function isBridgeOrigin(rawUrl: string): boolean {
  if (!rawUrl || typeof rawUrl !== "string") {
    return false;
  }
  try {
    return new URL(rawUrl).origin === MUSIC_ORIGIN;
  } catch {
    return false;
  }
}

/**
 * Checks whether a URL is allowed inside the main local UI window.
 */
export function isAllowedMainNavigationUrl(
  rawUrl: string,
  appScheme: string,
  devUiUrl?: string | null
): boolean {
  if (!rawUrl || typeof rawUrl !== "string") {
    return false;
  }
  let parsed: URL;
  try {
    parsed = new URL(rawUrl);
  } catch {
    return false;
  }

  if (parsed.protocol === `${appScheme}:` && parsed.hostname.toLowerCase() === "ui") {
    return true;
  }

  if (devUiUrl) {
    try {
      const devParsed = new URL(devUiUrl);
      if (
        (devParsed.protocol === "http:" || devParsed.protocol === "https:") &&
        parsed.origin === devParsed.origin
      ) {
        return true;
      }
    } catch {
      return false;
    }
  }

  return false;
}

export interface WindowManagerOptions {
  readonly identity: AppIdentity;
  readonly distDir: string;
  readonly mockMode: boolean;
  readonly devUiUrl?: string | null;
  readonly onBridgeLifecycle?: (payload: BridgeLifecyclePayload) => void;
  readonly onVisibilityChanged?: (state: {
    mainVisible: boolean;
    mainFocused: boolean;
    playerVisible: boolean;
  }) => void;
  readonly onSecurityRejection?: (detail: SecurityRejection) => void;
  /** The player renderer died (Electron's reason, e.g. "crashed", "oom"); diagnostics only. */
  readonly onPlayerRendererGone?: (reason: string) => void;
  /** Decides what closing the main window means; defaults to quitting the app. */
  readonly onMainWindowClose?: () => "hide" | "quit";
}

export interface SecurityRejection {
  readonly window: "main" | "player" | "popup";
  readonly reason: string;
  readonly origin: string;
}

export function buildMainWindowOptions(
  identity: AppIdentity,
  uiPreloadPath: string,
  show = true
): BrowserWindowConstructorOptions {
  return {
    width: 1280,
    height: 800,
    minWidth: 900,
    minHeight: 600,
    title: identity.displayName,
    frame: true,
    show,
    autoHideMenuBar: true,
    webPreferences: {
      partition: identity.uiSessionPartition,
      sandbox: true,
      contextIsolation: true,
      nodeIntegration: false,
      nodeIntegrationInWorker: false,
      nodeIntegrationInSubFrames: false,
      webSecurity: true,
      allowRunningInsecureContent: false,
      preload: uiPreloadPath,
    },
  };
}

export function buildPlayerWindowOptions(
  identity: AppIdentity,
  playerPreloadPath: string,
  show = false
): BrowserWindowConstructorOptions {
  return {
    width: 1120,
    height: 760,
    minWidth: 640,
    minHeight: 480,
    title: `${identity.displayName} — Official YouTube Music Player and Sign-in`,
    frame: true,
    show,
    paintWhenInitiallyHidden: false,
    autoHideMenuBar: true,
    webPreferences: {
      partition: identity.playerSessionPartition,
      sandbox: true,
      contextIsolation: true,
      nodeIntegration: false,
      nodeIntegrationInWorker: false,
      nodeIntegrationInSubFrames: false,
      webSecurity: true,
      allowRunningInsecureContent: false,
      autoplayPolicy: "no-user-gesture-required",
      preload: playerPreloadPath,
    },
  };
}

export class WindowManager {
  private readonly identity: AppIdentity;
  private readonly uiPreloadPath: string;
  private readonly playerPreloadPath: string;
  private readonly mockMode: boolean;
  private readonly devUiUrl: string | null;
  private readonly onBridgeLifecycle?: (payload: BridgeLifecyclePayload) => void;
  private readonly onVisibilityChanged?: (state: {
    mainVisible: boolean;
    mainFocused: boolean;
    playerVisible: boolean;
  }) => void;
  private readonly onSecurityRejection?: (detail: SecurityRejection) => void;
  private readonly onPlayerRendererGone?: (reason: string) => void;
  private readonly onMainWindowClose?: () => "hide" | "quit";

  private mainWindow: BrowserWindow | null = null;
  private playerWindow: BrowserWindow | null = null;
  private authPopups = new Set<BrowserWindow>();
  private bridgeOriginActive = false;
  /**
   * Deadline (ms epoch) of an app-initiated player navigation. Until it starts, the official
   * page's beforeunload handler (armed while music plays) must not cancel it: the player is
   * hidden, so the prompt could never be answered and the navigation would silently fail.
   */
  private appUnloadUntil = 0;
  private quitting = false;
  private permissionsHardened = false;
  /** Main UI zoom from the core's UI-scale setting; null until settings arrive. */
  private mainZoomFactor: number | null = null;

  public constructor(options: WindowManagerOptions) {
    this.identity = options.identity;
    this.uiPreloadPath = resolve(options.distDir, "ui-preload.cjs");
    this.playerPreloadPath = resolve(options.distDir, "player-preload.cjs");
    this.mockMode = options.mockMode;
    this.devUiUrl = options.devUiUrl ?? null;
    this.onBridgeLifecycle = options.onBridgeLifecycle;
    this.onVisibilityChanged = options.onVisibilityChanged;
    this.onSecurityRejection = options.onSecurityRejection;
    this.onPlayerRendererGone = options.onPlayerRendererGone;
    this.onMainWindowClose = options.onMainWindowClose;
  }

  public getUiSession(): Session {
    return session.fromPartition(this.identity.uiSessionPartition);
  }

  public getPlayerSession(): Session {
    return session.fromPartition(this.identity.playerSessionPartition);
  }

  public hardenSessions(): void {
    if (this.permissionsHardened) {
      return;
    }
    this.permissionsHardened = true;

    const uiSess = this.getUiSession();
    uiSess.setPermissionRequestHandler((_wc, _permission, callback) => {
      callback(false);
    });
    uiSess.setPermissionCheckHandler(() => false);

    const playerSess = this.getPlayerSession();
    playerSess.setPermissionRequestHandler((_wc, permission, callback, details) => {
      const origin = details.requestingUrl ? safeOrigin(details.requestingUrl) : "";
      const allowed = ALLOWED_PLAYER_PERMISSIONS.has(permission) && origin === MUSIC_ORIGIN;
      callback(allowed);
    });
    playerSess.setPermissionCheckHandler((_wc, permission, requestingOrigin) => {
      return (
        ALLOWED_PLAYER_PERMISSIONS.has(permission) &&
        safeOrigin(requestingOrigin) === MUSIC_ORIGIN
      );
    });
  }

  public createMainWindow(options?: { show?: boolean; initialUrl?: string }): BrowserWindow {
    this.hardenSessions();
    if (this.mainWindow && !this.mainWindow.isDestroyed()) {
      return this.mainWindow;
    }

    const win = new BrowserWindow(
      buildMainWindowOptions(this.identity, this.uiPreloadPath, options?.show ?? true)
    );
    this.mainWindow = win;

    win.webContents.setWindowOpenHandler(({ url }) => {
      this.onSecurityRejection?.({
        window: "main",
        reason: "window_open_denied",
        origin: safeOrigin(url),
      });
      return { action: "deny" };
    });

    const guardMainNavigation = (event: Electron.Event, url: string): void => {
      if (!isAllowedMainNavigationUrl(url, this.identity.appScheme, this.devUiUrl)) {
        event.preventDefault();
        this.onSecurityRejection?.({
          window: "main",
          reason: "navigation_denied",
          origin: safeOrigin(url),
        });
      }
    };

    win.webContents.on("will-navigate", guardMainNavigation);
    win.webContents.on("will-redirect", guardMainNavigation);
    // A zoom set before the first load (or lost across a reload) is applied once the page is up.
    win.webContents.on("did-finish-load", () => {
      this.applyMainZoom(win);
    });

    const notifyVisibility = (): void => {
      this.emitVisibility();
    };
    win.on("show", notifyVisibility);
    win.on("hide", notifyVisibility);
    win.on("focus", () => {
      win.flashFrame(false);
      notifyVisibility();
    });
    win.on("blur", notifyVisibility);
    win.on("close", (event) => {
      if (this.quitting) {
        return;
      }
      if (this.onMainWindowClose?.() === "hide") {
        event.preventDefault();
        win.hide();
        return;
      }
      // Without a usable tray the app must never linger invisibly: closing quits everything.
      app.quit();
    });
    win.on("closed", () => {
      if (this.mainWindow === win) {
        this.mainWindow = null;
      }
      this.emitVisibility();
    });

    const targetUrl =
      options?.initialUrl ??
      this.devUiUrl ??
      `${this.identity.appScheme}://ui/${this.mockMode ? "?mock=1" : ""}`;
    void win.loadURL(targetUrl);

    return win;
  }

  public createPlayerWindow(options?: { show?: boolean; loadMusicUrl?: boolean }): BrowserWindow {
    this.hardenSessions();
    if (this.playerWindow && !this.playerWindow.isDestroyed()) {
      return this.playerWindow;
    }

    const win = new BrowserWindow(
      buildPlayerWindowOptions(this.identity, this.playerPreloadPath, options?.show ?? false)
    );
    this.playerWindow = win;
    this.attachPlayerNavigationGuards(win, "player");

    win.webContents.on("did-navigate", (_event, url) => {
      this.updateBridgeOriginState(url, "did-navigate");
    });
    win.webContents.on("did-navigate-in-page", (_event, url, isMainFrame) => {
      if (isMainFrame) {
        this.updateBridgeOriginState(url, "did-navigate-in-page");
      }
    });
    win.webContents.on("render-process-gone", (_event, details) => {
      this.bridgeOriginActive = false;
      this.onPlayerRendererGone?.(details.reason);
      this.onBridgeLifecycle?.({ event: "crashed", isMusicOrigin: false });
    });
    win.webContents.on("will-prevent-unload", (event) => {
      if (Date.now() < this.appUnloadUntil) {
        event.preventDefault();
      }
    });
    win.webContents.on("did-start-navigation", (details) => {
      if (details.isMainFrame && !details.isSameDocument) {
        this.appUnloadUntil = 0;
      }
    });

    win.on("close", (event) => {
      if (!this.quitting) {
        event.preventDefault();
        win.hide();
        this.emitVisibility();
      }
    });
    win.on("show", () => this.emitVisibility());
    win.on("hide", () => this.emitVisibility());
    win.on("closed", () => {
      this.bridgeOriginActive = false;
      if (this.playerWindow === win) {
        this.playerWindow = null;
      }
      this.emitVisibility();
    });

    const shouldLoadMusic = options?.loadMusicUrl ?? !this.mockMode;
    if (shouldLoadMusic) {
      void win.loadURL(MUSIC_ORIGIN);
    } else {
      void win.loadURL("about:blank");
    }

    return win;
  }

  private attachPlayerNavigationGuards(
    win: BrowserWindow,
    kind: "player" | "popup"
  ): void {
    win.webContents.setWindowOpenHandler(({ url }) => {
      // An unattended page must not activate the desktop with a login popup.
      if (!win.isVisible() || win.isMinimized() || !win.isFocused()) {
        return { action: "deny" };
      }
      if (!isAllowedPlayerNavigationUrl(url)) {
        this.rejectPlayerNavigation(win, kind, "window_open_denied", url, true);
        return { action: "deny" };
      }

      // Auth popups (e.g. Google 2FA) run in the isolated player partition with
      // sandbox + contextIsolation and WITHOUT any preload script.
      return {
        action: "allow",
        overrideBrowserWindowOptions: {
          width: 540,
          height: 700,
          autoHideMenuBar: true,
          webPreferences: {
            partition: this.identity.playerSessionPartition,
            sandbox: true,
            contextIsolation: true,
            nodeIntegration: false,
            nodeIntegrationInWorker: false,
            nodeIntegrationInSubFrames: false,
            webSecurity: true,
            allowRunningInsecureContent: false,
            preload: undefined,
          },
        },
      };
    });

    win.webContents.on("did-create-window", (childWindow) => {
      this.authPopups.add(childWindow);
      this.attachPlayerNavigationGuards(childWindow, "popup");
      childWindow.on("closed", () => {
        this.authPopups.delete(childWindow);
      });
    });

    // Top-level navigations and redirects never leave the app: will-navigate also fires for
    // script-initiated navigations, which carry no user-gesture flag in Electron.
    const guardNav = (event: Electron.Event, url: string): void => {
      if (url === "about:blank" && this.mockMode) {
        return;
      }
      if (!isAllowedPlayerNavigationUrl(url)) {
        event.preventDefault();
        this.rejectPlayerNavigation(win, kind, "navigation_denied", url, false);
      }
    };

    win.webContents.on("will-navigate", guardNav);
    win.webContents.on("will-redirect", guardNav);
  }

  /**
   * Denies a non-allowlisted player/popup navigation. Only a new-window request (a link
   * opened in a new tab/window) for a plain https URL may go to the system browser, and
   * only while the user is looking at that window (visible and focused); the hidden
   * player can never open anything externally.
   */
  private rejectPlayerNavigation(
    win: BrowserWindow,
    kind: "player" | "popup",
    reason: string,
    url: string,
    mayOpenExternally: boolean
  ): void {
    const userFacing = !win.isDestroyed() && win.isVisible() && win.isFocused();
    const openExternally = mayOpenExternally && userFacing && isExternalOpenableUrl(url);
    this.onSecurityRejection?.({
      window: kind,
      reason: openExternally ? `${reason}_opened_externally` : reason,
      origin: safeOrigin(url),
    });
    if (openExternally) {
      void shell.openExternal(url).catch(() => {});
    }
  }

  private updateBridgeOriginState(url: string, lifecycleEvent: string): void {
    const active = isBridgeOrigin(url);
    this.bridgeOriginActive = active;
    this.onBridgeLifecycle?.({
      event: lifecycleEvent,
      isMusicOrigin: active,
    });
  }

  private emitVisibility(): void {
    this.onVisibilityChanged?.(this.getVisibilityState());
  }

  public getVisibilityState(): {
    mainVisible: boolean;
    mainFocused: boolean;
    playerVisible: boolean;
  } {
    const main = this.mainWindow && !this.mainWindow.isDestroyed() ? this.mainWindow : null;
    const player =
      this.playerWindow && !this.playerWindow.isDestroyed() ? this.playerWindow : null;
    return {
      mainVisible: main ? main.isVisible() && !main.isMinimized() : false,
      mainFocused: main ? main.isFocused() : false,
      playerVisible: player ? player.isVisible() && !player.isMinimized() : false,
    };
  }

  /**
   * Shows a main window that is hidden (not minimized) without focusing or raising it.
   * Returns whether it was shown.
   */
  public revealHiddenMainWindowWithoutFocus(): boolean {
    const main = this.getMainWindow();
    if (!main || main.isVisible() || main.isMinimized()) {
      return false;
    }
    main.showInactive();
    return true;
  }

  /**
   * Zooms the whole main UI (layout, not just text) to the core's UI scale, clamped to
   * 75%-150%. Kept for windows created or reloaded later.
   */
  public setMainUiScalePercent(percent: number): void {
    const factor = Number.isFinite(percent) ? Math.min(1.5, Math.max(0.75, percent / 100)) : 1;
    if (factor === this.mainZoomFactor) {
      return;
    }
    this.mainZoomFactor = factor;
    const main = this.getMainWindow();
    if (main) {
      this.applyMainZoom(main);
    }
  }

  private applyMainZoom(win: BrowserWindow): void {
    if (this.mainZoomFactor !== null && !win.isDestroyed() && !win.webContents.isDestroyed()) {
      win.webContents.setZoomFactor(this.mainZoomFactor);
    }
  }

  public getMainWindow(): BrowserWindow | null {
    return this.mainWindow && !this.mainWindow.isDestroyed() ? this.mainWindow : null;
  }

  public getPlayerWindow(): BrowserWindow | null {
    return this.playerWindow && !this.playerWindow.isDestroyed() ? this.playerWindow : null;
  }

  public isBridgeOriginActive(): boolean {
    const player = this.getPlayerWindow();
    if (!player) {
      return false;
    }
    return this.bridgeOriginActive && isBridgeOrigin(player.webContents.getURL());
  }

  /**
   * Shows the main window, recreating it if it no longer exists. When the Wayland
   * compositor refuses to move focus, the window requests attention instead.
   */
  public showMainWindow(): void {
    if (this.quitting) {
      return;
    }
    const main = this.getMainWindow() ?? this.createMainWindow({ show: true });
    if (main.isMinimized()) {
      main.restore();
    }
    if (!main.isVisible()) {
      main.show();
    }
    main.focus();
    if (!main.isFocused()) {
      main.flashFrame(true);
    }
  }

  /**
   * Tray click: hide the main window when it is shown, otherwise bring it back. Focus is
   * not consulted because clicking the tray icon itself moves focus to the panel.
   */
  public toggleMainWindow(): void {
    const main = this.getMainWindow();
    if (main && main.isVisible() && !main.isMinimized()) {
      main.hide();
      return;
    }
    this.showMainWindow();
  }

  /** Hands a tray playback action to the main UI renderer, which issues it as a normal UI command. */
  public sendTrayCommand(op: TrayPlayerOp): boolean {
    const main = this.getMainWindow();
    if (!main) {
      return false;
    }
    main.webContents.send(IPC_CHANNELS.UI_TRAY_COMMAND, op);
    return true;
  }

  /**
   * Unloads the player page (logout): the old account's page stops running and writing
   * storage, and the bridge origin becomes inactive so no more bridge frames are accepted.
   */
  public async blankPlayer(): Promise<void> {
    const player = this.getPlayerWindow();
    if (!player) {
      return;
    }
    this.bridgeOriginActive = false;
    this.allowAppUnload();
    await player.loadURL("about:blank").catch(() => {});
  }

  /** Points the player at the official sign-in entry (used after logout). */
  public loadPlayerSignIn(): void {
    const player = this.getPlayerWindow();
    if (player) {
      this.allowAppUnload();
      void player.loadURL(MUSIC_SIGN_IN_URL);
    }
  }

  /** Reloads the player page even while it plays (recovery); the bridge re-injects on load. */
  public reloadPlayer(): void {
    const player = this.getPlayerWindow();
    if (player) {
      this.allowAppUnload();
      player.webContents.reload();
    }
  }

  private allowAppUnload(): void {
    this.appUnloadUntil = Date.now() + APP_UNLOAD_BYPASS_MS;
  }

  /** Shows the official player page; a crashed player renderer is reloaded first. */
  public openPlayerWindow(): void {
    const player = this.createPlayerWindow({ show: true, loadMusicUrl: !this.mockMode });
    if (!this.mockMode) {
      const current = player.webContents.getURL();
      if (player.webContents.isCrashed()) {
        this.reloadPlayer();
      } else if (!current || current === "about:blank") {
        void player.loadURL(MUSIC_ORIGIN);
      }
    }
    if (player.isMinimized()) {
      player.restore();
    }
    player.show();
    player.focus();
  }

  public hidePlayerWindow(): void {
    const player = this.getPlayerWindow();
    if (player && player.isVisible()) {
      player.hide();
    }
  }

  public sendToMainUi(envelope: Envelope): boolean {
    const main = this.getMainWindow();
    if (!main) {
      return false;
    }
    main.webContents.send(IPC_CHANNELS.UI_EVENT, envelope);
    return true;
  }

  public sendToPlayerBridge(envelope: Envelope): boolean {
    if (!this.isBridgeOriginActive()) {
      return false;
    }
    const player = this.getPlayerWindow();
    if (!player) {
      return false;
    }
    player.webContents.send(IPC_CHANNELS.BRIDGE_EVENT, envelope);
    return true;
  }

  public prepareForQuit(): void {
    this.quitting = true;
    for (const popup of this.authPopups) {
      if (!popup.isDestroyed()) {
        popup.destroy();
      }
    }
    this.authPopups.clear();

    const player = this.getPlayerWindow();
    if (player) {
      player.destroy();
      this.playerWindow = null;
    }
    this.bridgeOriginActive = false;
  }
}

function safeOrigin(rawUrl: string): string {
  try {
    return new URL(rawUrl).origin;
  } catch {
    return "invalid-url";
  }
}
