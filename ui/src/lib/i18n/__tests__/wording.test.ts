import { describe, expect, it } from "vitest";
import { en } from "../en.js";
import { zhCN } from "../zh-CN.js";

/** Sections a listener never sees on a normal page: diagnostics and the synthetic mock notice. */
const TECHNICAL_SECTIONS = new Set(["dialog", "mock"]);

const EN_IMPLEMENTATION_WORDS =
  /capabilit|authoritative|\bbatch|bridge|main process|revision|occurrence|anchor|epoch|by default|\d+ seconds|\bverified\b|\bare disabled\b|confirmed to match/i;
const ZH_IMPLEMENTATION_WORDS = /能力|为准|一批|桥接|主进程|修订|锚点|写操作|默认关闭|\d+\s*秒内|核验|已禁用|确认一致/;

/** Drops {placeholder} names so a placeholder such as {verified} is not read as a word. */
function withoutPlaceholders(text: string): string {
  return text.replace(/\{\w+\}/g, "{}");
}

/** Flattens a message tree into dotted-key / value pairs, skipping technical sections. */
function listenerStrings(tree: object, prefix = ""): Array<[string, string]> {
  const out: Array<[string, string]> = [];
  for (const [key, value] of Object.entries(tree)) {
    if (!prefix && TECHNICAL_SECTIONS.has(key)) continue;
    const path = prefix ? `${prefix}.${key}` : key;
    if (typeof value === "string") out.push([path, value]);
    else if (value && typeof value === "object") out.push(...listenerStrings(value, path));
  }
  return out;
}

describe("listener-facing wording", () => {
  it("keeps implementation words out of English strings", () => {
    const offenders = listenerStrings(en).filter(
      ([, text]) => EN_IMPLEMENTATION_WORDS.test(withoutPlaceholders(text))
    );
    expect(offenders).toEqual([]);
  });

  it("keeps implementation words out of Chinese strings", () => {
    const offenders = listenerStrings(zhCN).filter(
      ([, text]) => ZH_IMPLEMENTATION_WORDS.test(withoutPlaceholders(text))
    );
    expect(offenders).toEqual([]);
  });

  it("tells how many more tracks follow as a sentence, not a trailing counter", () => {
    expect(en.queue.laterHidden).not.toMatch(/:\s*\{count\}/);
    expect(zhCN.queue.laterHidden).not.toMatch(/未在此显示/);
  });

  it("says plainly that clearing a long queue may leave some upcoming tracks", () => {
    expect(en.queue.clearHint).toMatch(/long queue may keep some/);
    expect(en.queue.clearHint).not.toMatch(/shown here/);
    expect(zhCN.queue.clearHint).toContain("可能留下");
  });

  it("does not blame the player when the app itself could not pass a request on", () => {
    for (const text of [en.transport.noBridge, en.transport.refused]) expect(text).not.toMatch(/player/i);
    for (const text of [zhCN.transport.noBridge, zhCN.transport.refused]) expect(text).not.toContain("播放器");
  });
});
