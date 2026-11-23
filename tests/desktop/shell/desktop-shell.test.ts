import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, sep } from "node:path";
import { PROTOCOL_VERSION, type AppSettings, type Envelope } from "@ytm-glass/protocol";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const trayHost = vi.hoisted(() => ({ registered: false, owner: ":1.42", delayMs: 0, failing: false }));

// The host resolves `electron` from apps/desktop/node_modules; mock that exact module.
vi.mock("../../../apps/desktop/node_modules/electron/index.js", async () => (await import("./fake-electron.js")).fake.electron);
vi.mock("node:child_process", async (importOriginal) => {
  const original = await importOriginal<typeof import("node:child_process")>();
  return {
    ...original,
    // Answers the StatusNotifierWatcher probe (`gdbus call ... GetNameOwner` and
    // `... IsStatusNotifierHostRegistered`).
    execFile: (_cmd: string, args: string[], _opts: unknown, cb: (err: Error | null, out: string) => void) => {
      const answer = (): void => {
        if (trayHost.failing) {
          // gdbus killed by the probe timeout (e.g. under heavy load): says nothing about the host.
          cb(Object.assign(new Error("Command failed: gdbus call"), { killed: true, signal: "SIGTERM" }), "");
        } else if (trayHost.registered) {
          cb(null, args.includes("org.freedesktop.DBus.GetNameOwner") ? `('${trayHost.owner}',)\n` : "(<true>,)\n");
        } else {
          cb(new Error("GDBus.Error:org.freedesktop.DBus.Error.NameHasNoOwner: The name does not have an owner"), "");
        }
      };
      if (trayHost.delayMs > 0) {
        setTimeout(answer, trayHost.delayMs);
      } else {
        answer();
      }
    },
  };
});
// Hide any built ui/dist so the missing-UI behaviour is tested regardless of checkout state.
vi.mock("node:fs", async (importOriginal) => {
  const original = await importOriginal<typeof import("node:fs")>();
  const hidden = (p: unknown): boolean => String(p).endsWith(`${sep}ui${sep}dist${sep}index.html`);
  return { ...original, existsSync: (p: Parameters<typeof original.existsSync>[0]) => !hidden(p) && original.existsSync(p) };
});

import { fake, FakeBrowserWindow, FakeNotification, FakeTray, makeEvent } from "./fake-electron.js";
import { IPC_CHANNELS } from "../../../apps/desktop/preload/channels.js";
import {
  startApp,
  startDesktopShell,
  type DesktopHostHooks,
} from "../../../apps/desktop/src/bootstrap/main.js";
import { MUSIC_SIGN_IN_URL } from "../../../apps/desktop/src/windows/window-manager.js";

const SETTINGS: AppSettings = {
  autoplayOnLaunch: false,
  closeToTray: true,
  lrclibEnabled: true,
  nativeWindowDecorations: true,
  reduceMotion: false,
  reduceTransparency: false,
  theme: "dark",
  trackNotifications: false,
  uiScalePercent: 100,
};

function coreEnvelope(kind: "core.snapshot" | "core.delta", payload: unknown): Envelope {
  return { protocolVersion: PROTOCOL_VERSION, kind, coreEpoch: "core-1", seq: 1, payload };
}

function snapshotWith(settings: AppSettings, playback = "paused"): Envelope {
  return coreEnvelope("core.snapshot", { settings, player: { playback } });
}

function mainWindow(): FakeBrowserWindow {
  const win = FakeBrowserWindow.instances.find(
    (w) => !w.destroyed && w.options.webPreferences?.preload?.endsWith("ui-preload.cjs")
  );
  if (!win) throw new Error("main window missing");
  return win;
}

function playerWindow(): FakeBrowserWindow {
  const win = FakeBrowserWindow.instances.find(
    (w) => !w.destroyed && w.options.webPreferences?.preload?.endsWith("player-preload.cjs")
  );
  if (!win) throw new Error("player window missing");
  return win;
}

/** Invokes the UI_SEND handler as the genuine main UI top frame would. */
async function sendFromMainUi(envelope: Envelope): Promise<unknown> {
  const handler = fake.ipcHandlers.get(IPC_CHANNELS.UI_SEND)!;
  const wc = mainWindow().webContents;
  return handler({ sender: wc, senderFrame: wc.mainFrame }, envelope);
}

let tempRoot = "";
const originalArgv = process.argv;

beforeEach(() => {
  fake.reset();
  trayHost.registered = false;
  trayHost.owner = ":1.42";
  trayHost.delayMs = 0;
  trayHost.failing = false;
  tempRoot = mkdtempSync(join(tmpdir(), "ytm-desktop-shell-"));
  vi.stubEnv("XDG_DATA_HOME", join(tempRoot, "data"));
  vi.stubEnv("YTM_GLASS_MODE", "");
  vi.stubEnv("YTM_GLASS_UI_DIST_DIR", join(tempRoot, "no-ui"));
});

