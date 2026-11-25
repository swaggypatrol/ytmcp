import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import ToastHost from "../ToastHost.svelte";
import { ToastStore } from "../../feedback/toasts.svelte.js";

/** Leaving toasts report one running exit transition whose end the test decides. */
function stubExitTransitions() {
  const settles: (() => void)[] = [];
  Object.defineProperty(Element.prototype, "getAnimations", {
    configurable: true,
    value(this: Element) {
      if (!this.classList.contains("toast") || !this.hasAttribute("data-leaving")) return [];
      const finished = new Promise<void>((resolve) => settles.push(resolve));
      return [{ playState: "running", effect: { getComputedTiming: () => ({ endTime: 160 }) }, finished }];
    },
  });
  return settles;
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  delete (Element.prototype as { getAnimations?: unknown }).getAnimations;
});

describe("toast exit", () => {
  it("keeps a replaced toast in its cell until its exit has run, then removes only that one", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    const settles = stubExitTransitions();
    const store = new ToastStore();
    render(ToastHost, { props: { store, sticky: null } });
    store.show({ kind: "first", tone: "neutral", text: "first" });
    await vi.advanceTimersByTimeAsync(0);
    store.show({ kind: "second", tone: "neutral", text: "second" });
    await vi.advanceTimersByTimeAsync(0);
    const region = screen.getByTestId("toast-region");
    expect(region.children).toHaveLength(2);
    // Past the 160 ms clock the old toast is still leaving: a late start must not be cut short.
    await vi.advanceTimersByTimeAsync(400);
    expect(region.children).toHaveLength(2);
    settles.forEach((settle) => settle());
    await vi.advanceTimersByTimeAsync(0);
    expect(region.children).toHaveLength(1);
    expect(screen.getByTestId("toast-second")).toBeTruthy();
  });

  it("drops a pending removal when the host unmounts mid-exit", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    const settles = stubExitTransitions();
    const store = new ToastStore();
    const { unmount } = render(ToastHost, { props: { store, sticky: null } });
    store.show({ kind: "first", tone: "neutral", text: "first" });
    await vi.advanceTimersByTimeAsync(0);
    store.dismiss();
    await vi.advanceTimersByTimeAsync(0);
    unmount();
    settles.forEach((settle) => settle());
    await expect(vi.advanceTimersByTimeAsync(2000)).resolves.not.toThrow();
  });
});
