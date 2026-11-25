import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import {
  FakeCoreClient,
  type AppSnapshot,
  type CommandReceipt,
  type LyricsResult,
  type PlayerCommandPayload,
  type PlayerOp,
} from "@ytmcp/protocol";
import { afterEach, describe, expect, it, vi } from "vitest";
import { nextPositionTickMs } from "../../position-clock.js";
import LyricsView from "../LyricsView.svelte";
import NowPlayingView from "../NowPlayingView.svelte";
import PlayerBar from "../PlayerBar.svelte";
import TrackActions from "../TrackActions.svelte";

const originalScrollTo = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollTo");

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  if (originalScrollTo) Object.defineProperty(HTMLElement.prototype, "scrollTo", originalScrollTo);
  else Reflect.deleteProperty(HTMLElement.prototype, "scrollTo");
});

const base: AppSnapshot = new FakeCoreClient("normal").subscribeSnapshot(() => {}).snapshot;
const overlay = (pendingCount = 0) => ({
  pendingCount,
  activeOp: null,
  desiredPlayback: null,
  desiredPositionMs: null,
  desiredVolume: null,
  lastError: null,
});

function receipt(commandId: string, op: PlayerOp, status: CommandReceipt["status"]): CommandReceipt {
  return { commandId, op, status, acceptedAtMs: 0, observedAtMs: null, deduplicated: false, error: null };
}

/** A command path whose replies the test releases one by one, in any order. */
function controlledCommands() {
  const sent: { op: PlayerOp; args: unknown; reply: (r: CommandReceipt | null) => void }[] = [];
  const onCommand = vi.fn(
    (op: PlayerOp, args?: PlayerCommandPayload["args"]) =>
      new Promise<CommandReceipt | null>((resolve) => {
        sent.push({ op, args: args ?? null, reply: resolve });
      })
  );
  return { sent, onCommand };
}

function withPlayer(player: Partial<AppSnapshot["player"]>, lastReceipt: CommandReceipt | null = null): AppSnapshot {
  return { ...base, player: { ...base.player, ...player }, pending: { ...base.pending, lastReceipt } };
}

function renderBar(snapshot: AppSnapshot, onCommand: ReturnType<typeof controlledCommands>["onCommand"]) {
  return render(PlayerBar, {
    props: {
      snapshot,
      pendingOverlay: overlay(),
      interpolatedPositionMs: 0,
      onCommand,
      onOpenNowPlaying: () => {},
      onToggleLyrics: () => {},
    },
  });
}

