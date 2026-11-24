import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { FakeCoreClient, type AppSnapshot, type LyricsResult } from "@ytmcp/protocol";
import { afterEach, describe, expect, it, vi } from "vitest";
import { codecFamily } from "../../audio-quality.js";
import { en } from "../../i18n/en.js";
import { setLocale } from "../../i18n/index.svelte.js";
import { zhCN } from "../../i18n/zh-CN.js";
import NowPlayingView from "../NowPlayingView.svelte";
import PlayerBar from "../PlayerBar.svelte";
import QualityBadge from "../QualityBadge.svelte";
import TrackActions, { placeMenu } from "../TrackActions.svelte";

afterEach(() => {
  cleanup();
  setLocale("en");
  vi.useRealTimers();
});

const base: AppSnapshot = new FakeCoreClient("normal").subscribeSnapshot(() => {}).snapshot;
const idle = { pendingCount: 0, activeOp: null, desiredPlayback: null, desiredPositionMs: null, desiredVolume: null, lastError: null };

/** The player reports what the official page's statistics show, including its video label. */
function withMedia(snapshot: AppSnapshot, playback: AppSnapshot["player"]["playback"] = "playing"): AppSnapshot {
  return {
    ...snapshot,
    player: { ...snapshot.player, playback, codec: "mp4a.40.2 (140)", audioItag: 140, audioMode: "song", qualityLabel: "auto" },
  };
}

function renderBar(snapshot: AppSnapshot, extra: Record<string, unknown> = {}) {
  return render(PlayerBar, {
    props: {
      snapshot,
      pendingOverlay: idle,
      interpolatedPositionMs: 0,
      onCommand: vi.fn(async () => null),
      onOpenNowPlaying: () => {},
      onToggleLyrics: () => {},
      ...extra,
    },
  });
}

describe("Audio quality", () => {
  it("names the codec family in plain words", () => {
    expect(codecFamily("mp4a.40.2 (140)")).toBe("AAC");
    expect(codecFamily("opus (251)")).toBe("Opus");
    expect(codecFamily("something-new")).toBeNull();
    expect(codecFamily(null)).toBeNull();
  });

  it("draws the page's own label as a framed mark that is not a control, and nothing while the page has not said", () => {
    render(QualityBadge, { props: { quality: "high", codec: "mp4a.40.2", bitrateKbps: 258, size: "lg" } });
    const badge = screen.getByTestId("quality-badge");
    expect(badge.textContent?.trim()).toBe("HIGH");
    expect(badge.title).toBe("High · AAC · 258 kbps");
    expect(badge.tagName).toBe("SPAN");
    expect(badge.hasAttribute("tabindex")).toBe(false);
    cleanup();

    render(QualityBadge, { props: { quality: "standard", codec: "opus" } });
    expect(screen.getByTestId("quality-badge").textContent?.trim()).toBe("STD");
    expect(screen.getByTestId("quality-badge").title).toBe("Standard · Opus");
    cleanup();

    render(QualityBadge, { props: { quality: "low", codec: "opus", bitrateKbps: 50 } });
    expect(screen.getByTestId("quality-badge").textContent?.trim()).toBe("LOW");
    cleanup();

    // A stream number alone says nothing: without the page's label there is no badge.
    render(QualityBadge, { props: { quality: null, codec: "mp4a.40.2 (140)" } });
    expect(screen.queryByTestId("quality-badge")).toBeNull();
  });
});

