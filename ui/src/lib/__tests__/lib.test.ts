import {
  FakeCoreClient,
  createSyntheticTracks,
  type CatalogQueryRequest,
  type CatalogResponsePayload,
  type CommandReceipt,
  type LyricsResult,
} from "@ytm-glass/protocol";
import { describe, expect, it } from "vitest";
import { commandGate, writeGate } from "../capabilities.js";
import { CatalogStore, catalogAccountKey } from "../catalog/catalog.svelte.js";
import { resolvePageView } from "../catalog/page-state.js";
import { coverSrc } from "../cover.js";
import { activeLyricIndex, effectiveLineTimeMs, stepUserOffsetMs } from "../lyrics.js";
import { shouldRunDecorativeAnimation } from "../motion/preferences.js";
import { commitSeekPreview, reconcileSeekPreview, SEEK_PREVIEW_TIMEOUT_MS, startSeekPreview } from "../seek-preview.js";
import { resolveShortcut } from "../shortcuts.js";
import { accessibleTrackAccent } from "../theme/accent.js";

const el = (tagName: string, extra: Record<string, unknown> = {}) =>
  ({ tagName, isContentEditable: false, closest: () => null, ...extra }) as unknown as EventTarget;

describe("resolveShortcut", () => {
  it("maps SPEC 8.2 keys and leaves text entry, IME composition and native controls alone", () => {
    const body = el("BODY");
    expect(resolveShortcut({ key: " ", code: "Space", target: body }, false)).toBe("togglePlay");
    expect(resolveShortcut({ key: "ArrowRight", ctrlKey: true, target: body }, false)).toBe("next");
    expect(resolveShortcut({ key: "ArrowLeft", ctrlKey: true, target: body }, false)).toBe("previous");
    expect(resolveShortcut({ key: "Escape", target: body }, false)).toBe("escape");

    const input = el("INPUT");
    expect(resolveShortcut({ key: " ", code: "Space", target: input }, false)).toBeNull();
    expect(resolveShortcut({ key: "ArrowLeft", ctrlKey: true, target: input }, false)).toBeNull();
    expect(resolveShortcut({ key: "F", ctrlKey: true, target: input }, false)).toBe("focusSearch");
    expect(resolveShortcut({ key: "l", ctrlKey: true, target: input }, false)).toBe("openLyrics");
    expect(resolveShortcut({ key: " ", target: el("DIV", { isContentEditable: true }) }, false)).toBeNull();

    expect(resolveShortcut({ key: " ", code: "Space", target: body }, true)).toBeNull();
    expect(resolveShortcut({ key: "f", ctrlKey: true, isComposing: true, target: body }, false)).toBeNull();
    expect(resolveShortcut({ key: "Process", keyCode: 229, target: body }, false)).toBeNull();
    expect(resolveShortcut({ key: " ", code: "Space", target: el("BUTTON") }, false)).toBeNull();
  });
});

describe("seek preview reconcile", () => {
  const receipt = (status: CommandReceipt["status"]): CommandReceipt => ({
    commandId: "seek-1",
    op: "seek",
    status,
    acceptedAtMs: 0,
    observedAtMs: null,
    deduplicated: false,
    error: null,
  });

  it("holds a committed preview until its own terminal receipt, track change or timeout", () => {
    const started = startSeekPreview(250_000, 200_000, "ti-1");
    expect(started.positionMs).toBe(200_000);
    const committed = commitSeekPreview(started, 1_000, "seek-1");

    expect(reconcileSeekPreview(committed, "ti-1", receipt("dispatched"), 1_500)).toBe(committed);
    expect(reconcileSeekPreview(committed, "ti-1", { ...receipt("failed"), commandId: "other" }, 1_500)).toBe(committed);
    for (const status of ["observed", "failed", "indeterminate", "cancelled"] as const) {
      expect(reconcileSeekPreview(committed, "ti-1", receipt(status), 1_500)).toBeNull();
    }
    expect(reconcileSeekPreview(committed, "ti-2", null, 1_500)).toBeNull();
    expect(reconcileSeekPreview(committed, "ti-1", null, 1_000 + SEEK_PREVIEW_TIMEOUT_MS)).toBeNull();
    // While dragging (not committed) there is no timeout.
    expect(reconcileSeekPreview(started, "ti-1", null, 10 ** 9)).toBe(started);
  });
});

