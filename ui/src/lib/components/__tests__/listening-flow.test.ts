import { cleanup, fireEvent, render, screen, within } from "@testing-library/svelte";
import {
  FakeCoreClient,
  createSyntheticTracks,
  type AppSnapshot,
  type CommandReceipt,
  type OfficialUpcoming,
  type PlayerCommandArgs,
  type PlayerOp,
  type QueueEntry,
  type QueueSummary,
} from "@ytmcp/protocol";
import { tick } from "svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import NowPlayingView from "../NowPlayingView.svelte";
import PlaybackIndicator from "../PlaybackIndicator.svelte";
import PlayerBar from "../PlayerBar.svelte";
import QueueView from "../QueueView.svelte";
import Sidebar from "../Sidebar.svelte";
import { ARTWORK_HOLD_MS } from "../../ambient/held-artwork.svelte.js";
import { RETURN_FRAMES_KEY, ReturnFrames } from "../../return-frame.js";

afterEach(() => cleanup());

const base: AppSnapshot = new FakeCoreClient("normal").subscribeSnapshot(() => {}).snapshot;

function entries(count: number, prefix = "e"): QueueEntry[] {
  return createSyntheticTracks(count, prefix).map((track, i) => ({
    entryId: `${prefix}-${i}`,
    videoId: track.videoId,
    track: { ...track, availability: "available" },
    availability: "available",
  }));
}

/** A snapshot whose player is on `currentEntryId` with its own track instance. */
function snapshotWith(queueSummary: QueueSummary, playback: AppSnapshot["player"]["playback"] = "playing"): AppSnapshot {
  const current = queueSummary.previewEntries?.find((e) => e.entryId === queueSummary.currentEntryId);
  return {
    ...base,
    player: {
      ...base.player,
      playback,
      connection: "ready",
      currentTrack: current?.track ?? base.player.currentTrack,
      currentQueueEntryId: queueSummary.currentEntryId ?? null,
      trackInstanceId: "ti-1",
    },
    queueSummary,
  };
}

function renderQueue(snapshot: AppSnapshot, frames?: ReturnFrames) {
  return render(QueueView, {
    context: frames ? new Map([[RETURN_FRAMES_KEY, frames]]) : undefined,
    props: {
      queueSummary: snapshot.queueSummary,
      capabilities: snapshot.capabilities,
      connection: snapshot.player.connection,
      player: snapshot.player,
      mockMode: true,
      motion: true,
      onSelectEntry: () => {},
      onTrackActions: () => {},
    },
  });
}

const rowNumber = (row: HTMLElement) => row.querySelector("button span")?.textContent?.trim();

/** The 230-entry-like window: 30 entries before it, 200 loaded, a 100-entry preview from index 100. */
function windowSummary(): QueueSummary {
  const preview = entries(100);
  return {
    queueRevision: 4,
    currentEntryId: preview[90]!.entryId,
    completeness: "partial",
    hasMore: true,
    loadedStart: 30,
    loadedCount: 200,
    previewStart: 100,
    previewEntries: preview,
  };
}