describe("Player bar", () => {
  it("shows the quality badge once the page labels the playing stream", () => {
    const labelled = withMedia(base, "playing");
    renderBar({ ...labelled, player: { ...labelled.player, audioQuality: "high", audioBitrateKbps: 258 } });
    const badge = screen.getByTestId("quality-badge");
    expect(badge.textContent?.trim()).toBe("HIGH");
    expect(badge.title).toBe("High · AAC · 258 kbps");
    cleanup();
  });

  it("has no Diagnostics button and no status words; playing or paused is the play button's icon", () => {
    renderBar(withMedia(base, "playing"));
    const bar = screen.getByTestId("player-bar");
    expect(screen.queryByTestId("btn-open-diagnostics")).toBeNull();
    expect(bar.textContent).not.toMatch(/Diagnostics|Playing|Paused|\bauto\b|mp4a/);
    expect(screen.getByTestId("btn-play-pause").getAttribute("aria-label")).toBe("Pause");
    expect(screen.queryByTestId("quality-badge")).toBeNull();
  });

  it("shows like only while the write can work and sends the toggle for the playing track", async () => {
    const onToggleLike = vi.fn();
    const view = renderBar(base, { liked: true, likeGate: { enabled: false, reason: "no" }, onToggleLike });
    expect(screen.queryByTestId("player-like")).toBeNull();

    await view.rerender({ likeGate: { enabled: true, reason: null } });
    const like = screen.getByTestId("player-like");
    expect(like.getAttribute("aria-pressed")).toBe("true");
    expect(like.getAttribute("aria-label")).toBe("Remove like");
    await fireEvent.click(like);
    expect(onToggleLike).toHaveBeenCalledTimes(1);

    // While the write is in flight the same button stays, busy, and a second click sends nothing.
    await view.rerender({ likePending: true });
    expect(like.getAttribute("aria-busy")).toBe("true");
    await fireEvent.click(like);
    expect(onToggleLike).toHaveBeenCalledTimes(1);
  });

  it("opens the queue and marks it as the current page, not as a toggle, while it is open", async () => {
    const onOpenQueue = vi.fn();
    const view = renderBar(base, { onOpenQueue });
    const queue = screen.getByTestId("btn-open-queue");
    expect(queue.hasAttribute("aria-pressed")).toBe(false);
    expect(queue.hasAttribute("aria-current")).toBe(false);
    await fireEvent.click(queue);
    expect(onOpenQueue).toHaveBeenCalledTimes(1);
    await view.rerender({ queueOpen: true });
    expect(queue.getAttribute("aria-current")).toBe("page");
    expect(queue.hasAttribute("aria-pressed")).toBe(false);
  });

  it("keeps the play button's last settled state while buffering and marks it busy once buffering shows", async () => {
    vi.useFakeTimers();
    const view = renderBar({ ...base, player: { ...base.player, playback: "playing" } });
    const play = () => screen.getByTestId("btn-play-pause");
    expect(play().getAttribute("aria-label")).toBe("Pause");

    await view.rerender({ snapshot: { ...base, player: { ...base.player, playback: "buffering" } } });
    expect(play().getAttribute("aria-label")).toBe("Pause");
    expect(play().getAttribute("aria-busy")).toBe("false");
    await vi.advanceTimersByTimeAsync(600);
    expect(play().getAttribute("aria-label")).toBe("Pause");
    expect(play().getAttribute("aria-busy")).toBe("true");

    await view.rerender({ snapshot: { ...base, player: { ...base.player, playback: "paused" } } });
    expect(play().getAttribute("aria-label")).toBe("Play");
    await view.rerender({ snapshot: { ...base, player: { ...base.player, playback: "buffering" } } });
    expect(play().getAttribute("aria-label")).toBe("Play");
  });

  it("steps the volume with the arrow keys on the mute button and says the new level", async () => {
    const onCommand = vi.fn(async () => null);
    renderBar({ ...base, player: { ...base.player, volume: 0.4 } }, { onCommand });
    const mute = screen.getByTestId("btn-mute");
    await fireEvent.keyDown(mute, { key: "ArrowUp" });
    expect(onCommand).toHaveBeenLastCalledWith("setVolume", { volume: 0.45 });
    expect(screen.getByTestId("player-volume-status").textContent).toBe("Volume 45%");
    expect(mute.getAttribute("aria-label")).toBe("Mute");
    // Other keys keep the button's own behaviour and send nothing.
    await fireEvent.keyDown(mute, { key: "ArrowLeft" });
    expect(onCommand).toHaveBeenCalledTimes(1);
  });

  it("shows buffering only after 600 ms, as the ring and the flowing bar, and drops it at once", async () => {
    vi.useFakeTimers();
    const view = renderBar(base);
    const ring = () => screen.getByTestId("btn-play-pause").closest(".player-play")!;
    const scrub = () => screen.getByTestId("player-seek-slider").closest(".scrub")!;

    await view.rerender({ snapshot: { ...base, player: { ...base.player, playback: "buffering" } } });
    await vi.advanceTimersByTimeAsync(400);
    expect(ring().classList.contains("is-buffering")).toBe(false);
    // A short stall that ends in time never shows.
    await view.rerender({ snapshot: { ...base, player: { ...base.player, playback: "playing" } } });
    await vi.advanceTimersByTimeAsync(400);
    expect(ring().classList.contains("is-buffering")).toBe(false);

    await view.rerender({ snapshot: { ...base, player: { ...base.player, playback: "buffering" } } });
    await vi.advanceTimersByTimeAsync(600);
    expect(ring().classList.contains("is-buffering")).toBe(true);
    expect(scrub().classList.contains("is-busy")).toBe(true);
    await view.rerender({ snapshot: { ...base, player: { ...base.player, playback: "playing" } } });
    expect(ring().classList.contains("is-buffering")).toBe(false);
    expect(scrub().classList.contains("is-busy")).toBe(false);
  });
});