afterEach(() => {
  vi.useRealTimers();
  process.argv = originalArgv;
  vi.unstubAllEnvs();
  rmSync(tempRoot, { recursive: true, force: true });
});

describe("main window close policy", () => {
  it("quits the whole app on close when no tray host is registered, even with closeToTray on", async () => {
    const controller = (await startDesktopShell())!;
    expect(controller.trayAvailable).toBe(false);
    expect(FakeTray.instances).toHaveLength(0);
    controller.deliverCoreEnvelope(snapshotWith({ ...SETTINGS, closeToTray: true }));

    const closeEvent = mainWindow().close();

    expect(closeEvent.defaultPrevented).toBe(false);
    expect(fake.electron.app.quit).toHaveBeenCalledTimes(1);
  });

  it("hides to tray only while core settings say closeToTray and a tray is shown", async () => {
    trayHost.registered = true;
    const controller = (await startDesktopShell())!;
    expect(controller.trayAvailable).toBe(true);
    const main = mainWindow();

    // Settings not yet known from core: never hide.
    main.emit("close", makeEvent());
    expect(fake.electron.app.quit).toHaveBeenCalledTimes(1);

    controller.deliverCoreEnvelope(snapshotWith({ ...SETTINGS, closeToTray: true }));
    const hidden = makeEvent();
    main.emit("close", hidden);
    expect(hidden.defaultPrevented).toBe(true);
    expect(main.isVisible()).toBe(false);
    expect(fake.electron.app.quit).toHaveBeenCalledTimes(1);

    controller.deliverCoreEnvelope(
      coreEnvelope("core.delta", {
        deltaType: "settingsChanged",
        revision: 2,
        settings: { ...SETTINGS, closeToTray: false },
      })
    );
    const closing = makeEvent();
    main.emit("close", closing);
    expect(closing.defaultPrevented).toBe(false);
    expect(fake.electron.app.quit).toHaveBeenCalledTimes(2);
  });

  it("quits by destroying the player first, then awaiting core shutdown, then exiting", async () => {
    const order: string[] = [];
    let player: FakeBrowserWindow | null = null;
    await startDesktopShell({
      onQuit: async () => {
        order.push(player?.destroyed ? "onQuit:after-player" : "onQuit:player-alive");
      },
    });
    player = playerWindow();
    player.on("closed", () => order.push("player-destroyed"));
    fake.electron.app.quit.mockImplementation(() => order.push("app.quit"));

    const first = makeEvent();
    fake.appEvents.emit("before-quit", first);
    expect(first.defaultPrevented).toBe(true);
    await vi.waitFor(() => expect(order).toContain("app.quit"));
    expect(order).toEqual(["player-destroyed", "onQuit:after-player", "app.quit"]);

    // The re-issued quit passes through.
    const second = makeEvent();
    fake.appEvents.emit("before-quit", second);
    expect(second.defaultPrevented).toBe(false);
  });
});

describe("startup ordering", () => {
  it("emits no session material before startDesktopShell returns, even with a slow tray probe", async () => {
    trayHost.delayMs = 30;
    // Signed-in cookies arrive on the next task (a real cookies.get is a network-service round trip),
    // long before the slow probe finishes.
    const signedIn = [{ name: "SAPISID", value: "synthetic", domain: ".youtube.com" }];
    fake.electron.session
      .fromPartition("persist:player-default")
      .cookies.get.mockImplementation(() => new Promise((r) => setTimeout(() => r(signedIn), 0)));
    const events: string[] = [];
    let attached = false;
    const controller = (await startDesktopShell({
      onSessionMaterial: () => events.push(attached ? "material:after-attach" : "material:before-attach"),
    }))!;
    attached = true; // what runMain's attachController does right after the await
    await vi.waitFor(() => expect(events).toHaveLength(1));
    expect(events).toEqual(["material:after-attach"]);
    expect(controller.credentialBroker.getCredentialRevision()).toBe("cred-1");
  });
});

