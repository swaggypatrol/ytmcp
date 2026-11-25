import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { createRawSnippet } from "svelte";
import { afterEach, describe, expect, it } from "vitest";
import AccessibleDialog from "../AccessibleDialog.svelte";

afterEach(() => {
  cleanup();
  document.body.innerHTML = "";
});

/** A dialog body like Recent problems: an entry's button, then its Details disclosure with a control inside. */
function mountProblems() {
  const children = createRawSnippet(() => ({
    render: () =>
      `<div><button type="button" data-testid="entry-action">Open player</button>` +
      `<details data-testid="entry-details"><summary data-testid="entry-summary">Details</summary>` +
      `<button type="button" data-testid="entry-copy">Copy</button></details>` +
      `<p hidden><button type="button">Hidden</button></p></div>`,
  }));
  render(AccessibleDialog, { props: { open: true, title: "Diagnostics", onClose: () => {}, children } });
}

/** Presses Tab (or Shift+Tab) on the focused element; true when the trap moved focus itself. */
async function tab(shiftKey = false) {
  const event = new KeyboardEvent("keydown", { key: "Tab", shiftKey, bubbles: true, cancelable: true });
  await fireEvent(document.activeElement!, event);
  return event.defaultPrevented;
}

describe("dialog focus trap", () => {
  it("reaches a Details disclosure and still wraps at both ends", async () => {
    mountProblems();
    const close = screen.getByTestId("dialog-close-btn");
    const summary = screen.getByTestId("entry-summary");
    await waitFor(() => expect(document.activeElement).toBe(close));
    // Shift+Tab from the first control wraps to the last one: Details, not the control it hides.
    expect(await tab(true)).toBe(true);
    expect(document.activeElement).toBe(summary);
    // Shift+Tab from Details goes back the usual way, to the entry's button.
    expect(await tab(true)).toBe(false);
    // Tab from Details, the last control, wraps to the first.
    expect(await tab()).toBe(true);
    expect(document.activeElement).toBe(close);

    // Opened, its own control is the last stop.
    (screen.getByTestId("entry-details") as HTMLDetailsElement).open = true;
    expect(await tab(true)).toBe(true);
    expect(document.activeElement).toBe(screen.getByTestId("entry-copy"));
  });
});
