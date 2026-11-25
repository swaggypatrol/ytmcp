import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { FakeCoreClient, type AppSnapshot, type CommandReceipt } from "@ytmcp/protocol";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ClientPendingOverlay } from "../../ipc/client.js";
import PlayerBar from "../PlayerBar.svelte";

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const overlay: ClientPendingOverlay = { pendingCount: 0, activeOp: null, desiredPlayback: null, desiredPositionMs: null, desiredVolume: null, lastError: null };
/** The client's overlay while a setVolume to `volume` is unsettled. */
const volumePending = (volume: number): ClientPendingOverlay => ({ ...overlay, pendingCount: 1, activeOp: "setVolume", desiredVolume: volume });

type OnCommand = (...args: unknown[]) => Promise<CommandReceipt | null>;

function renderBar(edit: (s: AppSnapshot) => AppSnapshot = (s) => s, command?: OnCommand) {
  const base = new FakeCoreClient("normal").subscribeSnapshot(() => {}).snapshot;
  const snapshot = edit({ ...base, player: { ...base.player, volume: 0.5, muted: false } });
  // By default never settles: the bar keeps its own draft, as while the player has not reported yet.
  const onCommand = vi.fn<OnCommand>(command ?? (() => new Promise<CommandReceipt | null>(() => {})));
  const view = render(PlayerBar, {
    props: { snapshot, pendingOverlay: overlay, interpolatedPositionMs: 0, onCommand, onOpenNowPlaying: () => {}, onToggleLyrics: () => {} },
  });
  const volumeCalls = () => onCommand.mock.calls.filter(([op]) => op === "setVolume").map(([, args]) => args);
  /** A new projection from the player: its observed volume and what is still pending. */
  const report = (volume: number, pendingOverlay: ClientPendingOverlay = overlay, extra: Partial<AppSnapshot> = {}) =>
    view.rerender({ snapshot: { ...snapshot, ...extra, player: { ...snapshot.player, volume } }, pendingOverlay });
  return { ...view, snapshot, onCommand, volumeCalls, report };
}

// The level is the slider's accessible value (no words are shown in the bar).
const shown = () => screen.getByTestId("player-volume-slider").getAttribute("aria-valuetext");

/** A wheel event as Chromium builds it, with the legacy notch delta jsdom does not support. */
async function wheel(target: Element, init: WheelEventInit, wheelDeltaY?: number) {
  const event = new WheelEvent("wheel", { bubbles: true, cancelable: true, ...init });
  if (wheelDeltaY !== undefined) Object.defineProperty(event, "wheelDeltaY", { value: wheelDeltaY });
  return fireEvent(target, event);
}
/** One notch at 1x: wheelDeltaY is ±120 whatever pixel distance comes with it. */
const notchUp = (target: Element, px = 53) => wheel(target, { deltaY: -px }, 120);
const notchDown = (target: Element, px = 53) => wheel(target, { deltaY: px }, -120);

const failed = (commandId: string): CommandReceipt => ({
  commandId,
  op: "setVolume",
  status: "failed",
  acceptedAtMs: 0,
  observedAtMs: null,
  deduplicated: false,
  error: { code: "TIMEOUT", retriable: false, retryAfterMs: null, safeMessage: "player timed out", requestId: null },
});

