const tracked = new WeakSet<Document>();

/**
 * Records where focus came from as `data-focus-source="pointer" | "keyboard"` on the root, for
 * the app's one focus ring (styles/motion-a11y.css). Chromium matches :focus-visible on every
 * focused text field, even one focused by a click or by code right after a click, so the ring
 * alone cannot leave a clicked field quiet. The last input (pointer or key) is remembered and
 * written only when focus moves, so typing into a clicked field does not bring the ring in.
 * Until the first input the attribute is absent and the ring shows. Safe to call repeatedly.
 */
export function trackFocusSource(doc: Document = document): void {
  if (tracked.has(doc)) return;
  tracked.add(doc);
  let last: "pointer" | "keyboard" | null = null;
  const options = { capture: true, passive: true };
  doc.addEventListener("pointerdown", () => (last = "pointer"), options);
  doc.addEventListener("keydown", () => (last = "keyboard"), options);
  doc.addEventListener(
    "focusin",
    () => {
      if (last) doc.documentElement.dataset.focusSource = last;
    },
    options
  );
}