describe("Now Playing", () => {
  const track = base.player.currentTrack!;
  const lyrics: LyricsResult = {
    videoId: track.videoId,
    kind: "synced",
    source: "lrclib",
    language: null,
    matchBasis: null,
    fetchedAtMs: 0,
    fileOffsetMs: 0,
    userOffsetMs: 0,
    syncedLines: Array.from({ length: 10 }, (_, i) => ({ timeMs: i * 1000, text: `Line ${i}` })),
    plainText: null,
    error: null,
  };

  function props(snapshot: AppSnapshot, extra: Record<string, unknown> = {}) {
    return {
      snapshot,
      interpolatedPositionMs: 0,
      liked: false,
      likeGate: { enabled: true, reason: null },
      likePending: false,
      canOpenAlbum: false,
      canOpenArtist: false,
      accent: null,
      onToggleLike: vi.fn(),
      onOpenActions: vi.fn(),
      onOpenAlbum: vi.fn(),
      onOpenArtist: vi.fn(),
      onOpenLyrics: vi.fn(),
      ...extra,
    };
  }

  it("shows the track without internal state, codec chips or a second seek bar", () => {
    const snapshot = withMedia(base);
    render(NowPlayingView, {
      props: props({ ...snapshot, pending: { ...snapshot.pending, pendingCommandCount: 1, activeOp: "next" } }),
    });
    const page = screen.getByTestId("view-now-playing");
    expect(screen.getByRole("heading", { level: 1 }).textContent?.trim()).toBe(track.title);
    expect(page.textContent).not.toMatch(/Player page state|Waiting for|Codec|Audio mode|Quality|mp4a|\bauto\b/);
    expect(page.querySelector('input[type="range"]')).toBeNull();
    expect(screen.queryByTestId("np-playing-from")).toBeNull();
  });

  it("says where the track plays from only when that is given", () => {
    render(NowPlayingView, { props: props(base, { playingFrom: "Late Night" }) });
    expect(screen.getByTestId("np-playing-from").textContent).toBe("Playing from Late Night");
  });

  it("previews the lyrics the app holds: current line bright, its neighbours dimmer, and opens Lyrics", async () => {
    const onOpenLyrics = vi.fn();
    const view = render(NowPlayingView, { props: props(base, { lyrics, interpolatedPositionMs: 4_200, onOpenLyrics }) });
    const lines = () =>
      screen.getAllByTestId("np-lyrics-line").map((el) => [el.textContent, el.dataset.distance, el.className.match(/is-(current|near)/)?.[1] ?? "far"]);
    // One hidden line past each visible end, so lines fade in and out instead of popping.
    expect(lines()).toEqual([
      ["Line 1", "-3", "far"],
      ["Line 2", "-2", "far"],
      ["Line 3", "-1", "near"],
      ["Line 4", "0", "current"],
      ["Line 5", "1", "near"],
      ["Line 6", "2", "far"],
      ["Line 7", "3", "far"],
      ["Line 8", "4", "far"],
    ]);
    // A new current line keeps the same elements and the same number of slots: each line only
    // takes a new distance (its row), so the preview never reflows.
    const next = screen.getAllByTestId("np-lyrics-line")[4]!;
    await view.rerender(props(base, { lyrics, interpolatedPositionMs: 5_100, onOpenLyrics }));
    expect(next.classList.contains("is-current")).toBe(true);
    expect(next.style.getPropertyValue("--np-d").trim()).toBe("0");
    expect(screen.getAllByTestId("np-lyrics-line")).toHaveLength(8);
    await fireEvent.click(screen.getByTestId("np-lyrics-preview"));
    expect(onOpenLyrics).toHaveBeenCalledTimes(1);
  });

  it("leaves the preview out without synced lyrics for this track", () => {
    for (const held of [null, { ...lyrics, kind: "plain" as const, syncedLines: [], plainText: "Line" }, { ...lyrics, videoId: "other" }]) {
      render(NowPlayingView, { props: props(base, { lyrics: held }) });
      expect(screen.queryByTestId("np-lyrics-preview")).toBeNull();
      cleanup();
    }
  });

  it("offers Add to playlist between like and more only while adding can work", async () => {
    const onAddToPlaylist = vi.fn();
    render(NowPlayingView, { props: props(base, { playlistGate: { enabled: true, reason: null }, onAddToPlaylist }) });
    const buttons = Array.from(screen.getByTestId("np-actions").parentElement!.querySelectorAll(".np-actions > button"));
    expect(buttons.map((b) => b.getAttribute("data-testid"))).toEqual(["np-like", "np-add-to-playlist", "np-actions"]);
    await fireEvent.click(screen.getByTestId("np-add-to-playlist"));
    expect(onAddToPlaylist).toHaveBeenCalledTimes(1);
    cleanup();

    render(NowPlayingView, { props: props(base, { playlistGate: { enabled: false, reason: "Sign in" }, onAddToPlaylist }) });
    expect(screen.queryByTestId("np-add-to-playlist")).toBeNull();
  });

  it("leaves like out when the write cannot work", () => {
    render(NowPlayingView, { props: props(base, { likeGate: { enabled: false, reason: "Sign in" } }) });
    expect(screen.queryByTestId("np-like")).toBeNull();
    expect(screen.getByTestId("view-now-playing").textContent).not.toContain("Sign in");
    expect(screen.getByTestId("np-actions")).toBeTruthy();
  });
});