describe("single instance", () => {
  it("starts both windows hidden without a tray and ignores background relaunches", async () => {
    process.argv = [process.execPath, "--background", "--no-activate"];
    vi.stubEnv("YTM_GLASS_SHOW_PLAYER", "1");
    const log = vi.fn();
    const controller = (await startDesktopShell({ log }))!;
    const main = mainWindow();
    const player = playerWindow();

    expect(fake.electron.app.requestSingleInstanceLock).toHaveBeenCalledWith({ launchIntent: "background" });
    expect(controller.trayAvailable).toBe(false);
    expect(main.options.show).toBe(false);
    expect(player.options.show).toBe(false);
    expect(player.loadURL).toHaveBeenCalledWith("https://music.youtube.com");

    // Electron may append or reorder the arguments. The sender's launch intent is explicit.
    fake.appEvents.emit("second-instance", {}, [], "/", { launchIntent: "background" });

    expect(FakeBrowserWindow.instances).toEqual([main, player]);
    expect(main.isVisible()).toBe(false);
    expect(main.isFocused()).toBe(false);
    expect(main.flashFrame).not.toHaveBeenCalled();
    expect(fake.electron.app.quit).not.toHaveBeenCalled();

    fake.appEvents.emit("second-instance", {}, [], "/", { launchIntent: "foreground" });

    expect(main.isVisible()).toBe(true);
    expect(main.isFocused()).toBe(true);
    expect(player.isVisible()).toBe(false);

    main.destroy();
    fake.appEvents.emit("second-instance", {}, [], "/", { launchIntent: "background" });
    expect(() => mainWindow()).toThrow("main window missing");
  });

  it("preserves a foreground activation arriving while a background launch probes the tray", async () => {
    process.argv = [process.execPath, "--no-activate"];
    trayHost.registered = true;
    trayHost.delayMs = 20;
    const startup = startDesktopShell();
    expect(FakeBrowserWindow.instances).toHaveLength(0);
    fake.appEvents.emit("second-instance", {}, [], "/", { launchIntent: "foreground" });
    await startup;

    const main = mainWindow();
    expect(main.isVisible()).toBe(true);
    expect(playerWindow().isVisible()).toBe(false);

    const tray = FakeTray.instances[0]!;
    tray.click("Show / Hide Main Window");
    expect(main.isVisible()).toBe(false);
    fake.appEvents.emit("second-instance", {}, [], "/", { launchIntent: "background" });
    expect(main.isVisible()).toBe(false);
    tray.click("Show / Hide Main Window");
    expect(main.isVisible()).toBe(true);
  });

  it("denies login popups from a hidden player until the user opens it", async () => {
    process.argv = [process.execPath, "--background"];
    const controller = (await startDesktopShell())!;
    const handler = playerWindow().webContents.setWindowOpenHandler.mock.calls[0]![0]!;
    const request = { url: "https://accounts.google.com/signin" };

    expect(handler(request)).toMatchObject({ action: "deny" });
    expect(fake.electron.shell.openExternal).not.toHaveBeenCalled();

    controller.windowManager.openPlayerWindow();
    expect(handler(request)).toMatchObject({ action: "allow" });
  });

  it("requests attention on the existing window when the compositor refuses focus", async () => {
    const log = vi.fn();
    await startDesktopShell({ log });
    const original = mainWindow();
    original.focusAllowed = false;

    fake.appEvents.emit("second-instance", {}, [], "/");

    expect(mainWindow()).toBe(original);
    expect(original.isFocused()).toBe(false);
    expect(original.flashFrame).toHaveBeenCalledWith(true);
    expect(log).toHaveBeenCalledWith("info", "single_instance_activation_requested");
  });

  it("recreates and shows the main window when a second instance starts after it was destroyed", async () => {
    await startDesktopShell();
    const original = mainWindow();
    original.destroy();

    fake.appEvents.emit("second-instance", {}, [], "/");

    const recreated = mainWindow();
    expect(recreated).not.toBe(original);
    expect(recreated.isVisible()).toBe(true);
    expect(recreated.isFocused()).toBe(true);
    expect(recreated.loadURL).toHaveBeenCalledWith("app://ui/");
  });

  it("does not recreate the main window when a second instance starts during quit cleanup", async () => {
    let finishQuit: () => void = () => {};
    await startDesktopShell({ onQuit: () => new Promise<void>((r) => (finishQuit = r)) });
    fake.appEvents.emit("before-quit", makeEvent());
    mainWindow().destroy();

    fake.appEvents.emit("second-instance", {}, [], "/");

    expect(() => mainWindow()).toThrow("main window missing");
    finishQuit();
  });
});

