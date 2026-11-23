import type { UiLanguage } from "@ytmcp/protocol";
import { en, type Messages } from "./en.js";
import { zhCN } from "./zh-CN.js";
import { setActiveMessages } from "./active.js";

export type { Messages, NavView } from "./en.js";

/** UI locales are the settings' `uiLanguage` values. */
export type Locale = UiLanguage;
export const LOCALES: readonly Locale[] = ["en", "zh-CN"];

/** Each language named in itself, for the language picker. */
export const LOCALE_NAMES: Record<Locale, string> = { en: "English", "zh-CN": "简体中文" };

const CATALOGS: Record<Locale, Messages> = { en, "zh-CN": zhCN };

/**
 * The language shown. Core's settings own the choice: App applies `settings.uiLanguage` from each
 * snapshot, and the picker writes it through the settings update. Until the first snapshot
 * arrives the UI is English.
 */
let current = $state<Locale>("en");

export function getLocale(): Locale {
  return current;
}

/** Shows the UI in `next` and updates the document's `lang`. Does not change the setting. */
export function setLocale(next: Locale): void {
  current = LOCALES.includes(next) ? next : "en";
  setActiveMessages(current);
  if (typeof document !== "undefined") document.documentElement.lang = current;
}

/**
 * Strings for the current language. Reads go through the reactive locale, so templates and
 * `$derived` values update when it changes; a value copied out once does not.
 */
export const t: Messages = new Proxy({} as Messages, {
  get: (_, key) => CATALOGS[current][key as keyof Messages],
});