describe("lyrics timing", () => {
  const lyrics: LyricsResult = {
    videoId: "v",
    kind: "synced",
    source: "lrclib",
    language: null,
    matchBasis: null,
    fetchedAtMs: 0,
    fileOffsetMs: 100,
    userOffsetMs: 400,
    syncedLines: [
      { timeMs: 10_000, text: "a" },
      { timeMs: 20_000, text: "b" },
      { timeMs: 20_000, text: "b2" },
      { timeMs: 30_000, text: "c" },
    ],
    plainText: null,
    error: null,
  };

  it("applies file and user offsets like Rust effective_line_time_ms and clamps user offset", () => {
    // 10 000 - 100 (file) + 400 (user)
    expect(activeLyricIndex(lyrics, 10_299)).toBe(-1);
    expect(activeLyricIndex(lyrics, 10_300)).toBe(0);
    expect(activeLyricIndex(lyrics, 20_300)).toBe(2);
    expect(activeLyricIndex({ ...lyrics, kind: "plain" }, 99_000)).toBe(-1);
    expect(stepUserOffsetMs(9_800, 500)).toBe(10_000);
    expect(stepUserOffsetMs(-9_800, -500)).toBe(-10_000);
  });

  it("shows lyrics earlier for a positive LRC [offset:] tag and later for a positive user offset", () => {
    const line = { timeMs: 10_000, text: "a" };
    // LRC convention: [offset:+500] means the cue is shown 500 ms before its tag time.
    expect(effectiveLineTimeMs(line, { fileOffsetMs: 500, userOffsetMs: 0 })).toBe(9_500);
    expect(effectiveLineTimeMs(line, { fileOffsetMs: -500, userOffsetMs: 0 })).toBe(10_500);
    // The "later" button steps the user offset up and delays the line; "earlier" steps it down.
    expect(effectiveLineTimeMs(line, { fileOffsetMs: 0, userOffsetMs: stepUserOffsetMs(0, 500) })).toBe(10_500);
    expect(effectiveLineTimeMs(line, { fileOffsetMs: 0, userOffsetMs: stepUserOffsetMs(0, -500) })).toBe(9_500);
    expect(effectiveLineTimeMs({ timeMs: 100, text: "a" }, { fileOffsetMs: 500, userOffsetMs: 0 })).toBe(0);
  });
});