const STYLES = resolve(import.meta.dirname, "../../../styles");
const css = (name: string) => readFileSync(join(STYLES, name), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");

/** `{ selector, body }` for each rule inside the `@container player-dock (max-width: N)` blocks, by N. */
function dockCuts(text: string): Map<number, { selector: string; body: string }[]> {
  const out = new Map<number, { selector: string; body: string }[]>();
  for (const block of text.matchAll(/@container player-dock \(max-width: ([\d.]+)px\) \{((?:[^{}]*\{[^{}]*\})*)\s*\}/g)) {
    const rules = Array.from(block[2]!.matchAll(/([^{}]+)\{([^{}]*)\}/g), (m) => ({ selector: m[1]!.trim(), body: m[2]! }));
    out.set(Number(block[1]), rules);
  }
  return out;
}

/** Classes, attributes and pseudo-classes in a selector (the middle number of its specificity). */
const classWeight = (selector: string) => (selector.replace(/::[\w-]+/g, "").match(/\.[\w-]+|\[[^\]]+\]|:(?!where\b)[\w-]+/g) ?? []).length;

describe("Player bar at narrow docks", () => {
  it("cuts each control for real: every cut outranks the single-class display rule the control also carries", () => {
    const cuts = [...dockCuts(css("player-bar.css")).values()].flat().filter((r) => /display:\s*none/.test(r.body));
    expect(cuts.length).toBe(5);
    // .btn-icon sets its own display in controls.css, which loads after player-bar.css.
    expect(css("controls.css")).toMatch(/\.btn-icon \{[^}]*display: inline-grid/);
    for (const cut of cuts) expect(classWeight(cut.selector), cut.selector).toBeGreaterThan(1);
  });

  /** The bar's columns at a dock width, from the compact grid rule and the cuts that apply there. */
  function barAt(dock: number) {
    const cuts = dockCuts(css("player-bar.css"));
    const active = [...cuts.entries()].filter(([at]) => dock <= at).flatMap(([, rules]) => rules);
    const hidden = (cls: string) => active.some((r) => r.selector.endsWith(`.${cls}`) && /display:\s*none/.test(r.body));
    const grid = dock - 28;
    const compact = active.find((r) => r.selector === ".app-player-bar")?.body;
    const columns = compact?.match(/grid-template-columns:\s*minmax\(0, 1fr\) (\d+)px minmax\(0, 1fr\)/);
    const centre = columns ? Number(columns[1]) : Math.max(360, grid * 0.44);
    const gap = compact ? Number(compact.match(/column-gap:\s*var\(--space-(\d+)\)/)![1]) : 20;
    const side = (grid - centre - 2 * gap) / 2;
    const like = !hidden("player-like");
    // Cover, its gap and the button's padding; 44 more beside like.
    const title = side - 80 - (like ? 44 : 0);
    const tail = 2 * 40 + 4 + (hidden("player-lyrics") ? 0 : 44) + (hidden("player-quality") ? 0 : 56) + (hidden("player-volume") ? 0 : 100);
    return { centre, side, title, tail, like };
  }

  it("keeps the transport centred and a readable title across the compact tier, with the tail in its column", () => {
    const cuts = dockCuts(css("player-bar.css"));
    const compactAt = [...cuts.entries()].find(([, rules]) => rules.some((r) => r.selector === ".app-player-bar"))?.[0];
    // The compact tier is content under 880 px; the dock is 10 px narrower than the content area.
    expect(compactAt).toBe(869.98);
    const body = cuts.get(869.98)!.find((r) => r.selector === ".app-player-bar")!.body;
    // Equal outer tracks keep the transport centred, which the sidebar's half-way glide relies on.
    expect(body).toMatch(/grid-template-columns:\s*minmax\(0, 1fr\) \d+px minmax\(0, 1fr\)/);
    // The centre still holds the transport: 44 + 4 × 40 + 4 × 8 px.
    expect(barAt(869).centre).toBeGreaterThanOrEqual(44 + 4 * 40 + 4 * 8);
    for (const dock of [869, 840, 810, 776]) {
      const bar = barAt(dock);
      expect(bar.like, `like at ${dock}`).toBe(true);
      expect(bar.title, `title at ${dock}`).toBeGreaterThanOrEqual(120);
      expect(bar.tail, `tail at ${dock}`).toBeLessThanOrEqual(bar.side);
    }
    // 630 px: a 900 px window with the sidebar open.
    for (const dock of [775, 630]) expect(barAt(dock).title, `title at ${dock}`).toBeGreaterThanOrEqual(85);
    // Lyrics, queue and mute hold to the narrowest dock that keeps lyrics.
    for (const dock of [775, 630, 544]) expect(barAt(dock).tail, `tail at ${dock}`).toBeLessThanOrEqual(barAt(dock).side);
  });

  it("draws the quality mark as firmly as an HD mark", () => {
    const badge = css("player-bar.css").match(/\.quality-badge \{([^}]*)\}/)![1]!;
    expect(badge).toMatch(/font-weight: 700/);
    expect(badge).toMatch(/letter-spacing: 0\.08em/);
  });
});

