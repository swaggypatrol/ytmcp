import { randomUUID } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { BridgeLifecyclePayload } from "@ytmcp/protocol";
import type { BrowserWindow } from "electron";
import { BRIDGE_WORLD_ID, MUSIC_ORIGIN } from "../../preload/channels.js";
import { isBridgeOrigin } from "../windows/window-manager.js";

export interface BridgeBundleAssets {
  readonly pageShimSource: string;
  readonly bridgeProbeSource: string;
}

export function resolveBridgeDistDir(): string | null {
  const candidates = [
    process.env.YTMCP_BRIDGE_DIST_DIR,
    ...(typeof process.resourcesPath === "string"
      ? [resolve(process.resourcesPath, "bridge-dist")]
      : []),
    resolve(process.cwd(), "bridge/dist"),
    resolve(import.meta.dirname, "../../../../bridge/dist"),
    resolve(import.meta.dirname, "../../../bridge/dist"),
  ];
  for (const dir of candidates) {
    if (
      dir &&
      existsSync(resolve(dir, "page-shim.iife.js")) &&
      existsSync(resolve(dir, "bridge-probe.mjs"))
    ) {
      return dir;
    }
  }
  return null;
}

export function loadBridgeBundleAssets(bridgeDistDir?: string | null): BridgeBundleAssets | null {
  const dir = bridgeDistDir ?? resolveBridgeDistDir();
  if (!dir) {
    return null;
  }
  const pageShimPath = resolve(dir, "page-shim.iife.js");
  const bridgeProbePath = resolve(dir, "bridge-probe.mjs");
  if (!existsSync(pageShimPath) || !existsSync(bridgeProbePath)) {
    return null;
  }
  return {
    pageShimSource: readFileSync(pageShimPath, "utf8"),
    bridgeProbeSource: readFileSync(bridgeProbePath, "utf8"),
  };
}

/**
 * Wraps the self-contained ESM `bridge-probe.mjs` bundle into a top-frame IIFE
 * that calls `installBridge(window.ytmcpBridge, bridgeEpoch)` strictly when
 * `window.location.origin === "https://music.youtube.com"` and `window === window.top`.
 * The script runs in the bridge isolated world, where the preload exposed `ytmcpBridge`.
 */
export function buildBridgeInjectionScript(
  bridgeProbeSource: string,
  bridgeEpoch: string
): string {
  const stripped = bridgeProbeSource.replace(/\nexport\s*\{[\s\S]*?\};\s*$/, "\n");
  return `(() => {
  if (typeof window === "undefined" || window !== window.top || window.location.origin !== ${JSON.stringify(MUSIC_ORIGIN)}) {
    return { installed: false, reason: "UNTRUSTED_ORIGIN_OR_SUBFRAME" };
  }
  if (!window.ytmcpBridge) {
    return { installed: false, reason: "PRELOAD_BRIDGE_UNAVAILABLE" };
  }
${stripped}
  // Only plain data crosses back to the host; the handle's functions cannot be cloned.
  const handle = installBridge(window.ytmcpBridge, ${JSON.stringify(bridgeEpoch)});
  return { installed: handle.installed === true, reason: handle.reason ?? null };
})();`;
}

export function isTrustedPlayerMusicFrame(playerWin: BrowserWindow): boolean {
  if (playerWin.isDestroyed()) {
    return false;
  }
  const wc = playerWin.webContents;
  if (wc.isDestroyed()) {
    return false;
  }
  const url = wc.getURL();
  const mainFrame = wc.mainFrame;
  return Boolean(
    mainFrame &&
      mainFrame.parent === null &&
      mainFrame.origin === MUSIC_ORIGIN &&
      isBridgeOrigin(url)
  );
}

export async function injectBridgeIntoMusicPlayer(
  playerWin: BrowserWindow,
  assets: BridgeBundleAssets,
  onBridgeLifecycle?: (payload: BridgeLifecyclePayload) => void,
  log?: (level: "info" | "warn" | "error", event: string, detail?: Record<string, unknown>) => void
): Promise<string | null> {
  if (!isTrustedPlayerMusicFrame(playerWin)) {
    return null;
  }

  const bridgeEpoch = `bridge-${randomUUID()}`;
  onBridgeLifecycle?.({
    event: "successorAuthorized",
    bridgeEpoch,
    isMusicOrigin: true,
  });

  try {
    // The shim must reach the page's private player objects, so it alone runs in the main world.
    await playerWin.webContents.mainFrame.executeJavaScript(assets.pageShimSource, true);
    const probeScript = buildBridgeInjectionScript(assets.bridgeProbeSource, bridgeEpoch);
    const res = (await playerWin.webContents.executeJavaScriptInIsolatedWorld(
      BRIDGE_WORLD_ID,
      [{ code: probeScript }],
      true
    )) as {
      installed?: boolean;
      reason?: string;
    } | null;

    if (!res?.installed) {
      log?.("warn", "bridge_injection_skipped", {
        bridgeEpoch,
        reason: res?.reason ?? "unknown",
      });
      return null;
    }

    log?.("info", "bridge_injected", { bridgeEpoch });
    return bridgeEpoch;
  } catch (err) {
    log?.("warn", "bridge_injection_failed", {
      bridgeEpoch,
      message: err instanceof Error ? err.message : String(err),
    });
    return null;
  }
}

export function attachPlayerBridgeInjector(
  getPlayerWindow: () => BrowserWindow | null,
  onBridgeLifecycle?: (payload: BridgeLifecyclePayload) => void,
  log?: (level: "info" | "warn" | "error", event: string, detail?: Record<string, unknown>) => void
): { readonly attachToWindow: (playerWin: BrowserWindow) => void; readonly reinjectIfReady: () => Promise<void> } {
  const assets = loadBridgeBundleAssets();
  if (!assets) {
    log?.("warn", "bridge_dist_assets_missing", {});
  }
  const wiredWindows = new WeakSet<BrowserWindow>();

  const tryInject = async (win: BrowserWindow): Promise<void> => {
    if (!assets) {
      return;
    }
    await injectBridgeIntoMusicPlayer(win, assets, onBridgeLifecycle, log);
  };

  const attachToWindow = (playerWin: BrowserWindow): void => {
    if (wiredWindows.has(playerWin) || playerWin.isDestroyed()) {
      return;
    }
    wiredWindows.add(playerWin);

    playerWin.webContents.on("did-finish-load", () => {
      if (isTrustedPlayerMusicFrame(playerWin)) {
        void tryInject(playerWin);
      }
    });

    if (!playerWin.webContents.isLoadingMainFrame() && isTrustedPlayerMusicFrame(playerWin)) {
      void tryInject(playerWin);
    }
  };

  const reinjectIfReady = async (): Promise<void> => {
    const win = getPlayerWindow();
    if (win) {
      attachToWindow(win);
      if (!win.webContents.isLoadingMainFrame() && isTrustedPlayerMusicFrame(win)) {
        await tryInject(win);
      }
    }
  };

  return { attachToWindow, reinjectIfReady };
}
