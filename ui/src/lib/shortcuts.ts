/** Global keyboard actions defined by SPEC 8.2. */
export type ShortcutAction = "togglePlay" | "focusSearch" | "toggleLyrics" | "previous" | "next" | "escape";

export interface ShortcutKeyEvent {
  readonly key: string;
  readonly code?: string;
  readonly ctrlKey?: boolean;
  readonly metaKey?: boolean;
  readonly altKey?: boolean;
  readonly shiftKey?: boolean;
  readonly isComposing?: boolean;
  readonly keyCode?: number;
  readonly target?: EventTarget | null;
}

interface ElementLike {
  readonly tagName?: string;
  readonly isContentEditable?: boolean;
  closest?(selector: string): unknown;
}

function asElement(target: EventTarget | null | undefined): ElementLike | null {
  if (!target || typeof target !== "object") return null;
  return target as unknown as ElementLike;
}

/** True when the target consumes typed text (inputs, selects, contenteditable). */
export function isTextEntryTarget(target: EventTarget | null | undefined): boolean {
  const el = asElement(target);
  if (!el) return false;
  const tag = (el.tagName ?? "").toUpperCase();
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
  if (el.isContentEditable) return true;
  return typeof el.closest === "function" && el.closest('[contenteditable=""], [contenteditable="true"]') != null;
}

/** True when Space already has a native meaning on the target (activating a control). */
function isNativeSpaceTarget(target: EventTarget | null | undefined): boolean {
  const el = asElement(target);
  if (!el) return false;
  const tag = (el.tagName ?? "").toUpperCase();
  if (tag === "BUTTON" || tag === "A" || tag === "SUMMARY") return true;
  return typeof el.closest === "function" && el.closest('[role="button"], [role="option"], [role="menuitem"]') != null;
}

/**
 * Maps a keydown to a global action, or null when the key must be left alone:
 * during IME composition, inside text entry (except Ctrl+F / Ctrl+L), or when Space
 * would activate a focused control.
 */
export function resolveShortcut(event: ShortcutKeyEvent, imeComposing: boolean): ShortcutAction | null {
  if (imeComposing || event.isComposing || event.key === "Process" || event.keyCode === 229) {
    return null;
  }
  if (event.altKey) return null;

  const mod = Boolean(event.ctrlKey || event.metaKey);
  const key = event.key.length === 1 ? event.key.toLowerCase() : event.key;

  if (mod && !event.shiftKey && key === "f") return "focusSearch";
  if (mod && !event.shiftKey && key === "l") return "toggleLyrics";

  if (isTextEntryTarget(event.target)) return null;

  if (key === "Escape") return "escape";
  if (mod && key === "ArrowLeft") return "previous";
  if (mod && key === "ArrowRight") return "next";
  if (!mod && (event.code === "Space" || key === " ") && !isNativeSpaceTarget(event.target)) {
    return "togglePlay";
  }
  return null;
}
