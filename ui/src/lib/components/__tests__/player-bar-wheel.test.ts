import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { FakeCoreClient, type AppSnapshot, type CommandReceipt } from "@ytmcp/protocol";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import PlayerBar from "../PlayerBar.svelte";

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const overlay = { pendingCount: 0, activeOp: null, desiredPlayback: null, desiredPositionMs: null, desiredVolume: null, lastError: null };

function renderBar(edit: (s: AppSnapshot) => AppSnapshot = (s) => s) {
  const base = new FakeCoreClient("normal").subscribeSnapshot(() => {}).snapshot;
  const snapshot = edit({ ...base, player: { ...base.player, volume: 0.5, muted: false } });
  // Never settles: the bar keeps its own draft, as while the player has not reported yet.
  const onCommand = vi.fn((..._args: unknown[]) => new Promise<CommandReceipt | null>(() => {}));
  const view = render(PlayerBar, {
    props: { snapshot, pendingOverlay: overlay, interpolatedPositionMs: 0, onCommand, onOpenNowPlaying: () => {}, onToggleLyrics: () => {} },
  });
  const volumeCalls = () => onCommand.mock.calls.filter(([op]) => op === "setVolume").map(([, args]) => args);
  return { ...view, onCommand, volumeCalls };
}

// The level is the slider's accessible value (no words are shown in the bar).
const shown = () => screen.getByTestId("player-volume-slider").getAttribute("aria-valuetext");

describe("Player bar volume wheel", () => {
  it("steps a notch per wheel click, adds up trackpad movement, and keeps the trailing send", async () => {
    const { volumeCalls } = renderBar();
    const slider = screen.getByTestId("player-volume-slider");

    // Consumed (default prevented), sent at once on the leading edge.
    expect(await fireEvent.wheel(slider, { deltaY: -100, deltaMode: 0 })).toBe(false);
    expect(volumeCalls()).toEqual([{ volume: 0.52 }]);

    // A line-mode notch inside the 60 ms window is shown now and sent on the trailing edge.
    await fireEvent.wheel(slider, { deltaY: -3, deltaMode: 1 });
    expect(shown()).toBe("54%");
    expect(volumeCalls()).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(60);
    expect(volumeCalls().at(-1)).toEqual({ volume: 0.54 });

    // Small pixel deltas (a trackpad) accumulate instead of rounding away.
    for (let i = 0; i < 5; i++) await fireEvent.wheel(slider, { deltaY: -10, deltaMode: 0 });
    await vi.advanceTimersByTimeAsync(60);
    expect(shown()).toBe("55%");
    expect(volumeCalls().at(-1)).toEqual({ volume: 0.55 });

    // Clamped at the ends; the mute button area counts as the volume control too.
    await vi.advanceTimersByTimeAsync(60);
    await fireEvent.wheel(screen.getByTestId("btn-mute"), { deltaY: 100_000, deltaMode: 0 });
    expect(shown()).toBe("0%");
    expect(volumeCalls().at(-1)).toEqual({ volume: 0 });
  });

  it("sends no mute change, only the volume, just like dragging", async () => {
    const { onCommand } = renderBar((s) => ({ ...s, player: { ...s.player, muted: true } }));
    await fireEvent.wheel(screen.getByTestId("player-volume-slider"), { deltaY: -100, deltaMode: 0 });
    expect(onCommand.mock.calls.map(([op]) => op)).toEqual(["setVolume"]);
  });

  it("leaves zoom, horizontal gestures, a disabled control and the seek bar to the page", async () => {
    const { onCommand } = renderBar();
    const slider = screen.getByTestId("player-volume-slider");
    expect(await fireEvent.wheel(slider, { deltaY: -100, ctrlKey: true })).toBe(true);
    expect(await fireEvent.wheel(slider, { deltaX: 80, deltaY: 10 })).toBe(true);
    // The seek bar never gains wheel seeking.
    expect(await fireEvent.wheel(screen.getByTestId("player-seek-slider"), { deltaY: -100 })).toBe(true);
    expect(onCommand).not.toHaveBeenCalled();
    cleanup();

    const disabled = renderBar((s) => ({ ...s, capabilities: { ...s.capabilities, volume: "unsupported" } }));
    expect(await fireEvent.wheel(screen.getByTestId("player-volume-slider"), { deltaY: -100 })).toBe(true);
    expect(disabled.onCommand).not.toHaveBeenCalled();
  });

  it("drops the trailing send when the bar goes away", async () => {
    const { volumeCalls, unmount } = renderBar();
    const slider = screen.getByTestId("player-volume-slider");
    await fireEvent.wheel(slider, { deltaY: -100, deltaMode: 0 });
    await fireEvent.wheel(slider, { deltaY: -100, deltaMode: 0 });
    expect(volumeCalls()).toHaveLength(1);
    unmount();
    await vi.advanceTimersByTimeAsync(200);
    expect(volumeCalls()).toHaveLength(1);
  });
});