describe("logout", () => {
  it("shares a pending logout and accepts a new logout after it completes", async () => {
    const onUiEnvelope = vi.fn();
    const onSessionCleared = vi.fn();
    const controller = (await startDesktopShell({ onUiEnvelope, onSessionCleared }))!;
    const session = fake.sessions.get(controller.identity.playerSessionPartition)!;
    const wipe = Promise.withResolvers<void>();
    session.clearStorageData.mockReturnValueOnce(wipe.promise);
    const logout: Envelope = { protocolVersion: PROTOCOL_VERSION, kind: "ui.logout", seq: 1, payload: {} };

    const first = sendFromMainUi(logout);
    await vi.waitFor(() => expect(session.clearStorageData).toHaveBeenCalledOnce());
    const second = sendFromMainUi({ ...logout, seq: 2 });
    wipe.resolve();
    expect(await Promise.all([first, second])).toEqual([{ ok: true }, { ok: true }]);
    expect(session.clearStorageData).toHaveBeenCalledTimes(1);
    expect(onUiEnvelope).toHaveBeenCalledTimes(1);
    expect(onSessionCleared).toHaveBeenCalledTimes(1);

    expect(await sendFromMainUi({ ...logout, seq: 3 })).toEqual({ ok: true });
    expect(session.clearStorageData).toHaveBeenCalledTimes(2);
  });

  it("stops credentials, unloads the old page, lets core drop auth, wipes the partition, then loads sign-in", async () => {
    const order: string[] = [];
    const hooks: DesktopHostHooks = {
      onUiEnvelope: (env) => {
        order.push(`core:${env.kind}`);
      },
      onSessionCleared: (reason) => {
        order.push(`sessionCleared:${reason}`);
      },
    };
    const controller = (await startDesktopShell(hooks))!;
    const playerSession = fake.sessions.get(controller.identity.playerSessionPartition)!;
    playerSession.clearStorageData.mockImplementation(async () => {
      order.push("clearStorageData");
    });
    playerSession.clearCache.mockImplementation(async () => {
      order.push("clearCache");
    });
    const player = playerWindow();
    player.loadURL.mockImplementation(async (url: string) => {
      order.push(`player:${url}`);
    });
    const cookieListenerCallsBefore = playerSession.cookies.on.mock.calls.length;

    const ack = await sendFromMainUi({
      protocolVersion: PROTOCOL_VERSION,
      kind: "ui.logout",
      seq: 1,
      payload: {},
    });

    expect(ack).toEqual({ ok: true });
    expect(order).toEqual([
      "player:about:blank",
      "core:ui.logout",
      "clearStorageData",
      "clearCache",
      "sessionCleared:logout",
      `player:${MUSIC_SIGN_IN_URL}`,
    ]);
    expect(playerSession.clearAuthCache).toHaveBeenCalled();
    // The broker was stopped before clearing and started again afterwards for the next sign-in.
    expect(playerSession.cookies.removeListener).toHaveBeenCalled();
    expect(playerSession.cookies.on.mock.calls.length).toBe(cookieListenerCallsBefore + 1);
  });
});

describe("startup failure", () => {
  it("logs the missing sidecar, stops the player and exits without waiting for a dialog", async () => {
    vi.stubEnv("YTM_GLASS_DAEMON_BIN", join(tempRoot, "missing-daemon"));
    const stderr = vi.spyOn(process.stderr, "write").mockReturnValue(true);
    try {
      await expect(startApp()).resolves.toBeNull();

      const errors = stderr.mock.calls.map(([line]) => JSON.parse(String(line)));
      expect(errors).toContainEqual(expect.objectContaining({
        level: "error",
        event: "startup_failed",
        msg: expect.stringContaining("sidecar binary not found"),
      }));
      expect(() => playerWindow()).toThrow("player window missing");
      expect(fake.electron.dialog.showErrorBox).not.toHaveBeenCalled();
      expect(fake.electron.app.exit).toHaveBeenCalledWith(1);
    } finally {
      stderr.mockRestore();
    }
  });
});

