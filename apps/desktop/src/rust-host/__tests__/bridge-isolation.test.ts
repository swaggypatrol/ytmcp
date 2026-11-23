import { afterEach, describe, expect, it, vi } from "vitest";

const electron = vi.hoisted(() => ({
  exposeInMainWorld: vi.fn(),
  exposeInIsolatedWorld: vi.fn(),
}));

vi.mock("electron", () => ({
  contextBridge: {
    exposeInMainWorld: electron.exposeInMainWorld,
    exposeInIsolatedWorld: electron.exposeInIsolatedWorld,
  },
  ipcRenderer: { invoke: vi.fn(), on: vi.fn(), removeListener: vi.fn() },
  app: { isPackaged: false },
  BrowserWindow: vi.fn(),
  session: { fromPartition: vi.fn() },
  shell: { openExternal: vi.fn() },
}));

import { BRIDGE_WORLD_ID, MUSIC_ORIGIN } from "../../../preload/channels.js";
import { buildBridgeInjectionScript, injectBridgeIntoMusicPlayer } from "../bridge-injector.js";

function stubPageWindow(origin: string): void {
  const win = { location: { origin } } as { location: { origin: string }; top?: unknown };
  win.top = win;
  vi.stubGlobal("window", win);
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
  electron.exposeInMainWorld.mockClear();
  electron.exposeInIsolatedWorld.mockClear();
});

describe("player bridge world isolation", () => {
  it("exposes ytmGlassBridge only in the bridge isolated world, never to the page main world", async () => {
    stubPageWindow(MUSIC_ORIGIN);
    await import("../../../preload/player-preload.js");

    expect(electron.exposeInMainWorld).not.toHaveBeenCalled();
    expect(electron.exposeInIsolatedWorld).toHaveBeenCalledTimes(1);
    const [worldId, key, api] = electron.exposeInIsolatedWorld.mock.calls[0] as [number, string, object];
    // World 0 is the page main world; Electron's own preload world is 999.
    expect(worldId).toBe(BRIDGE_WORLD_ID);
    expect(worldId).not.toBe(0);
    expect(worldId).not.toBe(999);
    expect(key).toBe("ytmGlassBridge");
    expect(Object.isFrozen(api)).toBe(true);
    expect(Object.keys(api).sort()).toEqual(["onEnvelope", "sendEnvelope"]);
  });

  it("exposes nothing outside the music.youtube.com top frame", async () => {
    stubPageWindow("https://accounts.google.com");
    await import("../../../preload/player-preload.js");
    expect(electron.exposeInMainWorld).not.toHaveBeenCalled();
    expect(electron.exposeInIsolatedWorld).not.toHaveBeenCalled();
  });

  it("injects only the page shim into the main world and the bridge into the isolated world", async () => {
    const mainWorld = vi.fn(async () => undefined);
    const isolatedWorld = vi.fn(async () => ({ installed: true }));
    const playerWin = {
      isDestroyed: () => false,
      webContents: {
        isDestroyed: () => false,
        getURL: () => `${MUSIC_ORIGIN}/`,
        mainFrame: { parent: null, origin: MUSIC_ORIGIN, executeJavaScript: mainWorld },
        executeJavaScript: mainWorld,
        executeJavaScriptInIsolatedWorld: isolatedWorld,
      },
    };
    const assets = {
      pageShimSource: "/* page shim */",
      bridgeProbeSource: "function installBridge(t, e) { return { installed: true }; }\nexport { installBridge };\n",
    };

    const epoch = await injectBridgeIntoMusicPlayer(playerWin as never, assets);

    expect(epoch).toMatch(/^bridge-/);
    expect(mainWorld).toHaveBeenCalledTimes(1);
    expect(mainWorld).toHaveBeenCalledWith(assets.pageShimSource, true);
    expect(isolatedWorld).toHaveBeenCalledTimes(1);
    const [worldId, scripts] = isolatedWorld.mock.calls[0] as unknown as [number, Array<{ code: string }>];
    expect(worldId).toBe(BRIDGE_WORLD_ID);
    expect(scripts).toHaveLength(1);
    expect(scripts[0]!.code).toContain("installBridge(window.ytmGlassBridge");
    expect(scripts[0]!.code).toContain(JSON.stringify(epoch));
  });

  it("hands the host only a cloneable result, even though the bridge handle carries functions", () => {
    // The real installBridge returns its uninstall/getStateSnapshot functions; Electron rejects
    // a non-cloneable script result, which made every successful injection look failed.
    const source =
      "function installBridge(t, e) { return { installed: true, bridgeEpoch: e, uninstall: () => {}, getStateSnapshot: () => null }; }\nexport { installBridge };\n";
    const code = buildBridgeInjectionScript(source, "bridge-test");
    const fakeWindow: Record<string, unknown> = { location: { origin: MUSIC_ORIGIN }, ytmGlassBridge: {} };
    fakeWindow.top = fakeWindow;
    const result = new Function("window", `return ${code}`)(fakeWindow) as unknown;
    expect(structuredClone(result)).toEqual({ installed: true, reason: null });
  });
});
