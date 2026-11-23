import { existsSync, realpathSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import type { OutputGateControlPayload, OutputGateResultPayload } from "@ytmcp/protocol";
import { app, net, powerMonitor } from "electron";
import type { CoverAssetResolution, DesktopHostController, DesktopHostHooks } from "../bootstrap/main.js";
import { attachPlayerBridgeInjector } from "./bridge-injector.js";
import { RustHostClient, type MprisUiAction, type RustHostClientOptions } from "./client.js";

export interface IntegratedDesktopHost {
  readonly rustHost: RustHostClient;
  readonly hooks: DesktopHostHooks;
  attachController(controller: DesktopHostController): Promise<void>;
  shutdown(): Promise<void>;
}

const COVER_RESOLVE_TIMEOUT_MS = 8_000;
/** Chromium's connectivity flag is cheap to read; Electron's main process has no event for it. */
const NETWORK_POLL_MS = 5_000;

function readGpuAccelerated(): boolean | null {
  const status = app.getGPUFeatureStatus();
  return typeof status?.gpu_compositing === "string" ? status.gpu_compositing.includes("enabled") : null;
}

export function resolveCoversCacheRoot(env: NodeJS.ProcessEnv = process.env): string {
  const customRoot = env.YTMCP_DATA_DIR?.trim();
  if (customRoot) {
    return resolve(customRoot, "cache/ytmcp/covers");
  }
  const xdgCache = env.XDG_CACHE_HOME?.trim() || join(homedir(), ".cache");
  return resolve(xdgCache, "ytmcp/covers");
}

export interface CreateIntegratedDesktopHooksOptions {
  readonly daemonBin?: string;
  readonly clientOptions?: Partial<RustHostClientOptions>;
  readonly onMprisUiNotification?: (action: MprisUiAction) => void;
}

export function createIntegratedDesktopHooks(
  options: CreateIntegratedDesktopHooksOptions = {}
): IntegratedDesktopHost {
  let controller: DesktopHostController | null = null;
  const activeGateTransactions = new Set<string>();
  let bridgeInjector: ReturnType<typeof attachPlayerBridgeInjector> | null = null;
  let removeSystemListeners: (() => void) | null = null;
  let networkPoll: NodeJS.Timeout | null = null;
  let shutdownPromise: Promise<void> | null = null;
  let online = true;
  /**
   * Tray availability last written into the cached hello (null before startup). The client
   * delivers that cache to the core: now, after the in-flight handshake, or on restart.
   */
  let announcedTray: boolean | null = null;

  const log = (
    level: "info" | "warn" | "error",
    event: string,
    detail?: Record<string, unknown>
  ): void => {
    const line = JSON.stringify({
      ts: new Date().toISOString(),
      component: "rust-host",
      level,
      event,
      ...(detail ? { detail } : {}),
    });
    process.stderr.write(`${line}\n`);
  };

  const pauseAndMutePlayerImmediately = async (): Promise<void> => {
    const playerWin = controller?.windowManager.getPlayerWindow();
    if (!playerWin || playerWin.isDestroyed()) {
      return;
    }
    try {
      playerWin.webContents.setAudioMuted(true);
    } catch {
      // Ignore if webContents already destroyed
    }
    try {
      await playerWin.webContents.mainFrame.executeJavaScript(
        `(() => {
          const media = document.querySelector("video, audio");
          if (media && typeof media.pause === "function") { media.pause(); }
        })();`,
        true
      );
    } catch {
      // If page cannot be paused cleanly, close/hide player window to guarantee zero orphan audio.
      try {
        playerWin.destroy();
      } catch {
        // Ignore
      }
    }
  };

  const handleOutputGateControl = async (
    payload: OutputGateControlPayload
  ): Promise<OutputGateResultPayload> => {
    const playerWin = controller?.windowManager.getPlayerWindow();
    if (!playerWin || playerWin.isDestroyed()) {
      return {
        transactionId: payload.transactionId,
        applied: false,
        error: "player window unavailable for output gate control",
      };
    }

    if (payload.gated) {
      activeGateTransactions.add(payload.transactionId);
      playerWin.webContents.setAudioMuted(true);
    } else {
      activeGateTransactions.delete(payload.transactionId);
      if (activeGateTransactions.size === 0) {
        playerWin.webContents.setAudioMuted(false);
      }
    }

    return {
      transactionId: payload.transactionId,
      applied: true,
      error: null,
    };
  };

  const rustHost = new RustHostClient({
    daemonBin: options.daemonBin,
    ...options.clientOptions,
    onUiEnvelope: (env) => {
      controller?.deliverCoreEnvelope(env);
      options.clientOptions?.onUiEnvelope?.(env);
    },
    onBridgeEnvelope: (env) => {
      const playerWin = controller?.windowManager.getPlayerWindow();
      if (playerWin) {
        bridgeInjector?.attachToWindow(playerWin);
      }
      controller?.windowManager.sendToPlayerBridge(env);
      options.clientOptions?.onBridgeEnvelope?.(env);
    },
    onOutputGateControl: handleOutputGateControl,
    onMprisAction: (action) => {
      options.onMprisUiNotification?.(action);
      options.clientOptions?.onMprisAction?.(action);
      if (action === "mpris:raise") {
        controller?.windowManager.showMainWindow();
      } else if (action === "mpris:quit") {
        app.quit();
      }
    },
    onUnexpectedExit: async (detail) => {
      activeGateTransactions.clear();
      await pauseAndMutePlayerImmediately();
      if (!detail.willRestart) {
        const playerWin = controller?.windowManager.getPlayerWindow();
        if (playerWin && !playerWin.isDestroyed()) {
          playerWin.destroy();
        }
      }
      await options.clientOptions?.onUnexpectedExit?.(detail);
    },
    onSessionMaterialError: (error) => {
      controller?.credentialBroker.handleDeliveryError(error);
      options.clientOptions?.onSessionMaterialError?.(error);
    },
    onControlledRestarted: async (ready) => {
      // The restarted core starts without credentials.
      void controller?.credentialBroker.resendCurrent();
      const playerWin = controller?.windowManager.getPlayerWindow();
      if (playerWin && !playerWin.isDestroyed()) {
        // Keep player paused and restore audio output only after paused state is ensured.
        await pauseAndMutePlayerImmediately();
        playerWin.webContents.setAudioMuted(false);
        await bridgeInjector?.reinjectIfReady();
      }
      await options.clientOptions?.onControlledRestarted?.(ready);
    },
    onLog: log,
  });

  const hooks: DesktopHostHooks = {
    onUiEnvelope: (envelope) => {
      if (envelope.kind === "ui.openPlayerWindow") {
        const playerWin = controller?.windowManager.getPlayerWindow();
        if (playerWin) {
          bridgeInjector?.attachToWindow(playerWin);
        }
      }
      rustHost.sendEnvelope(envelope);
    },
    onBridgeEnvelope: (envelope) => {
      rustHost.sendEnvelope(envelope);
    },
    onBridgeLifecycle: (payload) => {
      if (payload.isMusicOrigin) {
        const playerWin = controller?.windowManager.getPlayerWindow();
        if (playerWin) {
          bridgeInjector?.attachToWindow(playerWin);
        }
      }
      const accepted = rustHost.sendBridgeLifecycle(payload);
      if (payload.event === "crashed") {
        log(accepted ? "info" : "warn", "player_crash_lifecycle_forwarded", {
          accepted,
          daemonStatus: rustHost.getStatus(),
        });
      }
    },
    onSessionMaterial: (payload) => {
      // Forward directly from SessionCredentialBroker to ytm-daemon; NEVER log or forward to UI.
      rustHost.sendSessionMaterial(payload);
    },
    onSessionCleared: () => {
      rustHost.sendSessionCleared();
    },
    onVisibilityChanged: (state) => {
      rustHost.sendWindowVisibility(state);
    },
    onTrayAvailabilityChanged: () => {
      syncTray();
    },
    resolveCoverAsset: async (assetId): Promise<CoverAssetResolution | null> => {
      try {
        // Core may fetch the cover lazily on first request, so allow it a while.
        const resolved = await rustHost.resolveAsset(assetId, COVER_RESOLVE_TIMEOUT_MS);
        const cacheRoot = resolveCoversCacheRoot();
        if (!resolved.filePath || !existsSync(resolved.filePath) || !existsSync(cacheRoot)) {
          return null;
        }
        return {
          filePath: resolved.filePath,
          cacheRoot: realpathSync(cacheRoot),
          mimeType: resolved.mimeType ?? undefined,
        };
      } catch (err) {
        log("warn", "cover_asset_resolution_failed", {
          assetId,
          message: err instanceof Error ? err.message : String(err),
        });
        return null;
      }
    },
    onQuit: () => shutdown(),
    log,
  };

  /** Re-announces tray availability via a host-health hello; the cached hello follows it either way. */
  const syncTray = (): void => {
    if (shutdownPromise || !controller || announcedTray === null) return;
    const now = controller.trayAvailable;
    if (now !== announcedTray) {
      rustHost.updateHostHealth({ trayAvailable: now });
      announcedTray = now;
    }
  };

  const attachController = async (ctrl: DesktopHostController): Promise<void> => {
    controller = ctrl;
    bridgeInjector = attachPlayerBridgeInjector(
      () => controller?.windowManager.getPlayerWindow() ?? null,
      (payload) => rustHost.sendBridgeLifecycle(payload),
      log
    );

    const playerWin = ctrl.windowManager.getPlayerWindow();
    if (playerWin) {
      bridgeInjector.attachToWindow(playerWin);
    }

    online = net.isOnline();
    if (!networkPoll) {
      networkPoll = setInterval(() => {
        const now = net.isOnline();
        if (now === online) {
          return;
        }
        online = now;
        rustHost.sendNetworkChanged(now ? "online" : "offline");
        if (now) {
          controller?.credentialBroker.handleNetworkOnline();
        }
      }, NETWORK_POLL_MS);
      networkPoll.unref();
    }

    // The feature status read here can predate the GPU process; it is re-read whenever Chromium
    // reports new GPU info and re-announced once the core is up.
    let announcedGpu = readGpuAccelerated();
    const syncGpu = (): void => {
      if (shutdownPromise) return;
      const now = readGpuAccelerated();
      if (now !== announcedGpu) {
        // Before start() this only changes what the startup hello carries.
        rustHost.updateHostHealth({ gpuAccelerated: now });
        announcedGpu = now;
      }
    };
    if (!removeSystemListeners) {
      const onResume = (): void => {
        rustHost.sendPowerResume();
        // Waking up restarts the retry budget even if net.isOnline() missed the outage.
        controller?.credentialBroker.handleNetworkOnline();
      };
      powerMonitor.on("resume", onResume);
      app.on("gpu-info-update", syncGpu);
      removeSystemListeners = () => {
        powerMonitor.removeListener("resume", onResume);
        app.removeListener("gpu-info-update", syncGpu);
      };
      void app.getGPUInfo("basic").then(syncGpu, () => {});
    }
    const gpuAccelerated = announcedGpu;
    const trayAvailable = ctrl.trayAvailable;
    announcedTray = trayAvailable;

    await rustHost.start({
      appVersion: ctrl.identity.version,
      electronVersion: process.versions.electron ?? "unknown",
      chromiumVersion: process.versions.chrome ?? "unknown",
      hostHealth: {
        waylandNative: process.env.XDG_SESSION_TYPE === "wayland" ? true : null,
        gpuAccelerated,
        sandboxEnabled: !process.argv.includes("--no-sandbox"),
        secretStoreBackend: ctrl.safeStorageStatus.selectedBackend,
        safeStorageAvailable: ctrl.safeStorageStatus.secureStorageActive,
        trayAvailable,
        // ytm-daemon owns the MPRIS service and overwrites this with its real registration state.
        mprisActive: false,
        coreRestartCount: rustHost.getRestartCount(),
        lastError: null,
      },
      networkState: online ? "online" : "offline",
      mockMode: ctrl.mockMode,
    });

    syncGpu();
    syncTray();
    await bridgeInjector.reinjectIfReady();
  };

  const shutdown = (): Promise<void> => {
    activeGateTransactions.clear();
    removeSystemListeners?.();
    removeSystemListeners = null;
    if (networkPoll) {
      clearInterval(networkPoll);
      networkPoll = null;
    }
    return (shutdownPromise ??= rustHost.stop());
  };

  return {
    rustHost,
    hooks,
    attachController,
    shutdown,
  };
}