describe("player page lifecycle", () => {
  /** Emits Electron's will-prevent-unload (the page's beforeunload asked to stay) and reports whether the host overrode it. */
  function pageBlocksUnload(player: FakeBrowserWindow): boolean {
    const event = makeEvent();
    player.webContents.emit("will-prevent-unload", event);
    return !event.defaultPrevented;
  }

  it("lets app-initiated navigations through the page's beforeunload prompt, but not page-initiated ones", async () => {
    const controller = (await startDesktopShell({}))!;
    const player = playerWindow();
    // The official page asks to stay while music plays; nothing app-initiated is pending.
    expect(pageBlocksUnload(player)).toBe(true);

    // Logout blanking must really unload the old account's page.
    let blockedDuringBlank: boolean | null = null;
    player.loadURL.mockImplementation(async (url: string) => {
      if (url === "about:blank") blockedDuringBlank = pageBlocksUnload(player);
      player.webContents.url = url;
    });
    await controller.windowManager.blankPlayer();
    expect(blockedDuringBlank).toBe(false);

    // Once that navigation started, the page's own prompts count again.
    player.webContents.emit("did-start-navigation", { isMainFrame: true, isSameDocument: false });
    expect(pageBlocksUnload(player)).toBe(true);

    // Recovery reload while playing.
    controller.windowManager.reloadPlayer();
    expect(player.webContents.reload).toHaveBeenCalledTimes(1);
    expect(pageBlocksUnload(player)).toBe(false);
  });

  it("reports a renderer crash with the core's structured event and reloads it from 'open player page'", async () => {
    const lifecycle: Array<{ event: string; isMusicOrigin?: boolean }> = [];
    const controller = (await startDesktopShell({ onBridgeLifecycle: (p) => void lifecycle.push(p) }))!;
    const player = playerWindow();
    player.webContents.crashed = true;
    player.webContents.emit("render-process-gone", {}, { reason: "crashed", exitCode: 139 });
    expect(lifecycle.at(-1)).toEqual({ event: "crashed", isMusicOrigin: false });

    controller.windowManager.openPlayerWindow();
    expect(player.webContents.reload).toHaveBeenCalledTimes(1);
    expect(player.visible).toBe(true);
  });

  it("logs only the origin of a denied URL when identifying the remote site", async () => {
    const log = vi.fn();
    await startDesktopShell({ log });
    const event = makeEvent();
    playerWindow().webContents.emit(
      "will-navigate",
      event,
      "https://synthetic:password@example.test/account/token?code=synthetic#email"
    );

    expect(event.defaultPrevented).toBe(true);
    expect(log).toHaveBeenCalledWith("warn", "security_navigation_rejected", {
      window: "player",
      reason: "navigation_denied",
      origin: "https://example.test",
    });
  });
});

describe("tray", () => {
  it("routes playback entries to the main UI renderer and tracks play/pause from core", async () => {
    trayHost.registered = true;
    const controller = (await startDesktopShell())!;
    const tray = FakeTray.instances[0]!;
    const main = mainWindow();

    tray.click("Next");
    tray.click("Play");
    expect(main.webContents.send).toHaveBeenCalledWith(IPC_CHANNELS.UI_TRAY_COMMAND, "next");
    expect(main.webContents.send).toHaveBeenCalledWith(IPC_CHANNELS.UI_TRAY_COMMAND, "toggle");

    controller.deliverCoreEnvelope(
      coreEnvelope("core.delta", { deltaType: "playerChanged", revision: 3, pending: {}, player: { playback: "playing" } })
    );
    expect(tray.menuTemplate.some((item) => item.label === "Pause")).toBe(true);

    tray.click("Show / Hide Main Window");
    expect(main.isVisible()).toBe(false);
    tray.click("Show / Hide Main Window");
    expect(main.isVisible()).toBe(true);
  });
});

describe("app://covers", () => {
  it("returns HTTP 400 for malformed percent encoding on UI and cover routes", async () => {
    writeFileSync(join(tempRoot, "index.html"), "<title>fixture</title>");
    vi.stubEnv("YTM_GLASS_UI_DIST_DIR", tempRoot);
    const resolveCoverAsset = vi.fn();
    const controller = (await startDesktopShell({ resolveCoverAsset }))!;
    const handler = fake.sessions.get(controller.identity.uiSessionPartition)!
      .protocolHandlers.get(controller.identity.appScheme)!;

    for (const host of ["ui", "covers"]) {
      const response = await handler(new Request(`app://${host}/malformed%`));
      expect(response.status).toBe(400);
    }
    expect(fake.electron.net.fetch).not.toHaveBeenCalled();
    expect(resolveCoverAsset).not.toHaveBeenCalled();
  });

  it("serves only files inside the cover cache and reuses resolved paths", async () => {
    const coversRoot = join(tempRoot, "covers");
    mkdirSync(coversRoot);
    const inside = join(coversRoot, "a.jpg");
    writeFileSync(inside, "x");
    const outside = join(tempRoot, "secret.jpg");
    writeFileSync(outside, "y");

    const resolveCoverAsset = vi.fn(async (assetId: string) => ({
      filePath: assetId === "inside" ? inside : outside,
      cacheRoot: coversRoot,
      mimeType: "image/jpeg",
    }));
    const controller = (await startDesktopShell({ resolveCoverAsset }))!;
    const handler = fake.sessions
      .get(controller.identity.uiSessionPartition)!
      .protocolHandlers.get(controller.identity.appScheme)!;

    expect((await handler(new Request("app://covers/inside"))).status).toBe(200);
    expect((await handler(new Request("app://covers/inside"))).status).toBe(200);
    expect(resolveCoverAsset).toHaveBeenCalledTimes(1);
    expect(fake.electron.net.fetch).toHaveBeenCalledWith(expect.stringMatching(/covers\/a\.jpg$/));

    expect((await handler(new Request("app://covers/outside"))).status).toBe(404);
    expect((await handler(new Request("app://covers/..%2Fsecret"))).status).toBe(400);
  });

  it("shows an inline error page instead of any probe page when ui/dist is missing", async () => {
    const controller = (await startDesktopShell())!;
    const handler = fake.sessions
      .get(controller.identity.uiSessionPartition)!
      .protocolHandlers.get(controller.identity.appScheme)!;
    const html = await (await handler(new Request("app://ui/"))).text();
    expect(html).toContain("could not load its interface");
    expect(html).not.toContain("probe-hero-img");
  });
});

