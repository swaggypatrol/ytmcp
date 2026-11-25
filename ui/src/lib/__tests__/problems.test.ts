import { afterEach, describe, expect, it, vi } from "vitest";
import { PROBLEM_LOG_LIMIT, ProblemLog } from "../feedback/problems.svelte.js";
import { TOAST_DWELL_MS, ToastStore, type Toast } from "../feedback/toasts.svelte.js";

afterEach(() => {
  vi.useRealTimers();
});

const toast = (kind: string, tone: Toast["tone"], extra: Partial<Toast> = {}): Omit<Toast, "id"> => ({
  kind,
  tone,
  text: `${kind} happened`,
  ...extra,
});

describe("Problem log", () => {
  it("logs a warning or failure toast even when the next toast replaces it before it is seen", () => {
    const log = new ProblemLog();
    const store = new ToastStore(log);
    store.show(toast("write-failed", "danger", { problem: { subject: "Song A", technical: "TIMEOUT: no answer" } }));
    store.show(toast("write-done", "success"));
    store.show(toast("enqueued", "neutral"));
    expect(store.current?.kind).toBe("enqueued");
    expect(log.entries).toHaveLength(1);
    expect(log.entries[0]).toMatchObject({
      kind: "write-failed",
      text: "write-failed happened",
      subject: "Song A",
      technical: "TIMEOUT: no answer",
      unknownOutcome: false,
    });
  });

  it("logs a standing problem once per appearance, each one on its own", () => {
    const log = new ProblemLog();
    const store = new ToastStore(log);
    const offline: Toast = { id: "problem-offline", kind: "offline", tone: "warning", text: "Offline" };
    const refresh: Toast = { id: "refresh-home", kind: "refresh-failed", tone: "warning", text: "Couldn't refresh", detail: "BUSY: x" };
    store.trackStanding([offline, refresh]);
    store.trackStanding([offline, refresh]);
    expect(log.entries.map((e) => e.kind)).toEqual(["refresh-failed", "offline"]);
    expect(log.entries[0]!.technical).toBe("BUSY: x");
    store.trackStanding([null, refresh]);
    store.trackStanding([offline, refresh]);
    expect(log.entries.map((e) => e.kind)).toEqual(["offline", "refresh-failed", "offline"]);
    // A new scope starts empty and logs what is still standing.
    store.syncScope("epoch-2:acct");
    store.trackStanding([offline, refresh]);
    expect(log.entries.map((e) => e.kind)).toEqual(["refresh-failed", "offline"]);
  });

  it("keeps at most the limit, newest first, and an unchecked unknown write outlives the rest", () => {
    const log = new ProblemLog();
    const write = log.add({ toastId: "t-w", kind: "write-unknown", tone: "warning", text: "w", subject: "Song", advice: null, technical: null, unknownOutcome: true });
    for (let i = 0; i < PROBLEM_LOG_LIMIT + 5; i += 1) {
      log.add({ toastId: `t-${i}`, kind: "command-failed", tone: "danger", text: `f${i}`, subject: null, advice: null, technical: null, unknownOutcome: false });
    }
    expect(log.entries).toHaveLength(PROBLEM_LOG_LIMIT);
    expect(log.entries[0]!.text).toBe(`f${PROBLEM_LOG_LIMIT + 4}`);
    expect(log.entries.at(-1)!.id).toBe(write.id);

    log.markChecked(write.id);
    log.add({ toastId: "t-last", kind: "command-failed", tone: "danger", text: "last", subject: null, advice: null, technical: null, unknownOutcome: false });
    expect(log.entries).toHaveLength(PROBLEM_LOG_LIMIT);
    expect(log.entries.some((e) => e.id === write.id)).toBe(false);
  });

  it("starts empty for another account but keeps its entries while the core restarts", () => {
    const log = new ProblemLog();
    log.sync("acct:a:1");
    new ToastStore(log).show(toast("settings-failed", "danger"));
    expect(log.entries).toHaveLength(1);
    expect(log.sync("acct:a:1")).toBe(false);
    expect(log.sync(null)).toBe(false);
    expect(log.entries).toHaveLength(1);
    expect(log.sync("acct:b:2")).toBe(true);
    expect(log.entries).toEqual([]);
  });

  it("logs a problem raised in the same update as an account change under the new account", () => {
    const log = new ProblemLog();
    let scope = "acct:a:1";
    const store = new ToastStore(log, () => scope);
    store.syncScope(scope);
    store.show(toast("write-failed", "danger"));
    scope = "acct:b:2";
    store.show(toast("command-failed", "danger"));
    // The effect that follows the account catches up afterwards and changes nothing.
    store.syncScope(scope);
    expect(log.entries.map((e) => e.kind)).toEqual(["command-failed"]);
  });
});

describe("Toast countdown while the window is hidden", () => {
  it("holds while hidden, even through a release for a replaced toast, and resumes when shown", () => {
    vi.useFakeTimers();
    const store = new ToastStore();
    store.show(toast("failed", "danger"));
    vi.advanceTimersByTime(1_000);
    store.setHidden(true);
    store.release();
    vi.advanceTimersByTime(60_000);
    expect(store.current?.kind).toBe("failed");
    store.setHidden(false);
    vi.advanceTimersByTime(TOAST_DWELL_MS - 1_001);
    expect(store.current?.kind).toBe("failed");
    vi.advanceTimersByTime(1);
    expect(store.current).toBeNull();
  });

  it("gives a toast raised while hidden its full time once the window is back", () => {
    vi.useFakeTimers();
    const store = new ToastStore();
    store.setHidden(true);
    store.show(toast("command-failed", "danger"));
    store.hold("hover");
    store.release("hover");
    vi.advanceTimersByTime(60_000);
    expect(store.current?.kind).toBe("command-failed");
    store.setHidden(false);
    vi.advanceTimersByTime(TOAST_DWELL_MS - 1);
    expect(store.current?.kind).toBe("command-failed");
    vi.advanceTimersByTime(1);
    expect(store.current).toBeNull();
  });
});