describe("Player bar volume wheel", () => {
  it("moves five points a notch, adds up touchpad movement, and keeps the trailing send", async () => {
    const { volumeCalls } = renderBar();
    const slider = screen.getByTestId("player-volume-slider");

    // Consumed (default prevented), sent at once on the leading edge.
    expect(await notchUp(slider)).toBe(false);
    expect(volumeCalls()).toEqual([{ volume: 0.55 }]);

    // A line-mode notch inside the 60 ms window is shown now and sent on the trailing edge.
    await wheel(slider, { deltaY: -3, deltaMode: 1 });
    expect(shown()).toBe("60%");
    expect(volumeCalls()).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(60);
    expect(volumeCalls().at(-1)).toEqual({ volume: 0.6 });

    // Small deltas (a touchpad) add up instead of each counting as a notch: 8 × a quarter point.
    for (let i = 0; i < 8; i++) await wheel(slider, { deltaY: -6 }, 6);
    await vi.advanceTimersByTimeAsync(60);
    expect(shown()).toBe("62%");
    expect(volumeCalls().at(-1)).toEqual({ volume: 0.62 });

    // Clamped at the ends; the mute button area counts as the volume control too.
    await vi.advanceTimersByTimeAsync(60);
    await wheel(screen.getByTestId("btn-mute"), { deltaY: 100_000 }, -120_000);
    expect(shown()).toBe("0%");
    expect(volumeCalls().at(-1)).toEqual({ volume: 0 });
    // Still taken at the limit, so the page does not scroll instead.
    expect(await notchDown(slider)).toBe(false);
  });

  it("unmutes once when raised while muted, and stays muted when lowered", async () => {
    const { onCommand } = renderBar((s) => ({ ...s, player: { ...s.player, muted: true } }));
    const slider = screen.getByTestId("player-volume-slider");
    await notchDown(slider);
    expect(onCommand.mock.calls.map(([op]) => op)).toEqual(["setVolume"]);
    await vi.advanceTimersByTimeAsync(60);
    await notchUp(slider);
    await vi.advanceTimersByTimeAsync(60);
    await notchUp(slider);
    await vi.advanceTimersByTimeAsync(60);
    expect(onCommand.mock.calls.map(([op, args]) => [op, args])).toEqual([
      ["setVolume", { volume: 0.45 }],
      ["setVolume", { volume: 0.5 }],
      ["setMuted", { muted: false }],
      ["setVolume", { volume: 0.55 }],
    ]);
  });

  it("unmutes on the wheel up when muted at 100, with no volume to send", async () => {
    const { onCommand } = renderBar((s) => ({ ...s, player: { ...s.player, volume: 1, muted: true } }));
    expect(await notchUp(screen.getByTestId("player-volume-slider"))).toBe(false);
    expect(onCommand.mock.calls).toEqual([["setMuted", { muted: false }]]);
  });

  it("holds the next target while a volume command is unsettled, and always sends the last one", async () => {
    const { volumeCalls, report } = renderBar();
    const slider = screen.getByTestId("player-volume-slider");
    await notchUp(slider);
    await report(0.5, volumePending(0.55));
    for (let i = 0; i < 3; i++) {
      await vi.advanceTimersByTimeAsync(70);
      await notchUp(slider);
    }
    expect(shown()).toBe("70%");
    expect(volumeCalls()).toEqual([{ volume: 0.55 }]);
    // The first one settles: only the newest target goes, nothing in between.
    await report(0.55);
    expect(volumeCalls()).toEqual([{ volume: 0.55 }, { volume: 0.7 }]);
  });

  it("sends anyway when a volume command stays unsettled too long", async () => {
    const { volumeCalls, report } = renderBar();
    const slider = screen.getByTestId("player-volume-slider");
    await notchUp(slider);
    await report(0.5, volumePending(0.55));
    await notchUp(slider);
    await vi.advanceTimersByTimeAsync(249);
    expect(volumeCalls()).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(volumeCalls().at(-1)).toEqual({ volume: 0.6 });
  });

  it("is never pulled back by a confirmation of an older target", async () => {
    const { report } = renderBar();
    const slider = screen.getByTestId("player-volume-slider");
    // Up then straight back down: the draft is 50 again while 55 is still on its way.
    await notchUp(slider);
    await report(0.5, volumePending(0.55));
    await notchDown(slider);
    expect(shown()).toBe("50%");
    // The player confirms 55; the bar keeps 50, which is sent next.
    await report(0.55, volumePending(0.5));
    expect(shown()).toBe("50%");
    await report(0.5);
    expect(shown()).toBe("50%");
  });

  it("returns to the true volume at once when the newest command fails", async () => {
    const { snapshot } = renderBar(undefined, async () => failed("cmd-vol"));
    await notchUp(screen.getByTestId("player-volume-slider"));
    await vi.advanceTimersByTimeAsync(0);
    expect(shown()).toBe("50%");
    cleanup();

    // A failure that arrives later, as the core's receipt in the projection.
    let n = 0;
    const later = renderBar(undefined, async () => ({ ...failed(`cmd-${++n}`), status: "accepted", error: null }));
    await notchUp(screen.getByTestId("player-volume-slider"));
    await vi.advanceTimersByTimeAsync(0);
    expect(shown()).toBe("55%");
    await later.report(0.5, overlay, { pending: { ...snapshot.pending, lastReceipt: failed("cmd-1") } });
    expect(shown()).toBe("50%");
  });

  it("shows the player's volume when our newest command is cancelled, but not for an older one", async () => {
    let n = 0;
    const accepted = async () => ({ ...failed(`cmd-${++n}`), status: "accepted" as const, error: null });
    const cancelled = (commandId: string): CommandReceipt => ({ ...failed(commandId), status: "cancelled", error: null });
    const { snapshot, report } = renderBar(undefined, accepted);
    const slider = screen.getByTestId("player-volume-slider");
    await notchUp(slider);
    await vi.advanceTimersByTimeAsync(0);
    expect(shown()).toBe("55%");
    // Another client set 70 and the core dropped ours: the player decided otherwise.
    await report(0.7, overlay, { pending: { ...snapshot.pending, lastReceipt: cancelled("cmd-1") } });
    expect(shown()).toBe("70%");
    cleanup();

    n = 0;
    const later = renderBar(undefined, accepted);
    const again = screen.getByTestId("player-volume-slider");
    await notchUp(again);
    await vi.advanceTimersByTimeAsync(60);
    await notchUp(again);
    await vi.advanceTimersByTimeAsync(0);
    expect(later.volumeCalls()).toHaveLength(2);
    // The first one is cancelled, the newer one is still on its way: the draft stays.
    await later.report(0.5, volumePending(0.6), { pending: { ...later.snapshot.pending, lastReceipt: cancelled("cmd-1") } });
    expect(shown()).toBe("60%");
  });

  it("drops a draft the player never reports after the timeout, however often the player updates", async () => {
    const { report } = renderBar();
    await notchUp(screen.getByTestId("player-volume-slider"));
    expect(shown()).toBe("55%");
    // The player keeps reporting its own volume every second (position updates).
    for (let i = 0; i < 3; i++) {
      await vi.advanceTimersByTimeAsync(1_000);
      await report(0.5);
    }
    expect(shown()).toBe("50%");
  });

  it("leaves zoom, horizontal gestures, a disabled control and the seek bar to the page", async () => {
    const { onCommand } = renderBar();
    const slider = screen.getByTestId("player-volume-slider");
    expect(await wheel(slider, { deltaY: -53, ctrlKey: true }, 120)).toBe(true);
    expect(await wheel(slider, { deltaX: 80, deltaY: 10 })).toBe(true);
    // The seek bar never gains wheel seeking.
    expect(await notchUp(screen.getByTestId("player-seek-slider"))).toBe(true);
    expect(onCommand).not.toHaveBeenCalled();
    cleanup();

    const disabled = renderBar((s) => ({ ...s, capabilities: { ...s.capabilities, volume: "unsupported" } }));
    expect(await notchUp(screen.getByTestId("player-volume-slider"))).toBe(true);
    expect(disabled.onCommand).not.toHaveBeenCalled();
  });

  it("drops the trailing send when the bar goes away", async () => {
    const { volumeCalls, unmount } = renderBar();
    const slider = screen.getByTestId("player-volume-slider");
    await notchUp(slider);
    await notchUp(slider);
    expect(volumeCalls()).toHaveLength(1);
    unmount();
    await vi.advanceTimersByTimeAsync(200);
    expect(volumeCalls()).toHaveLength(1);
  });
});
