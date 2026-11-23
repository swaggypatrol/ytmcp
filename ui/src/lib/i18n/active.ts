import { en, type Messages } from "./en.js";
import { zhCN } from "./zh-CN.js";

/**
 * The current language's strings for plain modules such as the IPC client, which must not depend
 * on Svelte's reactivity. `setLocale` in `index.svelte.ts` keeps it in step; components use `t`.
 */
let active: Messages = en;

export function activeMessages(): Messages {
  return active;
}

export function setActiveMessages(locale: string): void {
  active = locale === "zh-CN" ? zhCN : en;
}
