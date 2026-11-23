import { describe, expect, it, vi } from "vitest";

const ipc = vi.hoisted(() => ({
  listeners: new Map<string, (event: unknown, ...args: unknown[]) => void>(),
  invoke: vi.fn(async () => ({ ok: true })),
}));

vi.mock("../../../apps/desktop/node_modules/electron/index.js", () => ({
  contextBridge: { exposeInMainWorld: vi.fn() },
  ipcRenderer: {
    invoke: ipc.invoke,
    on: (channel: string, fn: (event: unknown, ...args: unknown[]) => void) => {
      ipc.listeners.set(channel, fn);
    },
    removeListener: vi.fn(),
  },
}));

import { validateRendererIpc } from "../../../apps/desktop/src/bootstrap/ipc-guard.js";
import { IPC_CHANNELS } from "../../../apps/desktop/preload/channels.js";

describe("main UI preload tray command route", () => {
  it("issues tray actions as ordinary mainUi ui.playerCommand envelopes and ignores anything else", async () => {
    // The preload only installs itself in a top-level frame.
    vi.stubGlobal("window", globalThis);
    vi.stubGlobal("top", globalThis);
    await import("../../../apps/desktop/preload/ui-preload.js");

    const trayListener = ipc.listeners.get(IPC_CHANNELS.UI_TRAY_COMMAND);
    expect(trayListener).toBeTypeOf("function");

    trayListener!({}, "seek");
    trayListener!({}, { op: "toggle" });
    expect(ipc.invoke).not.toHaveBeenCalled();

    trayListener!({}, "toggle");
    trayListener!({}, "next");
    expect(ipc.invoke).toHaveBeenCalledTimes(2);

    const [channel, envelope] = ipc.invoke.mock.calls[0] as unknown as [string, unknown];
    expect(channel).toBe(IPC_CHANNELS.UI_SEND);
    const verdict = validateRendererIpc(
      {
        role: "mainUi",
        allowedOrigins: new Set(["app://ui"]),
        senderMatchesExpected: true,
        senderFrame: { isMainFrame: true, origin: "app://ui" },
      },
      envelope
    );
    expect(verdict.ok).toBe(true);
    if (verdict.ok) {
      expect(verdict.envelope.kind).toBe("ui.playerCommand");
      expect(verdict.envelope.payload).toMatchObject({ op: "toggle" });
    }
    const sent = ipc.invoke.mock.calls.map(
      (call) => (call as unknown as [string, { payload: { op: string; commandId: string } }])[1].payload
    );
    expect(sent.map((p) => p.op)).toEqual(["toggle", "next"]);
    expect(new Set(sent.map((p) => p.commandId)).size).toBe(2);
    vi.unstubAllGlobals();
  });
});
