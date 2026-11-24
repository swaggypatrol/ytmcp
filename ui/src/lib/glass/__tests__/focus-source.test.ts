import { describe, expect, it } from "vitest";
import { trackFocusSource } from "../focus-source.js";

/** A fresh document with a search field and a chip, tracked from the start. */
function page(): { doc: Document; input: HTMLInputElement; chip: HTMLButtonElement } {
  const doc = document.implementation.createHTMLDocument("t");
  doc.body.innerHTML = `<button type="button">recent</button><label class="search-field"><input type="search" /></label>`;
  trackFocusSource(doc);
  trackFocusSource(doc);
  return { doc, input: doc.querySelector("input")!, chip: doc.querySelector("button")! };
}
const source = (doc: Document) => doc.documentElement.dataset.focusSource;

describe("focus source for the one focus ring (S-03)", () => {
  it("leaves the root unmarked until the first input, so the ring shows by default", () => {
    const { doc, input } = page();
    input.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    expect(source(doc)).toBeUndefined();
  });

  it("marks focus that follows a click as pointer, even when code moves it into the field", () => {
    const { doc, input, chip } = page();
    chip.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    input.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    expect(source(doc)).toBe("pointer");
  });

  it("keeps a clicked field quiet while the listener types, and rings it again on Tab", () => {
    const { doc, input, chip } = page();
    input.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    input.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "a", bubbles: true }));
    expect(source(doc)).toBe("pointer");
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    chip.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    expect(source(doc)).toBe("keyboard");
  });
});