describe("track-change notifications", () => {
  const signedIn = { state: "authenticated", playerCatalogMatched: true };

  function playing(videoId: string, instance: string, title = `Song ${videoId}`): Record<string, unknown> {
    return {
      playback: "playing",
      trackInstanceId: instance,
      currentTrack: { videoId, title, availability: "playable", artists: [{ name: "Artist A" }, { name: "Artist B" }] },
    };
  }

  function playerDelta(player: Record<string, unknown>): Envelope {
    return coreEnvelope("core.delta", { deltaType: "playerChanged", revision: 2, pending: {}, player });
  }

  async function startWith(settings: Partial<AppSettings>, auth: unknown = signedIn) {
    const controller = (await startDesktopShell())!;
    vi.useFakeTimers();
    controller.deliverCoreEnvelope(
      coreEnvelope("core.snapshot", {
        settings: { ...SETTINGS, trackNotifications: true, ...settings },
        authState: auth,
        player: playing("v0", "i0"),
      })
    );
    mainWindow().focused = false;
    return controller;
  }

  it("shows one silent, passive notification for the track settled on after rapid skips", async () => {
    const controller = await startWith({});
    // The track already playing at startup is not announced.
    await vi.advanceTimersByTimeAsync(2_000);
    expect(FakeNotification.instances).toHaveLength(0);

    controller.deliverCoreEnvelope(playerDelta(playing("v1", "i1")));
    await vi.advanceTimersByTimeAsync(500);
    controller.deliverCoreEnvelope(playerDelta(playing("v2", "i2")));
    await vi.advanceTimersByTimeAsync(500);
    controller.deliverCoreEnvelope(playerDelta(playing("v3", "i3", "Last Song")));
    await vi.advanceTimersByTimeAsync(1_499);
    expect(FakeNotification.instances).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(1);

    expect(FakeNotification.instances).toHaveLength(1);
    const shown = FakeNotification.instances[0]!;
    expect(shown.shown).toBe(true);
    expect(shown.options).toEqual({ title: "Last Song", body: "Artist A, Artist B", silent: true });
    // Nothing that could raise or focus a window on click.
    expect(shown.listenedEvents()).not.toContain("click");
    expect(shown.listenedEvents()).not.toContain("action");

    // The same play rebuilt by a later snapshot/delta is not announced again.
    controller.deliverCoreEnvelope(playerDelta({ ...playing("v3", "i3", "Last Song"), playback: "paused" }));
    controller.deliverCoreEnvelope(playerDelta({ playback: "paused" }));
    controller.deliverCoreEnvelope(playerDelta(playing("v3", "i3", "Last Song")));
    await vi.advanceTimersByTimeAsync(2_000);
    expect(FakeNotification.instances).toHaveLength(1);

    // The next track replaces (closes) the previous notification.
    controller.deliverCoreEnvelope(playerDelta(playing("v4", "i4")));
    await vi.advanceTimersByTimeAsync(1_500);
    expect(FakeNotification.instances).toHaveLength(2);
    expect(shown.closed).toBe(true);
    expect(mainWindow().isFocused()).toBe(false);
    controller.shutdown();
  });

  it("stays quiet while the main window is focused, the setting is off or the account is signed out", async () => {
    const focusedRun = await startWith({});
    mainWindow().focused = true;
    focusedRun.deliverCoreEnvelope(playerDelta(playing("v1", "i1")));
    await vi.advanceTimersByTimeAsync(2_000);
    // Seen in the focused window: blurring later does not announce it.
    mainWindow().focused = false;
    focusedRun.deliverCoreEnvelope(playerDelta(playing("v1", "i1")));
    await vi.advanceTimersByTimeAsync(2_000);
    expect(FakeNotification.instances).toHaveLength(0);

    focusedRun.deliverCoreEnvelope(
      coreEnvelope("core.delta", {
        deltaType: "settingsChanged",
        revision: 3,
        settings: { ...SETTINGS, trackNotifications: false },
      })
    );
    focusedRun.deliverCoreEnvelope(playerDelta(playing("v2", "i2")));
    await vi.advanceTimersByTimeAsync(2_000);
    expect(FakeNotification.instances).toHaveLength(0);
    focusedRun.shutdown();
    vi.useRealTimers();

    const signedOut = await startWith({}, { state: "signedOut", playerCatalogMatched: false });
    signedOut.deliverCoreEnvelope(playerDelta(playing("v1", "i1")));
    await vi.advanceTimersByTimeAsync(2_000);
    expect(FakeNotification.instances).toHaveLength(0);

    // Signing out while a notification is pending cancels it.
    signedOut.deliverCoreEnvelope(
      coreEnvelope("core.delta", { deltaType: "authChanged", revision: 4, auth_state: signedIn })
    );
    signedOut.deliverCoreEnvelope(playerDelta(playing("v2", "i2")));
    await vi.advanceTimersByTimeAsync(1_000);
    signedOut.deliverCoreEnvelope(
      coreEnvelope("core.delta", { deltaType: "authChanged", revision: 5, auth_state: { state: "signedOut", playerCatalogMatched: false } })
    );
    await vi.advanceTimersByTimeAsync(2_000);
    expect(FakeNotification.instances).toHaveLength(0);
    signedOut.shutdown();
  });
});

