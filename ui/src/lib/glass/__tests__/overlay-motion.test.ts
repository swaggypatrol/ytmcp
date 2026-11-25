import { afterEach, describe, expect, it, vi } from "vitest";
import { OverlayPresence, afterExit } from "../overlay-motion.svelte.js";

afterEach(() => {
  vi.useRealTimers();
});

/** A stand-in for a running CSS transition whose end the test decides. */
function transition(endTime = 160) {
  let settle!: () => void;
  let cancel!: () => void;
  const finished = new Promise<void>((resolve, reject) => {
    settle = resolve;
    cancel = () => reject(new DOMException("replaced", "AbortError"));
  });
  finished.catch(() => {});
  const animation = { playState: "running", effect: { getComputedTiming: () => ({ endTime }) }, finished } as unknown as Animation;
  return { animation, settle, cancel };
}

/** A surface with a 160 ms exit clock whose every close starts the next set of transitions. */
function surface(...exits: Animation[][]): HTMLElement {
  const el = document.createElement("div");
  el.style.setProperty("--lg-overlay-exit-duration", "160ms");
  let call = 0;
  el.getAnimations = () => exits[call++] ?? [];
  document.body.append(el);
  return el;
}

const flush = () => vi.advanceTimersByTimeAsync(0);

describe("overlay exit lifecycle", () => {
  it("keeps a closed overlay until its transitions have run, even when they start late", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const a = transition();
    const el = surface([a.animation]);
    const presence = new OverlayPresence();
    presence.show();
    presence.hide(el);
    expect(el.dataset.leaving).toBe("dismiss");
    expect(el.inert).toBe(true);
    // A busy main thread delayed the first exit frame: the clock alone would remove it mid-exit.
    await vi.advanceTimersByTimeAsync(400);
    expect(presence.rendered).toBe(true);
    a.settle();
    await flush();
    expect(presence.rendered).toBe(false);
  });

  it("never lets an exit that a reopen interrupted remove the reopened surface", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const a = transition();
    const el = surface([a.animation]);
    const presence = new OverlayPresence();
    presence.show();
    presence.hide(el);
    presence.show();
    expect(el.dataset.leaving).toBeUndefined();
    expect(el.inert).toBe(false);
    // Reversing the transitions cancels or finishes the old ones later; the old clock passes too.
    a.cancel();
    a.settle();
    await vi.advanceTimersByTimeAsync(5000);
    expect(presence.rendered).toBe(true);
  });

  it("removes a surface closed again after a reopen by its second exit, not its first", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const first = transition();
    const second = transition();
    const el = surface([first.animation], [second.animation]);
    const presence = new OverlayPresence();
    presence.show();
    presence.hide(el);
    presence.show();
    presence.hide(el);
    first.settle();
    await flush();
    expect(presence.rendered).toBe(true);
    second.settle();
    await flush();
    expect(presence.rendered).toBe(false);
  });

  it("ignores endless animations and bounds a wait whose transitions never run", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const spinner = transition(Infinity);
    const stuck = transition();
    const onlySpinner = vi.fn();
    const hidden = vi.fn();
    afterExit(surface([spinner.animation]), 160, onlySpinner);
    afterExit(surface([spinner.animation, stuck.animation]), 160, hidden);
    await vi.advanceTimersByTimeAsync(160);
    expect(onlySpinner).toHaveBeenCalledTimes(1);
    expect(hidden).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1000);
    expect(hidden).toHaveBeenCalledTimes(1);
  });
});