describe("track accent", () => {
  // OKLab lightness is the cube root of relative luminance for achromatic colors.
  const contrast = (l1: number, l2: number) => {
    const [a, b] = [l1 ** 3, l2 ** 3].sort((x, y) => y - x) as [number, number];
    return (a + 0.05) / (b + 0.05);
  };
  const lightness = (css: string) => Number(/oklch\(([0-9.]+)/.exec(css)![1]);

  it("clamps cover accents into readable bands on both schemes and rejects malformed input", () => {
    const dark = accessibleTrackAccent("oklch(0.2 0.4 240)", "dark")!;
    expect(lightness(dark.accent)).toBeGreaterThanOrEqual(0.72);
    expect(dark.accent).toContain(" 0.200 ");
    expect(contrast(lightness(dark.accent), 0.135)).toBeGreaterThanOrEqual(4.5);
    expect(contrast(lightness(dark.accent), lightness(dark.onAccent))).toBeGreaterThanOrEqual(4.5);

    const light = accessibleTrackAccent("oklch(92% 0.1 90)", "light")!;
    expect(lightness(light.accent)).toBeLessThanOrEqual(0.48);
    expect(contrast(lightness(light.accent), 0.975)).toBeGreaterThanOrEqual(4.5);
    expect(contrast(lightness(light.accent), lightness(light.onAccent))).toBeGreaterThanOrEqual(4.5);

    expect(accessibleTrackAccent("red", "dark")).toBeNull();
    expect(accessibleTrackAccent(null, "dark")).toBeNull();
  });

  it("runs decorative motion only while playing, visible and not reduced", () => {
    const base = { reduceMotionSetting: false, prefersReducedMotion: false, documentHidden: false, playbackStatus: "playing" as const };
    expect(shouldRunDecorativeAnimation(base)).toBe(true);
    expect(shouldRunDecorativeAnimation({ ...base, reduceMotionSetting: true })).toBe(false);
    expect(shouldRunDecorativeAnimation({ ...base, prefersReducedMotion: true })).toBe(false);
    expect(shouldRunDecorativeAnimation({ ...base, documentHidden: true })).toBe(false);
    expect(shouldRunDecorativeAnimation({ ...base, playbackStatus: "paused" })).toBe(false);
  });
});

describe("page state and gates", () => {
  const base = { status: "done" as const, error: null, hasData: true, isEmpty: false, stale: false };

  it("resolves each SPEC 8.2 page state", () => {
    expect(resolvePageView({ ...base, status: "loading", hasData: false }).kind).toBe("loading");
    expect(resolvePageView({ ...base, isEmpty: true }).kind).toBe("empty");
    expect(resolvePageView({ ...base, stale: true })).toMatchObject({ kind: "ready", cached: true });
    expect(resolvePageView({ ...base, network: "offline" })).toMatchObject({ kind: "ready", cached: true });
    expect(resolvePageView({ ...base, hasData: false, network: "offline" }).kind).toBe("offline");
    expect(resolvePageView({ ...base, capability: "unsupported" }).kind).toBe("unsupported");
    expect(resolvePageView({ ...base, accountScoped: true, authState: "expired" }).kind).toBe("authExpired");
    expect(resolvePageView({ ...base, authState: "expired" }).kind).toBe("ready");
    // Signed out, the core reports reads unsupported: sign-in is the actionable state.
    expect(resolvePageView({ ...base, capability: "unsupported", authState: "signedOut" }).kind).toBe("authExpired");
    const err = (code: "AUTH_EXPIRED" | "NETWORK_OFFLINE" | "UNSUPPORTED_CAPABILITY" | "TIMEOUT", hasData = false) =>
      resolvePageView({ ...base, hasData, error: { code, retriable: false, safeMessage: "x" } });
    expect(err("AUTH_EXPIRED").kind).toBe("authExpired");
    expect(err("NETWORK_OFFLINE").kind).toBe("offline");
    expect(err("UNSUPPORTED_CAPABILITY").kind).toBe("unsupported");
    expect(err("TIMEOUT").kind).toBe("error");
    // A failed refresh keeps earlier content visible as cached.
    expect(err("NETWORK_OFFLINE", true)).toMatchObject({ kind: "ready", cached: true });
    expect(err("TIMEOUT", true)).toMatchObject({ kind: "ready", cached: true });
    expect(err("AUTH_EXPIRED", true).kind).toBe("authExpired");
  });

  it("only enables writes for a matched, authenticated account with a supported capability", () => {
    const auth = { state: "authenticated" as const, accountEpoch: "e", playerCatalogMatched: true };
    expect(writeGate("supported", auth).enabled).toBe(true);
    expect(writeGate("unverified", auth).enabled).toBe(false);
    expect(writeGate("supported", { ...auth, playerCatalogMatched: false }).enabled).toBe(false);
    expect(writeGate("supported", { ...auth, state: "expired" }).enabled).toBe(false);
  });

  it("gates player commands when the bridge is disconnected or incompatible even if capabilities remain stale", () => {
    expect(commandGate("supported", "ready")).toEqual({ enabled: true, reason: null });
    expect(commandGate("unverified", "ready").enabled).toBe(true);
    expect(commandGate("unverified", "ready").reason).toContain("not verified yet");
    expect(commandGate("unsupported", "ready").enabled).toBe(false);
    expect(commandGate("supported", "disconnected").enabled).toBe(false);
    expect(commandGate("supported", "disconnected").reason).toContain("not connected");
    expect(commandGate("supported", "incompatible").enabled).toBe(false);
  });

  it("builds cover URLs only from validated asset ids", () => {
    expect(coverSrc({ assetId: "a1_B-2" })).toBe("app://covers/a1_B-2");
    expect(coverSrc({ assetId: "../etc/passwd" })).toBeNull();
    expect(coverSrc({ assetId: null, remoteUrl: "https://x/y.jpg" })).toBeNull();
  });
});

describe("CatalogStore", () => {
  function deferredClient() {
    const pending: { req: CatalogQueryRequest; resolve: (r: CatalogResponsePayload) => void }[] = [];
    return {
      pending,
      client: {
        queryCatalog: (req: CatalogQueryRequest) =>
          new Promise<CatalogResponsePayload>((resolve) => pending.push({ req, resolve })),
      },
    };
  }
  const fake = new FakeCoreClient("normal");

  it("drops responses from older searches and from a previous account", async () => {
    const { client, pending } = deferredClient();
    const store = new CatalogStore(client);
    store.syncAccount(catalogAccountKey({ state: "authenticated", accountEpoch: "e1", accountScope: "s1", playerCatalogMatched: true }));

    const first = store.runSearch("old");
    const second = store.runSearch("new");
    pending[1]!.resolve(fake.queryCatalog({ kind: "searchSongs", query: "Neon" }));
    pending[0]!.resolve(fake.queryCatalog({ kind: "searchSongs", query: "夜" } /* a CJK query against the invented 夜の硝子 titles */));
    await Promise.all([first, second]);
    expect(store.search.key).toBe("new");
    expect(store.search.items.every((t) => t.title.includes("Neon"))).toBe(true);

    const library = store.loadLibrary();
    expect(store.syncAccount(catalogAccountKey({ state: "authenticated", accountEpoch: "e2", accountScope: "s1", playerCatalogMatched: true }))).toBe(true);
    expect(store.search.items).toHaveLength(0);
    pending[2]!.resolve(fake.queryCatalog({ kind: "libraryPlaylists" }));
    await library;
    expect(store.library.items).toHaveLength(0);
    expect(store.library.status).toBe("idle");
  });

  it("appends pages with the opaque cursor and keeps the playlist bound to its id", async () => {
    const store = new CatalogStore({ queryCatalog: async (req) => fake.queryCatalog(req) });
    store.syncAccount("acct:s:e");
    await store.openPlaylist("mock-pl-favorites");
    expect(store.playlist.items).toHaveLength(100);
    expect(store.playlist.nextCursor).toBe("100");
    await store.loadMorePlaylist();
    expect(store.playlist.items).toHaveLength(200);
    expect(store.playlist.items[100]!.videoId).toBe(createSyntheticTracks(101, "mock")[100]!.videoId);

    await store.openPlaylist("mock-pl-late-night");
    expect(store.playlist.key).toBe("mock-pl-late-night");
    expect(store.playlist.items).toHaveLength(100);
  });

  it("drops old Home and Library replies after a playlist membership change", async () => {
    const { client, pending } = deferredClient();
    const store = new CatalogStore(client);
    store.syncAccount("acct:s:e");
    const oldLibrary = fake.queryCatalog({ kind: "libraryPlaylists" });
    if (oldLibrary.resultType !== "libraryPlaylists") throw new Error("missing Library fixture");
    const first = store.loadLibrary();
    pending[0]!.resolve({ ...oldLibrary, nextCursor: "old-window" });
    await first;

    const more = store.loadMoreLibrary();
    expect(pending[1]!.req.cursor).toBe("old-window");
    const home = store.loadHome();
    const search = store.runSearch("Neon");
    expect(store.invalidate("songMembership", "mock-vid-00001")).toEqual([]);
    expect(store.library.nextCursor).toBe("old-window");
    expect(store.invalidate("playlistMembership", "mock-pl-favorites")).toEqual(["home", "library"]);
    expect(store.library.items).toEqual([]);
    expect(store.library.nextCursor).toBeNull();

    const refreshed = store.loadLibrary();
    pending[4]!.resolve({ ...oldLibrary, items: [], nextCursor: null });
    await refreshed;
    pending[1]!.resolve(oldLibrary);
    pending[2]!.resolve(fake.queryCatalog({ kind: "home" }));
    pending[3]!.resolve(fake.queryCatalog({ kind: "searchSongs", query: "Neon" }));
    await Promise.all([more, home, search]);
    expect(store.library.items).toEqual([]);
    expect(store.library.status).toBe("done");
    expect(store.home.detail).toBeNull();
    expect(store.search.status).toBe("done");
    expect(store.search.items.length).toBeGreaterThan(0);
  });
});