describe("track-change notifications: corrected metadata", () => {
  it("shows the latest title and artists reported for the same play within the debounce", async () => {
    const controller = (await startDesktopShell())!;
    vi.useFakeTimers();
    const track = (artists: string[], title = "Song"): Record<string, unknown> => ({
      playback: "playing",
      trackInstanceId: "i1",
      currentTrack: { videoId: "v1", title, availability: "playable", artists: artists.map((name) => ({ name })) },
    });
    controller.deliverCoreEnvelope(
      coreEnvelope("core.snapshot", {
        settings: { ...SETTINGS, trackNotifications: true },
        authState: { state: "authenticated", playerCatalogMatched: true },
        player: { playback: "paused" },
      })
    );
    mainWindow().focused = false;
    const delta = (player: Record<string, unknown>): Envelope =>
      coreEnvelope("core.delta", { deltaType: "playerChanged", revision: 2, pending: {}, player });

    controller.deliverCoreEnvelope(delta(track([], "Loading…")));
    await vi.advanceTimersByTimeAsync(1_000);
    controller.deliverCoreEnvelope(delta(track(["Real Artist"], "Real Title")));
    // The correction does not restart the debounce.
    await vi.advanceTimersByTimeAsync(500);

    expect(FakeNotification.instances).toHaveLength(1);
    expect(FakeNotification.instances[0]!.options).toEqual({ title: "Real Title", body: "Real Artist", silent: true });
    controller.shutdown();
  });
});

describe("UI scale", () => {
  it("zooms the main UI to the core's UI scale, clamped to 75%-150%, and re-applies after a load", async () => {
    const controller = (await startDesktopShell())!;
    const wc = mainWindow().webContents;
    expect(wc.setZoomFactor).not.toHaveBeenCalled();

    controller.deliverCoreEnvelope(snapshotWith({ ...SETTINGS, uiScalePercent: 125 }));
    expect(wc.setZoomFactor).toHaveBeenLastCalledWith(1.25);

    const scale = (uiScalePercent: number): void =>
      controller.deliverCoreEnvelope(
        coreEnvelope("core.delta", { deltaType: "settingsChanged", revision: 2, settings: { ...SETTINGS, uiScalePercent } })
      );
    scale(400);
    expect(wc.setZoomFactor).toHaveBeenLastCalledWith(1.5);
    scale(10);
    expect(wc.setZoomFactor).toHaveBeenLastCalledWith(0.75);

    wc.setZoomFactor.mockClear();
    wc.emit("did-finish-load");
    expect(wc.setZoomFactor).toHaveBeenCalledWith(0.75);
    controller.shutdown();
  });
});

