import { cleanup, render } from "@testing-library/svelte";
import { createRawSnippet } from "svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import AccessibleDialog from "../AccessibleDialog.svelte";

/** A 160 ms exit clock, and a leaving layer that reports one running transition the test ends. */
function stubExit() {
  const settles: (() => void)[] = [];
  const real = window.getComputedStyle.bind(window);
  vi.spyOn(window, "getComputedStyle").mockImplementation((el, pseudo) => {
    const style = real(el, pseudo);
    return new Proxy(style, {
      get: (target, key) =>
        key === "getPropertyValue"
          ? (name: string) => (name === "--lg-overlay-exit-duration" ? "160ms" : target.getPropertyValue(name))
          : Reflect.get(target, key, target),
    });
  });
  Object.defineProperty(Element.prototype, "getAnimations", {
    configurable: true,
    value(this: Element) {
      if (!this.classList.contains("app-dialog-layer") || !this.hasAttribute("data-leaving")) return [];
      const finished = new Promise<void>((resolve) => settles.push(resolve));
      return [{ playState: "running", effect: { getComputedTiming: () => ({ endTime: 160 }) }, finished }];
    },
  });
  return () => settles.splice(0).forEach((settle) => settle());
}

function mountInShell() {
  const shell = document.createElement("div");
  shell.className = "app-shell";
  document.body.append(shell);
  const children = createRawSnippet(() => ({ render: () => "<p>body</p>" }));
  const view = render(AccessibleDialog, { target: shell, props: { open: true, title: "Dialog", onClose: () => {}, children } });
  return { shell, ...view };
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
  delete (Element.prototype as { getAnimations?: unknown }).getAnimations;
  document.body.innerHTML = "";
});

describe("dialog exit", () => {
  it("leaves the shell as soon as its exit has run", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const settle = stubExit();
    const { shell, rerender } = mountInShell();
    await rerender({ open: false });
    const layer = shell.querySelector<HTMLElement>(".app-dialog-layer")!;
    expect(layer.dataset.leaving).toBe("dismiss");
    settle();
    await vi.advanceTimersByTimeAsync(0);
    expect(shell.querySelector(".app-dialog-layer")).toBeNull();
  });

  it("finishes its exit when the page holding it goes mid-exit", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const settle = stubExit();
    const { shell, rerender, unmount } = mountInShell();
    await rerender({ open: false });
    unmount();
    // Still on screen and still leaving: removing it now would cut the dissolve short.
    const layer = shell.querySelector<HTMLElement>(".app-dialog-layer");
    expect(layer?.dataset.leaving).toBe("dismiss");
    settle();
    await vi.advanceTimersByTimeAsync(0);
    expect(shell.querySelector(".app-dialog-layer")).toBeNull();
  });
});