describe("Player controls answer the press at once", () => {
  it("shows the asked play state as pending, ignores a late confirmation of an older press, and settles on the observed state", async () => {
    const { sent, onCommand } = controlledCommands();
    const view = renderBar(withPlayer({ playback: "playing" }), onCommand);
    const play = () => screen.getByTestId("btn-play-pause");

    await fireEvent.click(play());
    // The press says what it does: an explicit pause, shown at once and marked as waiting.
    expect(sent.map((s) => s.op)).toEqual(["pause"]);
    expect(play().getAttribute("aria-label")).toBe("Play");
    expect(play().hasAttribute("data-pending")).toBe(true);
    await view.rerender({ pendingOverlay: overlay(1) });

    // A second press before the first settles asks for the opposite of what is shown.
    await fireEvent.click(play());
    expect(sent.map((s) => s.op)).toEqual(["pause", "play"]);
    expect(play().getAttribute("aria-label")).toBe("Pause");
    await view.rerender({ pendingOverlay: overlay(2) });
    sent[0]!.reply(receipt("c1", "pause", "dispatched"));
    sent[1]!.reply(receipt("c2", "play", "accepted"));
    await Promise.resolve();

    // The first pause lands: the page reports paused and confirms c1. The newer press still wins.
    await view.rerender({ snapshot: withPlayer({ playback: "paused" }, receipt("c1", "pause", "observed")), pendingOverlay: overlay(1) });
    expect(play().getAttribute("aria-label")).toBe("Pause");
    expect(play().hasAttribute("data-pending")).toBe(true);

    // The play lands: observed first, then its receipt; the control settles without a flip.
    await view.rerender({ snapshot: withPlayer({ playback: "playing" }, receipt("c1", "pause", "observed")) });
    expect(play().getAttribute("aria-label")).toBe("Pause");
    await view.rerender({ snapshot: withPlayer({ playback: "playing" }, receipt("c2", "play", "observed")), pendingOverlay: overlay(0) });
    expect(play().getAttribute("aria-label")).toBe("Pause");
    expect(play().hasAttribute("data-pending")).toBe(false);
  });

  it("returns to the observed state at once when the command fails or its outcome is unknown", async () => {
    const { sent, onCommand } = controlledCommands();
    const view = renderBar(withPlayer({ playback: "paused" }), onCommand);
    const play = () => screen.getByTestId("btn-play-pause");
    await fireEvent.click(play());
    expect(play().getAttribute("aria-label")).toBe("Pause");
    await view.rerender({ pendingOverlay: overlay(1) });
    sent[0]!.reply(receipt("c1", "play", "accepted"));
    await Promise.resolve();
    expect(play().getAttribute("aria-label")).toBe("Pause");
    await view.rerender({ snapshot: withPlayer({ playback: "paused" }, receipt("c1", "play", "indeterminate")), pendingOverlay: overlay(0) });
    expect(play().getAttribute("aria-label")).toBe("Play");
    expect(play().hasAttribute("data-pending")).toBe(false);

    // A reply that is already a failure (or no reply at all) lets go as well.
    await fireEvent.click(play());
    sent[1]!.reply(null);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(play().getAttribute("aria-label")).toBe("Play");
  });

  it("steps shuffle and repeat from the asked value, so quick presses never send the same mode twice", async () => {
    const { sent, onCommand } = controlledCommands();
    const view = renderBar(withPlayer({ shuffle: false, repeat: "off" }), onCommand);
    await view.rerender({ snapshot: { ...withPlayer({ shuffle: false, repeat: "off" }), capabilities: { ...base.capabilities, shuffle: "supported", repeat: "supported" } } });
    const shuffle = screen.getByTestId("btn-shuffle");
    const repeat = screen.getByTestId("btn-repeat");

    await fireEvent.click(shuffle);
    expect(shuffle.getAttribute("aria-pressed")).toBe("true");
    expect(shuffle.hasAttribute("data-pending")).toBe(true);
    await fireEvent.click(repeat);
    await fireEvent.click(repeat);
    expect(sent.map((s) => [s.op, s.args])).toEqual([
      ["setShuffle", { shuffle: true }],
      ["setRepeat", { repeat: "all" }],
      ["setRepeat", { repeat: "one" }],
    ]);
    expect(repeat.getAttribute("aria-label")).toBe("Repeat: one");
  });

  it("marks next and previous as waiting on the button itself while their command is out", async () => {
    const { onCommand } = controlledCommands();
    const view = renderBar(withPlayer({ playback: "playing" }), onCommand);
    await view.rerender({ pendingOverlay: { ...overlay(1), activeOp: "next" } });
    expect(screen.getByTestId("btn-next").hasAttribute("data-pending")).toBe(true);
    expect(screen.getByTestId("btn-prev").hasAttribute("data-pending")).toBe(false);
  });
});

describe("Track menu while a write is pending", () => {
  it("keeps Play, queueing and navigation available; only like and add to playlist wait", () => {
    const track = base.player.currentTrack!;
    render(TrackActions, {
      props: {
        track,
        liked: false,
        enqueueGate: { enabled: true, reason: null },
        likeGate: { enabled: true, reason: null },
        playlistGate: { enabled: true, reason: null },
        canOpenAlbum: false,
        canOpenArtist: false,
        pending: true,
        playlistsView: { kind: "ready", cached: false, error: null },
        playlists: [],
        onClose: vi.fn(),
        onPlay: vi.fn(),
        onEnqueue: vi.fn(),
        onSetLike: vi.fn(),
        onRequestPlaylists: vi.fn(),
        onAddToPlaylist: vi.fn(),
        onOpenAlbum: vi.fn(),
        onOpenArtist: vi.fn(),
      },
    });
    const disabled = (id: string) => (screen.getByTestId(id) as HTMLButtonElement).disabled;
    expect(disabled("action-play")).toBe(false);
    expect(disabled("action-play-next")).toBe(false);
    expect(disabled("action-add-to-queue")).toBe(false);
    expect(disabled("action-like")).toBe(true);
    expect(disabled("action-add-to-playlist")).toBe(true);
  });
});