describe("Queue sections", () => {
  it("splits an anchored window into a collapsed Played, the current occurrence and Up next at absolute positions", async () => {
    renderQueue(snapshotWith(windowSummary()));

    // The total is not known on a partial window, so no bare "Track N" counter is drawn.
    expect(screen.queryByTestId("queue-position")).toBeNull();
    const played = screen.getByTestId("queue-played");
    const toggle = within(played).getByTestId("queue-played-toggle");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(within(played).getByTestId("queue-played-count").textContent).toBe("90");
    expect(within(played).getByTestId("queue-earlier-hidden")).toBeTruthy();
    expect(within(played).queryAllByTestId("virtual-track-row")).toHaveLength(0);

    const now = within(screen.getByTestId("queue-now")).getAllByTestId("virtual-track-row");
    expect(now.map((r) => r.getAttribute("data-row-key"))).toEqual(["e-90"]);

    const upNext = within(screen.getByTestId("queue-up-next")).getAllByTestId("virtual-track-row");
    expect(upNext.map((r) => r.getAttribute("data-row-key"))).toEqual(Array.from({ length: 9 }, (_, i) => `e-${91 + i}`));
    expect(rowNumber(upNext[0]!)).toBe("222");

    await fireEvent.click(toggle);
    const playedRows = within(played).getAllByTestId("virtual-track-row");
    expect(playedRows[0]!.getAttribute("data-row-key")).toBe("e-0");
    expect(rowNumber(playedRows[0]!)).toBe("131");
  });

  it("shows N of M only for a whole, finite queue", () => {
    const preview = entries(5);
    renderQueue(
      snapshotWith({
        queueRevision: 1,
        currentEntryId: preview[2]!.entryId,
        completeness: "complete",
        hasMore: false,
        loadedStart: 0,
        loadedCount: 5,
        previewStart: 0,
        previewEntries: preview,
      })
    );
    expect(screen.getByTestId("queue-position").textContent?.trim()).toBe("Track 3 of 5");
    expect(screen.queryByTestId("queue-earlier-hidden")).toBeNull();
  });

  it("marks rows as not startable, with the reason, while the page cannot jump in the queue", async () => {
    const snapshot = snapshotWith(windowSummary());
    const onSelectEntry = vi.fn();
    render(QueueView, {
      props: {
        queueSummary: snapshot.queueSummary,
        capabilities: { ...snapshot.capabilities, queueMirror: "unsupported" },
        connection: snapshot.player.connection,
        player: snapshot.player,
        mockMode: true,
        onSelectEntry,
        onTrackActions: () => {},
      },
    });
    const row = within(screen.getByTestId("queue-up-next")).getAllByTestId("virtual-track-row")[0]!;
    const button = row.querySelector<HTMLButtonElement>("button.tl-main")!;
    expect(button.getAttribute("aria-disabled")).toBe("true");
    expect(button.title).toBe("This connection does not support this action");
    await fireEvent.click(button);
    expect(onSelectEntry).not.toHaveBeenCalled();
  });

  it.each([["not in the preview", "e-gone"], ["not reported", null]])("falls back to one flat list when the current occurrence is %s", (_, currentEntryId) => {
    const summary = { ...windowSummary(), currentEntryId };
    renderQueue({ ...snapshotWith(windowSummary()), queueSummary: summary });

    expect(screen.queryByTestId("queue-unknown-place")).toBeNull();
    expect(screen.queryByTestId("queue-up-next")).toBeNull();
    expect(screen.queryByTestId("queue-played")).toBeNull();
    expect(screen.queryByTestId("queue-position")).toBeNull();
    const rows = screen.getAllByTestId("virtual-track-row");
    expect(rows[0]!.getAttribute("data-row-key")).toBe("e-0");
    expect(rowNumber(rows[0]!)).toBe("131");
    expect(screen.queryByTestId("playback-indicator")).toBeNull();
  });

  it("shows YouTube Music's continuation read-only for each status", () => {
    const preview = entries(3);
    const summary = (officialUpcoming: OfficialUpcoming | null): QueueSummary => ({
      queueRevision: 1,
      currentEntryId: preview[0]!.entryId,
      completeness: "partial",
      hasMore: true,
      loadedCount: 3,
      previewEntries: preview,
      officialUpcoming,
      filters: [
        { filterId: "f-all", label: "All", selected: false },
        { filterId: "f-discover", label: "Discover", selected: true },
      ],
    });

    renderQueue(snapshotWith(summary({ status: "observed", tracks: createSyntheticTracks(4, "auto") })));
    const official = screen.getByTestId("queue-official");
    expect(official.textContent).toContain("Then from YouTube Music");
    expect(within(official).getByTestId("queue-official-filter").textContent).toBe("Discover");
    expect(within(official).queryByRole("button", { name: "Discover" })).toBeNull();
    const rows = within(official).getAllByTestId("queue-official-row");
    expect(rows).toHaveLength(4);
    // Only the actions menu: no select, move or remove on a suggestion.
    expect(within(rows[0]!).getAllByRole("button").map((b) => b.getAttribute("data-testid"))).toEqual(["track-actions-btn"]);
    cleanup();

    const texts: [OfficialUpcoming["status"], string][] = [
      ["loading", "Loading suggestions…"],
      ["off", "Autoplay is off, so nothing plays after the queue."],
      ["radio", "This radio keeps adding tracks."],
    ];
    for (const [status, text] of texts) {
      renderQueue(snapshotWith(summary({ status })));
      expect(screen.getByTestId("queue-official-status").textContent?.trim()).toBe(text);
      expect(screen.queryAllByTestId("queue-official-row")).toHaveLength(0);
      cleanup();
    }

    for (const official of [{ status: "none" as const }, null]) {
      renderQueue(snapshotWith(summary(official)));
      expect(screen.queryByTestId("queue-official")).toBeNull();
      cleanup();
    }
  });

  it("lists no suggestions after a preview that stops before the loaded queue's end", () => {
    const preview = entries(100);
    const cutOff = (officialUpcoming: OfficialUpcoming, currentEntryId: string | null = preview[5]!.entryId): QueueSummary => ({
      queueRevision: 1,
      currentEntryId,
      completeness: "partial",
      hasMore: true,
      loadedCount: 200,
      previewStart: 0,
      previewEntries: preview,
      officialUpcoming,
    });
    const observed: OfficialUpcoming = { status: "observed", tracks: createSyntheticTracks(20, "auto") };

    for (const currentEntryId of [preview[5]!.entryId, null]) {
      renderQueue(snapshotWith(cutOff(observed, currentEntryId)));
      expect(screen.getByTestId("queue-later-hidden").textContent).toContain("100");
      expect(screen.queryByTestId("queue-official")).toBeNull();
      cleanup();
    }

    renderQueue(snapshotWith(cutOff({ status: "loading" })));
    expect(screen.queryByTestId("queue-official")).toBeNull();
    cleanup();
    renderQueue(snapshotWith(cutOff({ status: "off" })));
    expect(screen.getByTestId("queue-official-status").textContent).toContain("Autoplay is off");
  });
});

