import type { QueryErrorCode, RecentSearchesRequest, RecentSearchesSnapshot } from "@ytmcp/protocol";
import { describe, expect, it } from "vitest";
import { CoreRequestError } from "../ipc/client.js";
import { MAX_RECENT_QUERY_CHARS, RecentSearches, recentSearchQuery } from "../recent-searches.svelte.js";

interface HeldCall {
  readonly req: RecentSearchesRequest;
  readonly epoch: string;
  reply(items: string[]): Promise<void>;
  fail(code: QueryErrorCode): Promise<void>;
}

/** A core whose replies the test sends, one call at a time. */
function heldCore() {
  const calls: HeldCall[] = [];
  const source = {
    recentSearches: (req: RecentSearchesRequest, epoch: string) =>
      new Promise<RecentSearchesSnapshot>((resolve, reject) => {
        const settle = async (run: () => void) => {
          run();
          await Promise.resolve();
          await Promise.resolve();
        };
        calls.push({
          req,
          epoch,
          reply: (items) => settle(() => resolve({ items: items.map((query) => ({ query, recordedAtMs: 1 })) })),
          fail: (code) =>
            settle(() => reject(new CoreRequestError({ code, retriable: false, retryAfterMs: null, safeMessage: code, requestId: null }))),
        });
      }),
  };
  return { calls, store: new RecentSearches(source) };
}

describe("Recent searches cache of the core's list", () => {
  it("asks for the list when an account is signed in and shows the reply", async () => {
    const { calls, store } = heldCore();
    store.sync(null);
    expect(calls).toHaveLength(0);
    store.sync("epoch-1");
    store.sync("epoch-1");
    expect(calls.map((c) => [c.req, c.epoch])).toEqual([[{ action: "list" }, "epoch-1"]]);
    await calls[0]!.reply(["glass", "hanbit"]);
    expect(store.items).toEqual(["glass", "hanbit"]);
  });

  it("replaces the list with the reply to a record or a clear, never with an older reply", async () => {
    const { calls, store } = heldCore();
    store.sync("epoch-1");
    store.record("  Glass  ");
    expect(calls[1]!.req).toEqual({ action: "record", query: "Glass" });
    await calls[1]!.reply(["Glass", "older"]);
    // The list asked first answers last: it is older than what is shown.
    await calls[0]!.reply(["older"]);
    expect(store.items).toEqual(["Glass", "older"]);

    store.clear();
    expect(calls[2]!.req).toEqual({ action: "clear" });
    await calls[2]!.reply([]);
    expect(store.items).toEqual([]);
  });

  it("empties at once for another account and drops the late reply for the first one", async () => {
    const { calls, store } = heldCore();
    store.sync("epoch-1");
    await calls[0]!.reply(["first account"]);
    store.record("pending");
    store.sync("epoch-2");
    expect(store.items).toEqual([]);
    await calls[1]!.reply(["pending", "first account"]);
    expect(store.items).toEqual([]);
    expect(calls[2]).toMatchObject({ req: { action: "list" }, epoch: "epoch-2" });
    await calls[2]!.reply(["second account"]);
    expect(store.items).toEqual(["second account"]);
    store.sync(null);
    expect(store.items).toEqual([]);
  });

  it("asks again after a refused sign-in once the account's epoch appears", async () => {
    const { calls, store } = heldCore();
    store.sync("epoch-early");
    await calls[0]!.fail("AUTH_REQUIRED");
    expect(store.items).toEqual([]);
    store.sync("epoch-1");
    expect(calls[1]).toMatchObject({ req: { action: "list" }, epoch: "epoch-1" });
    await calls[1]!.reply(["now signed in"]);
    expect(store.items).toEqual(["now signed in"]);
  });

  it("on another account's epoch asks only for the new account's list and replays nothing", async () => {
    const { calls, store } = heldCore();
    store.sync("epoch-1");
    await calls[0]!.reply(["mine"]);
    store.record("typed under the old epoch");
    await calls[1]!.fail("ACCOUNT_MISMATCH");
    expect(calls).toHaveLength(2);
    store.sync("epoch-2");
    expect(calls.slice(2).map((c) => c.req)).toEqual([{ action: "list" }]);
  });

  it("keeps the last list when the core is busy, its storage failed or the write lost a race", async () => {
    const { calls, store } = heldCore();
    store.sync("epoch-1");
    await calls[0]!.reply(["kept"]);
    for (const code of ["STALE_CONTEXT", "STORAGE_ERROR", "BUSY"] as const) {
      store.record(code.toLowerCase());
      await calls.at(-1)!.fail(code);
      expect(store.items).toEqual(["kept"]);
    }
  });

  it("empties on an account-state invalidation without a clear, keeping the new account's list", async () => {
    const { calls, store } = heldCore();
    store.sync("epoch-1");
    await calls[0]!.reply(["first account"]);
    // The new epoch's list is on its way when the invalidation lands.
    store.sync("epoch-2");
    store.invalidate();
    expect(store.items).toEqual([]);
    await Promise.resolve();
    expect(calls.every((c) => c.req.action === "list")).toBe(true);
    for (const call of calls.filter((c) => c.epoch === "epoch-2")) await call.reply(["second account"]);
    expect(store.items).toEqual(["second account"]);

    // A sign-out right after the invalidation: nothing is asked for.
    const sent = calls.length;
    store.invalidate();
    store.sync(null);
    await Promise.resolve();
    expect(calls).toHaveLength(sent);
    expect(store.items).toEqual([]);
  });

  it("sends queries normalised and never an empty or overlong one", () => {
    const { calls, store } = heldCore();
    store.sync("epoch-1");
    store.record("   ");
    store.record("x".repeat(MAX_RECENT_QUERY_CHARS + 1));
    // 200 code points of an astral character is 400 UTF-16 units and still allowed.
    store.record("😀".repeat(MAX_RECENT_QUERY_CHARS));
    store.record("Café");
    expect(calls.slice(1).map((c) => c.req)).toEqual([
      { action: "record", query: "😀".repeat(MAX_RECENT_QUERY_CHARS) },
      { action: "record", query: "Café" },
    ]);
    expect(recentSearchQuery(` ${"y".repeat(MAX_RECENT_QUERY_CHARS)} `)).toBe("y".repeat(MAX_RECENT_QUERY_CHARS));
  });

  it("keeps working with a source that throws or is missing", async () => {
    const throwing = new RecentSearches({
      recentSearches: () => {
        throw new Error("unavailable");
      },
    });
    throwing.sync("epoch-1");
    throwing.record("q");
    await Promise.resolve();
    expect(throwing.items).toEqual([]);
    const none = new RecentSearches(null);
    none.sync("epoch-1");
    none.record("q");
    expect(none.items).toEqual([]);
  });
});