describe("tray host watcher", () => {
  it("creates the tray when the StatusNotifier host appears and removes it when it goes away", async () => {
    const onTrayAvailabilityChanged = vi.fn();
    // The watcher's interval starts during startup.
    vi.useFakeTimers();
    const controller = (await startDesktopShell({ onTrayAvailabilityChanged }))!;
    controller.deliverCoreEnvelope(snapshotWith({ ...SETTINGS, closeToTray: true }, "playing"));
    expect(controller.trayAvailable).toBe(false);
    expect(FakeTray.instances).toHaveLength(0);

    // plasmashell starts late.
    trayHost.registered = true;
    await vi.advanceTimersByTimeAsync(10_000);
    expect(FakeTray.instances).toHaveLength(1);
    expect(controller.trayAvailable).toBe(true);
    expect(onTrayAvailabilityChanged).toHaveBeenLastCalledWith(true);
    // The late tray shows the current playback state.
    expect(FakeTray.instances[0]!.menuTemplate.some((item) => item.label === "Pause")).toBe(true);
    const hidden = makeEvent();
    mainWindow().emit("close", hidden);
    expect(hidden.defaultPrevented).toBe(true);
    mainWindow().show();

    // plasmashell restarts under a new bus name: the tray is re-registered, availability unchanged.
    trayHost.owner = ":1.99";
    await vi.advanceTimersByTimeAsync(10_000);
    expect(FakeTray.instances).toHaveLength(2);
    expect(FakeTray.instances[0]!.destroy).toHaveBeenCalledTimes(1);
    expect(onTrayAvailabilityChanged).toHaveBeenCalledTimes(1);

    // The host goes away: after two definitive answers there is no tray, and closing must not
    // hide the window into nothing.
    trayHost.registered = false;
    await vi.advanceTimersByTimeAsync(10_000);
    expect(FakeTray.instances[1]!.destroy).not.toHaveBeenCalled();
    expect(controller.trayAvailable).toBe(true);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(FakeTray.instances[1]!.destroy).toHaveBeenCalledTimes(1);
    expect(controller.trayAvailable).toBe(false);
    expect(onTrayAvailabilityChanged).toHaveBeenLastCalledWith(false);
    const closing = makeEvent();
    mainWindow().emit("close", closing);
    expect(closing.defaultPrevented).toBe(false);
    expect(fake.electron.app.quit).toHaveBeenCalledTimes(1);

    // No probing after shutdown.
    controller.shutdown();
    trayHost.registered = true;
    await vi.advanceTimersByTimeAsync(30_000);
    expect(FakeTray.instances).toHaveLength(2);
  });

  it("keeps the tray through failed probes and only drops it after two definitive negatives", async () => {
    trayHost.registered = true;
    vi.useFakeTimers();
    const controller = (await startDesktopShell())!;
    controller.deliverCoreEnvelope(snapshotWith({ ...SETTINGS, closeToTray: true }));
    expect(controller.trayAvailable).toBe(true);

    // gdbus times out under a heavy game load, repeatedly: the tray and close-to-tray stay.
    trayHost.failing = true;
    await vi.advanceTimersByTimeAsync(40_000);
    expect(FakeTray.instances[0]!.destroy).not.toHaveBeenCalled();
    expect(controller.trayAvailable).toBe(true);
    const hidden = makeEvent();
    mainWindow().emit("close", hidden);
    expect(hidden.defaultPrevented).toBe(true);

    // One definitive "no owner" between failures is not confirmation either.
    trayHost.failing = false;
    trayHost.registered = false;
    await vi.advanceTimersByTimeAsync(10_000);
    trayHost.failing = true;
    await vi.advanceTimersByTimeAsync(10_000);
    trayHost.failing = false;
    trayHost.registered = true;
    await vi.advanceTimersByTimeAsync(10_000);
    trayHost.registered = false;
    await vi.advanceTimersByTimeAsync(10_000);
    expect(controller.trayAvailable).toBe(true);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(controller.trayAvailable).toBe(false);
    expect(FakeTray.instances).toHaveLength(1);
    expect(FakeTray.instances[0]!.destroy).toHaveBeenCalledTimes(1);
    controller.shutdown();
  });

  it("shows a window hidden in a lost tray without focusing it, and leaves visible or minimized windows alone", async () => {
    for (const state of ["hidden", "visible", "minimized"] as const) {
      fake.reset();
      trayHost.registered = true;
      vi.useFakeTimers();
      const controller = (await startDesktopShell())!;
      controller.deliverCoreEnvelope(snapshotWith({ ...SETTINGS, closeToTray: true }));
      const main = mainWindow();
      main.focused = false;
      if (state === "hidden") {
        main.emit("close", makeEvent());
        expect(main.isVisible()).toBe(false);
      } else if (state === "minimized") {
        main.minimized = true;
      }
      const focus = vi.spyOn(main, "focus");
      const show = vi.spyOn(main, "show");

      trayHost.registered = false;
      await vi.advanceTimersByTimeAsync(20_000);
      expect(controller.trayAvailable).toBe(false);

      expect(main.showInactive).toHaveBeenCalledTimes(state === "hidden" ? 1 : 0);
      expect(main.isVisible()).toBe(true);
      expect(focus).not.toHaveBeenCalled();
      expect(show).not.toHaveBeenCalled();
      expect(main.flashFrame).not.toHaveBeenCalledWith(true);
      expect(main.isFocused()).toBe(false);
      controller.shutdown();
      vi.useRealTimers();
    }
  });
});
