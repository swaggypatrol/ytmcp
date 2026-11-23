import { PROTOCOL_VERSION, type Envelope } from "@ytm-glass/protocol";
import { expect, it, vi } from "vitest";

vi.mock("electron", () => ({ app: {}, ipcMain: {}, net: {}, protocol: {} }));

const { trayPlayingFromCoreEnvelope } = await import("../main.js");

const envelope = (kind: Envelope["kind"], payload: unknown): Envelope => ({
  protocolVersion: PROTOCOL_VERSION,
  kind,
  coreEpoch: "core-1",
  seq: 1,
  payload,
});

it("clears tray playback on a core restart and restores it from the new core's state", () => {
  expect(trayPlayingFromCoreEnvelope(envelope("core.snapshot", { player: { playback: "playing" } }))).toBe(true);
  expect(trayPlayingFromCoreEnvelope(envelope("core.ready", { coreEpoch: "core-2", protocolVersion: PROTOCOL_VERSION }))).toBe(false);
  expect(trayPlayingFromCoreEnvelope(envelope("core.snapshot", { player: { playback: "paused" } }))).toBe(false);
  expect(
    trayPlayingFromCoreEnvelope(envelope("core.delta", { deltaType: "playerChanged", revision: 2, player: { playback: "buffering" } }))
  ).toBe(true);
  expect(trayPlayingFromCoreEnvelope(envelope("core.delta", { deltaType: "networkChanged", revision: 3 }))).toBeNull();
  expect(trayPlayingFromCoreEnvelope(envelope("core.error", {}))).toBeNull();
});