describe("Track menu", () => {
  const track = base.player.currentTrack!;
  const bounds = { left: 270, top: 8, right: 1432, bottom: 862 };

  it("grows below its trigger, lined up with the trigger's nearer side, and from the trigger's point", () => {
    // A row's ⋯ on the right half: right edges line up, the menu grows from the button's centre.
    const more = { left: 1380, right: 1412, top: 200, bottom: 232 };
    expect(placeMenu(more, { width: 300, height: 320 }, bounds, 960)).toEqual({
      left: 1112,
      width: 300,
      top: 238,
      bottom: null,
      maxHeight: 624,
      origin: "284px 0",
    });
    // A right-click near the left edge: the menu starts at the point.
    expect(placeMenu({ left: 300, right: 300, top: 100, bottom: 100 }, { width: 300, height: 200 }, bounds, 960)).toMatchObject({
      left: 300,
      top: 106,
      origin: "0px 0",
    });
  });

  it("opens upward when there is no room below, and never leaves the content area", () => {
    const low = { left: 1380, right: 1412, top: 800, bottom: 832 };
    expect(placeMenu(low, { width: 300, height: 320 }, bounds, 960)).toEqual({
      left: 1112,
      width: 300,
      top: null,
      bottom: 166,
      maxHeight: 786,
      origin: "284px 100%",
    });
    // Narrower than the menu: it takes the whole width; taller than both sides: the larger side, scrolling.
    const narrow = { left: 270, top: 8, right: 500, bottom: 400 };
    const placed = placeMenu({ left: 480, right: 490, top: 150, bottom: 160 }, { width: 300, height: 600 }, narrow, 960);
    expect(placed).toMatchObject({ left: 270, width: 230, top: 166, maxHeight: 234 });
  });

  function renderMenu(extra: Record<string, unknown> = {}) {
    const onClose = vi.fn();
    const onRequestPlaylists = vi.fn();
    const result = render(TrackActions, {
      props: {
        track,
        liked: false,
        enqueueGate: { enabled: true, reason: null },
        likeGate: { enabled: true, reason: null },
        playlistGate: { enabled: true, reason: null },
        canOpenAlbum: false,
        canOpenArtist: false,
        pending: false,
        playlistsView: { kind: "ready", cached: false, error: null },
        playlists: [
          { playlistId: "pl-a", title: "Alpha" },
          { playlistId: "pl-b", title: "Beta" },
        ],
        onClose,
        onPlay: vi.fn(),
        onEnqueue: vi.fn(),
        onSetLike: vi.fn(),
        onRequestPlaylists,
        onAddToPlaylist: vi.fn(),
        onOpenAlbum: vi.fn(),
        onOpenArtist: vi.fn(),
        ...extra,
      },
    });
    return { ...result, onClose, onRequestPlaylists };
  }

  it("is an overlay menu, not a modal: popover glass, no scrim, no modal dialog", () => {
    renderMenu();
    const menu = screen.getByTestId("track-actions-menu");
    expect(menu.classList.contains("lg--popover")).toBe(true);
    expect(menu.classList.contains("lg-overlay")).toBe(true);
    expect(menu.getAttribute("aria-modal")).toBeNull();
    expect(document.querySelector(".app-dialog-layer")).toBeNull();
    expect(screen.queryByTestId("accessible-dialog")).toBeNull();
  });

  it("takes focus, walks its items with the arrow keys, and closes on Escape without reaching global shortcuts", async () => {
    const outside = vi.fn();
    window.addEventListener("keydown", outside);
    try {
      const { onClose } = renderMenu();
      await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("action-play")));
      await fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
      expect(document.activeElement).toBe(screen.getByTestId("action-play-next"));
      await fireEvent.keyDown(document.activeElement!, { key: "ArrowUp" });
      await fireEvent.keyDown(document.activeElement!, { key: "ArrowUp" });
      expect(document.activeElement).toBe(screen.getByTestId("action-add-to-playlist"));
      await fireEvent.keyDown(document.activeElement!, { key: "Escape" });
      expect(onClose).toHaveBeenCalledTimes(1);
      expect(outside).not.toHaveBeenCalled();
    } finally {
      window.removeEventListener("keydown", outside);
    }
  });

  it("closes on a press outside or a scroll outside, but not on a scroll inside it", async () => {
    const { onClose } = renderMenu();
    const menu = screen.getByTestId("track-actions-menu");
    await fireEvent.pointerDown(menu.querySelector("button")!);
    menu.dispatchEvent(new Event("scroll"));
    expect(onClose).not.toHaveBeenCalled();
    await fireEvent.scroll(document);
    expect(onClose).toHaveBeenCalledTimes(1);
    await fireEvent.pointerDown(document.body);
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("goes back to the control that opened it, and a press on that control closes it without reopening", async () => {
    const opener = document.createElement("button");
    const reopen = vi.fn();
    opener.addEventListener("click", reopen);
    document.body.append(opener);
    try {
      opener.focus();
      await fireEvent.click(opener);
      reopen.mockClear();
      const { onClose, unmount } = renderMenu();
      await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("action-play")));
      await fireEvent.pointerDown(opener);
      expect(onClose).toHaveBeenCalledTimes(1);
      await fireEvent.click(opener);
      expect(reopen).not.toHaveBeenCalled();
      unmount();
      await waitFor(() => expect(document.activeElement).toBe(opener));
      // Only that one click is swallowed.
      await fireEvent.pointerDown(opener);
      await fireEvent.click(opener);
      expect(reopen).toHaveBeenCalledTimes(1);
    } finally {
      opener.remove();
    }
  });

  it("retires the menu before its outro ends and reopens the same surface", async () => {
    const { rerender } = renderMenu();
    const menu = screen.getByTestId("track-actions-menu");
    menu.style.setProperty("--lg-overlay-exit-duration", ".16s");
    await rerender({ active: false });
    expect(menu.isConnected).toBe(true);
    expect(menu.inert).toBe(true);
    expect(menu.getAttribute("aria-hidden")).toBe("true");
    expect(screen.queryByTestId("track-actions-menu")).toBeNull();
    expect(menu.querySelector("[data-testid]")).toBeNull();

    await rerender({ active: true, openKey: 1 });
    expect(screen.getByTestId("track-actions-menu")).toBe(menu);
    expect(menu.inert).toBe(false);
    expect(menu.getAttribute("aria-hidden")).toBeNull();
    await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("action-play")));
  });

  it("grows from the point of the right-click that opened it", () => {
    const row = document.createElement("div");
    document.body.append(row);
    try {
      row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 1000, clientY: 300 }));
      renderMenu();
      const menu = screen.getByTestId("track-actions-menu");
      // The content area is the window less 8 px here: the point is on its right half, so the
      // menu's right edge is at the point and it opens downward from it.
      expect(menu.style.left).toBe("700px");
      expect(menu.style.top).toBe("306px");
      expect(menu.style.width).toBe("300px");
      expect(menu.style.getPropertyValue("--lg-morph-origin")).toBe("300px 0");
    } finally {
      row.remove();
    }
  });

  it("swallows no later click after a right-click on its own trigger", async () => {
    const opener = document.createElement("button");
    const activate = vi.fn();
    opener.addEventListener("click", activate);
    document.body.append(opener);
    try {
      await fireEvent.click(opener);
      activate.mockClear();
      const { onClose } = renderMenu();
      await fireEvent.pointerDown(opener, { button: 2 });
      await fireEvent.contextMenu(opener);
      expect(onClose).toHaveBeenCalledTimes(1);
      await fireEvent.keyDown(opener, { key: "Enter" });
      await fireEvent.click(opener);
      expect(activate).toHaveBeenCalledTimes(1);
    } finally {
      opener.remove();
    }
  });

  it("after a right-click on a row, a press on the row's play button closes the menu and still plays", async () => {
    const row = document.createElement("div");
    const play = document.createElement("button");
    const onRowPlay = vi.fn();
    play.addEventListener("click", onRowPlay);
    row.append(play);
    document.body.append(row);
    try {
      // Chromium focuses a button on a right press, so the play button is the focused opener.
      play.focus();
      play.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 400, clientY: 300 }));
      const { onClose } = renderMenu();
      await fireEvent.pointerDown(play);
      expect(onClose).toHaveBeenCalledTimes(1);
      await fireEvent.click(play);
      expect(onRowPlay).toHaveBeenCalledTimes(1);
    } finally {
      row.remove();
    }
  });

  it("never takes the focusable page around a right-click as its opener", async () => {
    const main = document.createElement("main");
    main.setAttribute("tabindex", "-1");
    const cell = document.createElement("span");
    const link = document.createElement("button");
    const onLink = vi.fn();
    link.addEventListener("click", onLink);
    main.append(cell, link);
    document.body.append(main);
    try {
      cell.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 400, clientY: 300 }));
      const { onClose } = renderMenu();
      await fireEvent.pointerDown(link);
      expect(onClose).toHaveBeenCalledTimes(1);
      await fireEvent.click(link);
      expect(onLink).toHaveBeenCalledTimes(1);
    } finally {
      main.remove();
    }
  });

  it("stays inside the content area: right of the sidebar and above the player dock", () => {
    const content = document.createElement("div");
    content.className = "app-content";
    const dock = document.createElement("div");
    dock.className = "app-player-dock";
    const rect = (left: number, top: number, right: number, bottom: number) =>
      ({ left, top, right, bottom, width: right - left, height: bottom - top, x: left, y: top, toJSON: () => ({}) }) as DOMRect;
    content.getBoundingClientRect = () => rect(262, 0, window.innerWidth, window.innerHeight);
    dock.getBoundingClientRect = () => rect(262, window.innerHeight - 100, window.innerWidth - 10, window.innerHeight - 10);
    const row = document.createElement("div");
    document.body.append(content, dock, row);
    try {
      // A right-click left of the content area and over the dock: the menu is pushed right of the
      // sidebar and opens upward with its foot 8 px above the dock.
      row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 100, clientY: window.innerHeight - 50 }));
      renderMenu();
      const menu = screen.getByTestId("track-actions-menu");
      expect(menu.style.left).toBe("270px");
      expect(menu.style.top).toBe("");
      expect(menu.style.bottom).toBe(`${100 + 8}px`);
    } finally {
      content.remove();
      dock.remove();
      row.remove();
    }
  });

  it("opens straight on the playlist chooser when asked, and asks for the playlists", async () => {
    const { onRequestPlaylists } = renderMenu({ startAt: "pick" });
    expect(screen.queryByTestId("action-play")).toBeNull();
    expect(screen.getByTestId("pick-playlist-pl-a")).toBeTruthy();
    expect(onRequestPlaylists).toHaveBeenCalledTimes(1);
    await fireEvent.click(screen.getByTestId("pick-playlist-pl-b"));
    await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("confirm-add-to-playlist")));
  });

  it.each([["en", en], ["zh-CN", zhCN]] as const)(
    "keeps playlist pagination's provider error in the tooltip in %s",
    async (locale, copy) => {
      setLocale(locale);
      const error = { code: "RATE_LIMITED", safeMessage: "fixture rejected playlist continuation", retriable: true };
      const retry = vi.fn();
      renderMenu({ startAt: "pick", playlistsHasMore: true, playlistsMoreError: error, onLoadMorePlaylists: retry });
      const failure = screen.getByTestId("playlist-picker-more-error");
      expect(failure.textContent).toContain(copy.states.tryAgainSoon);
      expect(failure.textContent).not.toContain(error.safeMessage);
      expect(failure.title).toBe(`${error.code}${copy.common.colon}${error.safeMessage}`);
      await fireEvent.click(screen.getByTestId("playlist-picker-load-more"));
      expect(retry).toHaveBeenCalledOnce();
    }
  );
});