/** A whole 10-entry queue on entry `current`. */
function wholeSummary(current: number): QueueSummary {
  const preview = entries(10);
  return {
    queueRevision: 1,
    currentEntryId: preview[current]!.entryId,
    completeness: "complete",
    hasMore: false,
    loadedStart: 0,
    loadedCount: 10,
    previewStart: 0,
    previewEntries: preview,
  };
}

const rowButton = (section: string, key: string) =>
  within(screen.getByTestId(section))
    .getAllByTestId("virtual-track-row")
    .find((r) => r.getAttribute("data-row-key") === key)!
    .querySelector<HTMLElement>("[data-row-index]")!;
const focusedKey = () => document.activeElement?.closest("[data-row-key]")?.getAttribute("data-row-key") ?? null;

describe("Queue focus across sections", () => {
  async function moveTo(view: ReturnType<typeof renderQueue>, current: number) {
    const snapshot = snapshotWith(wholeSummary(current));
    await view.rerender({ queueSummary: snapshot.queueSummary, player: snapshot.player });
    await tick();
  }

  it("follows an Up next row that becomes the current one into Now playing", async () => {
    const view = renderQueue(snapshotWith(wholeSummary(2)));
    rowButton("queue-up-next", "e-5").focus();
    await moveTo(view, 5);
    expect(document.activeElement).toBe(rowButton("queue-now", "e-5"));
  });

  it("falls back to the Now playing row when the focused current row moves into a collapsed Played", async () => {
    const view = renderQueue(snapshotWith(wholeSummary(2)));
    rowButton("queue-now", "e-2").focus();
    await moveTo(view, 3);
    expect(focusedKey()).toBe("e-3");
    expect(document.activeElement?.closest('[data-testid="queue-now"]')).not.toBeNull();
  });

  it("follows the row into an expanded Played", async () => {
    const view = renderQueue(snapshotWith(wholeSummary(2)));
    await fireEvent.click(screen.getByTestId("queue-played-toggle"));
    rowButton("queue-now", "e-2").focus();
    await moveTo(view, 3);
    expect(document.activeElement).toBe(rowButton("queue-played", "e-2"));
  });

  it("hands the lyrics return frame to Now playing once it is the only list, and back to Up next", async () => {
    const frames = new ReturnFrames();
    const view = renderQueue(snapshotWith(wholeSummary(7)), frames);
    await tick();
    expect(frames.capture()?.anchorKey).toBe("e-8");

    await moveTo(view, 9);
    rowButton("queue-now", "e-9").focus();
    expect(frames.capture()).toMatchObject({ anchorKey: "e-9", focusKey: "e-9" });

    await moveTo(view, 7);
    expect(frames.capture()?.anchorKey).toBe("e-8");
  });
});