const LINES: LyricsResult = {
  videoId: base.player.currentTrack!.videoId,
  kind: "synced",
  source: "lrclib",
  language: null,
  matchBasis: null,
  fetchedAtMs: 0,
  fileOffsetMs: 0,
  userOffsetMs: 0,
  syncedLines: Array.from({ length: 30 }, (_, i) => ({ timeMs: i * 1000, text: `Line ${i}` })),
  plainText: null,
  error: null,
};

describe("Lyrics placement", () => {
  function renderLyrics(positionMs: number) {
    const scrollTo = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "scrollTo", { value: scrollTo, configurable: true, writable: true });
    const props = {
      track: base.player.currentTrack!,
      lyrics: LINES,
      loading: false,
      interpolatedPositionMs: positionMs,
      canSeek: true,
      mockMode: true,
      smoothScroll: true,
      accent: null,
      onSeekMs: vi.fn(),
      onStepOffset: () => {},
      onResetOffset: () => {},
    };
    const view = render(LyricsView, { props });
    const at = (ms: number) => view.rerender({ ...props, interpolatedPositionMs: ms });
    return { scrollTo, at };
  }

  it("places the current line at once on open, even while paused, and glides only to the next line", async () => {
    const { scrollTo, at } = renderLyrics(12_500);
    expect(scrollTo).toHaveBeenCalledTimes(1);
    expect(scrollTo.mock.lastCall![0]).toMatchObject({ behavior: "auto" });

    await at(13_500);
    expect(scrollTo).toHaveBeenCalledTimes(2);
    expect(scrollTo.mock.lastCall![0]).toMatchObject({ behavior: "smooth" });

    // A seek far away jumps; the same line again does not scroll at all.
    await at(25_200);
    expect(scrollTo.mock.lastCall![0]).toMatchObject({ behavior: "auto" });
    await at(25_900);
    expect(scrollTo).toHaveBeenCalledTimes(3);
  });
});

describe("Now Playing layout across a track change", () => {
  it("keeps the lyrics layout while the next track's lyrics load, without showing the old lines", async () => {
    const track = base.player.currentTrack!;
    const next = { ...track, videoId: "other-video", title: "Other" };
    const props = {
      snapshot: base,
      interpolatedPositionMs: 1500,
      liked: null,
      likeGate: { enabled: false, reason: null },
      likePending: false,
      canOpenAlbum: false,
      canOpenArtist: false,
      accent: null,
      motionReduced: true,
      lyrics: LINES,
      onToggleLike: () => {},
      onOpenActions: () => {},
      onOpenAlbum: () => {},
      onOpenArtist: () => {},
      onOpenLyrics: () => {},
    };
    const view = render(NowPlayingView, { props });
    const stage = () => document.querySelector(".np-stage")!;
    expect(stage().classList.contains("has-lyrics")).toBe(true);

    const moved = { ...base, player: { ...base.player, currentTrack: next, trackInstanceId: "ti-next" } };
    await view.rerender({ ...props, snapshot: moved, lyrics: null, holdLyricsLayout: true });
    expect(stage().classList.contains("has-lyrics")).toBe(true);
    expect(screen.queryAllByTestId("np-lyrics-line")).toHaveLength(0);
    // An empty slot keeps the lyrics rows their size (several tiers size a row by its content).
    expect(screen.getByTestId("np-lyrics-hold").getAttribute("aria-hidden")).toBe("true");

    // The new track has none: the layout changes once, when that is known.
    await view.rerender({ ...props, snapshot: moved, lyrics: null, holdLyricsLayout: false });
    expect(stage().classList.contains("has-lyrics")).toBe(false);
    expect(screen.queryByTestId("np-lyrics-hold")).toBeNull();
  });
});

describe("Position clock", () => {
  it("wakes on the next whole second or the next lyric line, whichever comes first, in wall time, and at least every 200 ms", () => {
    expect(nextPositionTickMs(12_900, 1)).toBe(108);
    expect(nextPositionTickMs(12_900, 1, [5_000, 12_950, 14_000])).toBe(58);
    expect(nextPositionTickMs(12_800, 2)).toBe(108);
    // Far from a boundary the seek fill still moves every 200 ms.
    expect(nextPositionTickMs(12_100, 1)).toBe(200);
    // A boundary just reached never spins the timer.
    expect(nextPositionTickMs(12_999.9, 1)).toBeGreaterThanOrEqual(16);
  });
});