describe("Up Next filter", () => {
  const preview = entries(3);
  const filterSummary = (selected: string): QueueSummary => ({
    queueRevision: 7,
    currentEntryId: preview[0]!.entryId,
    completeness: "complete",
    hasMore: false,
    loadedCount: 3,
    previewEntries: preview,
    officialUpcoming: { status: "observed", tracks: createSyntheticTracks(3, "auto") },
    filters: ["All", "Familiar", "Discover"].map((label) => ({ filterId: `qf-${label}`, label, selected: label === selected })),
  });
  const receipt = (status: CommandReceipt["status"], commandId = "cmd-1"): CommandReceipt => ({
    commandId,
    op: "setQueueFilter",
    status,
    acceptedAtMs: 0,
    deduplicated: false,
  });

  function renderFilters(selected: string, onCommand: (op: PlayerOp, args: PlayerCommandArgs) => Promise<CommandReceipt | null>) {
    const snapshot = snapshotWith(filterSummary(selected));
    const props = (summary: QueueSummary, lastReceipt: CommandReceipt | null = null) => ({
      queueSummary: summary,
      // Edits are supported, so the queue note would carry a pending reason if one applied.
      capabilities: { ...snapshot.capabilities, queueFilter: "supported" as const, queueRemove: "supported" as const },
      connection: snapshot.player.connection,
      player: snapshot.player,
      lastReceipt,
      mockMode: true,
      onSelectEntry: () => {},
      onTrackActions: () => {},
      onCommand,
    });
    const view = render(QueueView, { props: props(snapshot.queueSummary) });
    return { rerender: (summary: QueueSummary, lastReceipt?: CommandReceipt | null) => view.rerender(props(summary, lastReceipt)) };
  }

  const filterButton = (label: string) => within(screen.getByTestId("queue-filters")).getByRole("button", { name: label });

  it("sends the chosen filter's id, waits on that chip only, and settles once the summary shows it selected", async () => {
    const onCommand = vi.fn(async () => receipt("accepted"));
    const view = renderFilters("All", onCommand);
    expect(filterButton("All").getAttribute("aria-pressed")).toBe("true");
    expect(filterButton("Discover").getAttribute("aria-pressed")).toBe("false");

    await fireEvent.click(filterButton("All"));
    expect(onCommand).not.toHaveBeenCalled();

    await fireEvent.click(filterButton("Discover"));
    expect(onCommand).toHaveBeenCalledWith("setQueueFilter", { queueFilterId: "qf-Discover" });
    await tick();
    expect(filterButton("Discover").getAttribute("aria-busy")).toBe("true");
    expect(filterButton("Familiar").getAttribute("aria-busy")).toBe("false");
    // The group refuses clicks without native `disabled`, so the focused chip keeps focus.
    for (const label of ["All", "Familiar", "Discover"]) {
      expect(filterButton(label).getAttribute("aria-disabled")).toBe("true");
      expect((filterButton(label) as HTMLButtonElement).disabled).toBe(false);
    }
    expect(filterButton("Familiar").title).toBe("Waiting for the player page to switch the Up Next filter…");
    await fireEvent.click(filterButton("Familiar"));
    expect(onCommand).toHaveBeenCalledTimes(1);
    // The wait is the chip's own dot: no sentence is added to the page.
    expect(screen.queryByTestId("queue-note")).toBeNull();
    expect(screen.getByTestId("view-queue").textContent).not.toContain("Waiting");
    expect(filterButton("Discover").querySelector(".wait-dot")).not.toBeNull();
    const now = screen.getByTestId("queue-now");
    expect(now.querySelector('[aria-busy="true"]')).toBeNull();
    expect(screen.queryByTestId("queue-row-pending")).toBeNull();

    // The page keeps the explicit queue and its revision; only the selection moves.
    await view.rerender(filterSummary("Discover"));
    await tick();
    expect(filterButton("Discover").getAttribute("aria-pressed")).toBe("true");
    expect(filterButton("Discover").getAttribute("aria-busy")).toBe("false");
    expect(filterButton("All").getAttribute("aria-disabled")).toBe("false");
    expect(filterButton("All").title).toBe("Choose which suggestions YouTube Music plays after the queue");
  });

  it("adds nothing to the page when the page refuses the filter (the app's toast reports it), and lets the user try again", async () => {
    const onCommand = vi.fn(async () => receipt("dispatched"));
    const view = renderFilters("All", onCommand);
    const before = screen.getByTestId("view-queue").innerHTML;
    await fireEvent.click(filterButton("Familiar"));
    await tick();

    await view.rerender(filterSummary("All"), receipt("failed"));
    await tick();
    expect(screen.getByTestId("view-queue").innerHTML).toBe(before);
    expect(filterButton("Familiar").getAttribute("aria-disabled")).toBe("false");
    expect(filterButton("Familiar").getAttribute("aria-busy")).toBe("false");

    onCommand.mockImplementationOnce(async () => receipt("failed", "cmd-2"));
    await fireEvent.click(filterButton("Familiar"));
    expect(onCommand).toHaveBeenCalledTimes(2);
    await tick();
    expect(screen.getByTestId("view-queue").innerHTML).toBe(before);
    expect(filterButton("Familiar").getAttribute("aria-disabled")).toBe("false");
  });

  it("leaves the player bar's waiting dot off while only a filter is waiting, not while another command waits with it", async () => {
    vi.useFakeTimers();
    try {
      for (const [pendingCount, busy] of [[1, "false"], [2, "true"]] as const) {
        const pendingOverlay = { pendingCount, activeOp: "setQueueFilter" as const, desiredPlayback: null, desiredPositionMs: null, desiredVolume: null, lastError: null };
        render(PlayerBar, {
          props: { snapshot: snapshotWith(filterSummary("All")), pendingOverlay, interpolatedPositionMs: 0, onCommand: vi.fn(), onOpenNowPlaying: () => {}, onToggleLyrics: () => {} },
        });
        await vi.advanceTimersByTimeAsync(600);
        expect(screen.getByTestId("player-transport").getAttribute("aria-busy")).toBe(busy);
        expect(screen.queryByTestId("player-waiting-dot") !== null).toBe(busy === "true");
        cleanup();
      }
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps the passive selected label without the capability", () => {
    render(QueueView, {
      props: {
        queueSummary: filterSummary("Discover"),
        capabilities: { ...base.capabilities, queueFilter: "unverified" },
        connection: "ready",
        player: snapshotWith(filterSummary("Discover")).player,
        mockMode: true,
        onSelectEntry: () => {},
        onTrackActions: () => {},
        onCommand: vi.fn(),
      },
    });
    expect(screen.queryByTestId("queue-filters")).toBeNull();
    expect(screen.getByTestId("queue-official-filter").textContent).toBe("Discover");
    expect(within(screen.getByTestId("queue-official")).queryByRole("button", { name: "Discover" })).toBeNull();
  });
});

describe("Playback indicator", () => {
  it("animates while playing, holds still while paused, pulses while buffering and hides when idle or disconnected", () => {
    const cases = [
      { playback: "playing", motion: true, state: "playing", animated: "true" },
      { playback: "playing", motion: false, state: "playing", animated: "false" },
      { playback: "paused", motion: true, state: "paused", animated: "false" },
      { playback: "buffering", motion: true, state: "buffering", animated: "true" },
    ] as const;
    for (const c of cases) {
      render(PlaybackIndicator, { props: { playback: c.playback, motion: c.motion } });
      const el = screen.getByTestId("playback-indicator");
      expect([el.dataset.state, el.dataset.animated]).toEqual([c.state, c.animated]);
      cleanup();
    }
    render(PlaybackIndicator, { props: { playback: "idle", motion: true } });
    expect(screen.queryByTestId("playback-indicator")).toBeNull();
    cleanup();
    render(PlaybackIndicator, { props: { playback: "playing", connection: "disconnected", motion: true } });
    expect(screen.queryByTestId("playback-indicator")).toBeNull();
  });

  it("marks only the current occurrence, not a later copy of the same song", () => {
    const preview = entries(4);
    preview[3] = { ...preview[1]!, entryId: "e-dup" };
    const summary: QueueSummary = {
      queueRevision: 1,
      currentEntryId: "e-1",
      completeness: "complete",
      hasMore: false,
      loadedCount: 4,
      previewEntries: preview,
    };
    renderQueue(snapshotWith(summary));
    const indicators = screen.getAllByTestId("playback-indicator");
    expect(indicators).toHaveLength(1);
    expect(within(screen.getByTestId("queue-now")).getByTestId("playback-indicator")).toBe(indicators[0]);
    cleanup();

    // The player is on another occurrence: the queue's current row gets no indicator.
    const elsewhere = snapshotWith(summary);
    renderQueue({ ...elsewhere, player: { ...elsewhere.player, currentQueueEntryId: "e-dup" } });
    expect(screen.queryByTestId("playback-indicator")).toBeNull();
  });

  it("replaces the sidebar's Now Playing icon while a track is loaded", () => {
    const props = { activeView: null, collapsed: false, mockMode: false, motion: true, onNavigate: () => {}, onToggleCollapse: () => {} };
    render(Sidebar, { props: { ...props, playback: "paused" } });
    expect(within(screen.getByTestId("nav-now-playing")).getByTestId("playback-indicator").dataset.state).toBe("paused");
    cleanup();
    render(Sidebar, { props: { ...props, playback: "idle" } });
    expect(within(screen.getByTestId("nav-now-playing")).queryByTestId("playback-indicator")).toBeNull();
  });
});

describe("Now Playing up next", () => {
  const props = (snapshot: AppSnapshot, motionReduced = false) => ({
    snapshot,
    interpolatedPositionMs: 0,
    liked: null,
    likeGate: { enabled: false, reason: null },
    likePending: false,
    canOpenAlbum: false,
    canOpenArtist: false,
    accent: null,
    decorativeMotion: false,
    motionReduced,
    onToggleLike: vi.fn(),
    onOpenActions: vi.fn(),
    onOpenAlbum: vi.fn(),
    onOpenArtist: vi.fn(),
    onOpenLyrics: vi.fn(),
    onOpenQueue: vi.fn(),
    selectGate: { enabled: true, reason: null },
    onSelectEntry: vi.fn(async (): Promise<CommandReceipt | null> => null),
  });

  function summary(overrides: Partial<QueueSummary> = {}): QueueSummary {
    const preview = entries(4);
    return {
      queueRevision: 1,
      currentEntryId: preview[1]!.entryId,
      completeness: "partial",
      hasMore: true,
      loadedCount: 4,
      previewStart: 0,
      previewEntries: preview,
      officialUpcoming: { status: "observed", tracks: createSyntheticTracks(6, "auto") },
      filters: [{ filterId: "f-1", label: "Discover", selected: true }],
      ...overrides,
    };
  }

  const upNextRows = () =>
    screen.getAllByTestId("np-up-next-row").map((row) => [row.textContent?.match(/#\d+/)?.[0], row.dataset.suggestion]);

  it("lists the queued tracks first, then YouTube Music's suggestions, with the source", () => {
    render(NowPlayingView, { props: props(snapshotWith(summary())) });
    expect(upNextRows()).toEqual([
      ["#3", "false"],
      ["#4", "false"],
      ["#1", "true"],
      ["#2", "true"],
      ["#3", "true"],
    ]);
    expect(screen.getAllByTestId("np-up-next-row")[2]!.textContent).toMatch(/YouTube Music suggestion · \S/);
    expect(screen.getByTestId("np-up-next-source").textContent).toBe("Then YouTube Music autoplay · Discover");
  });

  it("adds no suggestions before queued tracks that are not in the preview", () => {
    render(NowPlayingView, { props: props(snapshotWith(summary({ loadedCount: 40 }))) });
    expect(upNextRows().every(([, suggestion]) => suggestion === "false")).toBe(true);
    expect(upNextRows()).toHaveLength(2);
  });

  it("says when autoplay is off or on a radio, and omits the note when the current place is unknown", () => {
    render(NowPlayingView, { props: props(snapshotWith(summary({ officialUpcoming: { status: "off" } }))) });
    expect(screen.getByTestId("np-up-next-source").textContent).toBe("Autoplay is off, so playback stops after the queue");
    cleanup();
    render(NowPlayingView, { props: props(snapshotWith(summary({ officialUpcoming: { status: "radio" } }))) });
    expect(screen.getByTestId("np-up-next-source").textContent).toBe("This radio keeps adding tracks");
    cleanup();
    render(NowPlayingView, { props: props(snapshotWith(summary({ currentEntryId: null }))) });
    expect(screen.queryAllByTestId("np-up-next-row")).toHaveLength(0);
    expect(screen.queryByTestId("np-up-next-source")).toBeNull();
  });

  const startReceipt = (status: CommandReceipt["status"], commandId = "sel-1"): CommandReceipt => ({
    commandId,
    op: "selectQueueEntry",
    status,
    acceptedAtMs: 0,
    deduplicated: false,
  });
  const withReceipt = (snapshot: AppSnapshot, lastReceipt: CommandReceipt | null): AppSnapshot => ({
    ...snapshot,
    pending: { ...snapshot.pending, lastReceipt },
  });
  const startButtons = () =>
    screen.getAllByTestId("np-up-next-row").flatMap((row) => within(row).queryAllByTestId("np-up-next-start"));

  it("starts a queued row through the queue's handler; suggestions offer no start", async () => {
    const p = props(snapshotWith(summary()));
    render(NowPlayingView, { props: p });
    const rows = screen.getAllByTestId("np-up-next-row");
    expect(rows.map((row) => within(row).queryAllByRole("button").length)).toEqual([1, 1, 0, 0, 0]);
    await fireEvent.click(startButtons()[1]!);
    expect(p.onSelectEntry).toHaveBeenCalledTimes(1);
    expect(p.onSelectEntry).toHaveBeenCalledWith(summary().previewEntries![3]);
  });

  it("keeps rows reachable but inert while the queue cannot start them, or the track is unavailable", async () => {
    const preview = entries(4);
    preview[2] = { ...preview[2]!, availability: "unavailable", track: { ...preview[2]!.track, availability: "unavailable" } };
    const blocked = { ...props(snapshotWith(summary())), selectGate: { enabled: false, reason: "Reconnecting" } };
    render(NowPlayingView, { props: blocked });
    const [first] = startButtons();
    expect(first!.getAttribute("aria-disabled")).toBe("true");
    expect(first!.title).toBe("Reconnecting");
    expect(first!.hasAttribute("disabled")).toBe(false);
    await fireEvent.click(first!);
    expect(blocked.onSelectEntry).not.toHaveBeenCalled();
    cleanup();

    const open = props(snapshotWith(summary({ previewEntries: preview })));
    render(NowPlayingView, { props: open });
    const unavailable = startButtons()[0]!;
    expect(unavailable.getAttribute("aria-disabled")).toBe("true");
    expect(unavailable.textContent).toContain("Unavailable");
    await fireEvent.click(unavailable);
    expect(open.onSelectEntry).not.toHaveBeenCalled();
    expect(startButtons()[1]!.getAttribute("aria-disabled")).toBeNull();
  });

  it("shows the wait on the pressed row and never re-arms it from a late or foreign receipt", async () => {
    const snapshot = snapshotWith(summary());
    const p = { ...props(snapshot), onSelectEntry: vi.fn(async () => startReceipt("accepted")) };
    const view = render(NowPlayingView, { props: p });
    await fireEvent.click(startButtons()[0]!);
    await tick();
    expect(startButtons()[0]!.getAttribute("aria-busy")).toBe("true");
    expect(startButtons()[1]!.getAttribute("aria-busy")).toBe("false");

    // Another command's outcome leaves the wait alone; this command's failure ends it.
    await view.rerender({ ...p, snapshot: withReceipt(snapshot, startReceipt("failed", "other")) });
    expect(startButtons()[0]!.getAttribute("aria-busy")).toBe("true");
    await view.rerender({ ...p, snapshot: withReceipt(snapshot, startReceipt("failed")) });
    expect(startButtons()[0]!.getAttribute("aria-busy")).toBe("false");
    // The same receipt delivered again, or a stale accepted one, does not bring the wait back.
    await view.rerender({ ...p, snapshot: withReceipt(snapshot, startReceipt("accepted")) });
    expect(startButtons()[0]!.getAttribute("aria-busy")).toBe("false");
  });

  it("keeps focus in Up next when the started row leaves it", async () => {
    const first = snapshotWith(summary());
    const p = props(first);
    const view = render(NowPlayingView, { props: p });
    const pressed = startButtons()[0]!;
    pressed.focus();
    await fireEvent.click(pressed);
    const moved = summary({ currentEntryId: "e-2" });
    await view.rerender({ ...p, snapshot: snapshotWith(moved) });
    await tick();
    await tick();
    expect(document.activeElement).not.toBe(document.body);
    expect(document.activeElement).toBe(startButtons()[0]!);
  });

  it("leads to the queue from the heading and names the next track on one line", async () => {
    const p = props(snapshotWith(summary()));
    render(NowPlayingView, { props: p });
    const link = screen.getByTestId("np-up-next-open-queue");
    expect(link.closest("h2")).toBeTruthy();
    expect(link.getAttribute("aria-label") ?? link.textContent).toContain("Up next");
    await fireEvent.click(link);
    expect(p.onOpenQueue).toHaveBeenCalledTimes(1);
    const next = summary().previewEntries![2]!.track;
    const line = screen.getByTestId("np-up-next-line");
    expect(line.textContent?.trim()).toBe(`Next: ${next.title} · ${next.artists!.map((a) => a.name).join(" / ")}`);
    // The line starts that entry the way its row does.
    await fireEvent.click(within(line).getByTestId("np-up-next-line-start"));
    expect(p.onSelectEntry).toHaveBeenCalledWith(summary().previewEntries![2]);
    cleanup();

    // A suggestion next: named as one, with nothing to press.
    render(NowPlayingView, { props: props(snapshotWith(summary({ currentEntryId: "e-3" }))) });
    const suggested = screen.getByTestId("np-up-next-line");
    expect(within(suggested).queryByRole("button")).toBeNull();
    expect(suggested.textContent).toContain("YouTube Music suggestion · ");
    cleanup();

    // Nothing queued after this track: the line says where playback goes instead of naming one.
    render(NowPlayingView, {
      props: props(snapshotWith(summary({ currentEntryId: "e-3", officialUpcoming: { status: "off" } }))),
    });
    expect(screen.getByTestId("np-up-next-line").textContent?.trim()).toBe("Autoplay is off, so playback stops after the queue");
  });

  it("fades the cover in on a track change, not on first show or under reduced motion", async () => {
    const first = snapshotWith(summary());
    const next = { ...first, player: { ...first.player, trackInstanceId: "ti-2" } };
    const wrap = () => screen.getByTestId("np-cover-wrap");
    // jsdom has no image decoding: hold it until the test lets the new cover finish.
    let decoded = () => {};
    Object.defineProperty(HTMLImageElement.prototype, "decode", {
      configurable: true,
      value: () => new Promise<void>((resolve) => (decoded = resolve)),
    });
    try {
      const view = render(NowPlayingView, { props: props(first) });
      expect(wrap().classList.contains("np-reveal")).toBe(false);
      await view.rerender(props(next));
      await tick();
      await tick();
      expect(wrap().classList.contains("np-wait")).toBe(true);
      decoded();
      // Well before the 500 ms cap, so only the decode can reveal it.
      await vi.waitFor(() => expect(wrap().classList.contains("np-reveal")).toBe(true), { timeout: 200 });
    } finally {
      delete (HTMLImageElement.prototype as { decode?: unknown }).decode;
    }
    cleanup();

    // Artwork the page reported but core has not cached yet: wait for its image, then fade.
    const remoteOnly = { ...next, player: { ...next.player, currentTrack: { ...next.player.currentTrack!, cover: { remoteUrl: "https://example.invalid/c.jpg" } } } };
    const cached = { ...remoteOnly, player: { ...remoteOnly.player, currentTrack: { ...remoteOnly.player.currentTrack, cover: { assetId: "asset-1" } } } };
    const waiting = render(NowPlayingView, { props: props({ ...first, mockMode: false }) });
    await waiting.rerender(props({ ...remoteOnly, mockMode: false }));
    await tick();
    expect(wrap().classList.contains("np-wait")).toBe(true);
    await waiting.rerender(props({ ...cached, mockMode: false }));
    await vi.waitFor(() => expect(wrap().classList.contains("np-reveal")).toBe(true));
    cleanup();

    const reduced = render(NowPlayingView, { props: props(first, true) });
    await reduced.rerender(props(next, true));
    expect(wrap().classList.contains("np-wait")).toBe(false);
    expect(wrap().classList.contains("np-reveal")).toBe(false);
  });

  it("never reveals the previous track's held cover under the new title", async () => {
    vi.useFakeTimers();
    try {
      const wrap = () => screen.getByTestId("np-cover-wrap");
      const img = () => wrap().querySelector("img")?.getAttribute("src") ?? null;
      const first = { ...snapshotWith(summary()), mockMode: false };
      const a = first.player.currentTrack!;
      const artA = { cover: { assetId: "asset-a", remoteUrl: "https://example.invalid/a.jpg" }, seed: a.videoId, label: a.title };
      const b = { ...a, videoId: "vid-b", title: "B", cover: { remoteUrl: "https://example.invalid/b.jpg" } };
      const onB = (cover: { assetId?: string; remoteUrl: string }) => ({
        ...first,
        player: { ...first.player, trackInstanceId: "ti-2", currentTrack: { ...b, cover } },
      });
      const remoteB = onB(b.cover);
      const cachedB = onB({ ...b.cover, assetId: "asset-b" });
      const artOf = (s: AppSnapshot) => ({ cover: s.player.currentTrack!.cover, seed: "vid-b", label: "B" });
      const view = render(NowPlayingView, { props: { ...props({ ...first, player: { ...first.player, currentTrack: { ...a, cover: artA.cover } } }), artwork: artA } });

      // App holds A's cover while B's is cached: the stage keeps waiting past the old 500 ms cap.
      await view.rerender({ ...props(remoteB), artwork: artA });
      await vi.advanceTimersByTimeAsync(600);
      expect(wrap().classList.contains("np-wait")).toBe(true);
      // B's own cover lands: it is what fades in.
      await view.rerender({ ...props(cachedB), artwork: artOf(cachedB) });
      await vi.advanceTimersByTimeAsync(0);
      expect(wrap().classList.contains("np-reveal")).toBe(true);
      expect(img()).toBe("app://covers/asset-b");
      cleanup();

      // The hold runs out first: the stage fades in with B's placeholder, never with A's cover.
      const late = render(NowPlayingView, { props: { ...props(first), artwork: artA } });
      await late.rerender({ ...props(remoteB), artwork: artA });
      await vi.advanceTimersByTimeAsync(ARTWORK_HOLD_MS);
      expect(wrap().classList.contains("np-reveal")).toBe(true);
      expect(img()).toBeNull();
      expect(within(wrap()).getByTestId("cover-placeholder")).toBeTruthy();
      cleanup();

      // No stand-in (a second skip): the stage waits the short while for B's own cover...
      const skipped = render(NowPlayingView, { props: { ...props(first), artwork: artA } });
      await skipped.rerender({ ...props(remoteB), artwork: artOf(remoteB) });
      await vi.advanceTimersByTimeAsync(200);
      expect(wrap().classList.contains("np-wait")).toBe(true);
      await skipped.rerender({ ...props(cachedB), artwork: artOf(cachedB) });
      await vi.advanceTimersByTimeAsync(0);
      expect(wrap().classList.contains("np-reveal")).toBe(true);
      expect(img()).toBe("app://covers/asset-b");
      cleanup();
      // ...and fades in its placeholder when the cover is not there by then.
      const slow = render(NowPlayingView, { props: { ...props(first), artwork: artA } });
      await slow.rerender({ ...props(remoteB), artwork: artOf(remoteB) });
      await vi.advanceTimersByTimeAsync(499);
      expect(wrap().classList.contains("np-wait")).toBe(true);
      await vi.advanceTimersByTimeAsync(1);
      expect(wrap().classList.contains("np-reveal")).toBe(true);
      expect(img()).toBeNull();
      cleanup();

      // A stand-in that ends after the short wait (App's hold ran out) reveals the placeholder then.
      const ended = render(NowPlayingView, { props: { ...props(first), artwork: artA } });
      await ended.rerender({ ...props(remoteB), artwork: artA });
      await vi.advanceTimersByTimeAsync(1_000);
      expect(wrap().classList.contains("np-wait")).toBe(true);
      await ended.rerender({ ...props(remoteB), artwork: artOf(remoteB) });
      await vi.advanceTimersByTimeAsync(0);
      expect(wrap().classList.contains("np-reveal")).toBe(true);
      expect(img()).toBeNull();
      cleanup();

      // Reduced motion hides nothing: the held cover stays in view, as in the player bar.
      const reduced = render(NowPlayingView, { props: { ...props(first, true), artwork: artA } });
      await reduced.rerender({ ...props(remoteB, true), artwork: artA });
      expect(wrap().classList.contains("np-wait")).toBe(false);
      expect(img()).toBe("app://covers/asset-a");
    } finally {
      vi.useRealTimers();
    }
  });
});
