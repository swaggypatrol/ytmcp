<script lang="ts">
  import type { AppSnapshot, CommandReceipt, LyricsResult, QueueEntry, Track } from "@ytmcp/protocol";
  import { tick, untrack } from "svelte";
  import type { Gate } from "../capabilities.js";
  import { t } from "../i18n/index.svelte.js";
  import { activeLyricIndex } from "../lyrics.js";
  import { isTerminalCommandStatus } from "../seek-preview.js";
  import type { TrackAccent } from "../theme/accent.js";
  import { ARTWORK_HOLD_MS } from "../ambient/held-artwork.svelte.js";
  import { artworkSrc, type Artwork } from "../cover.js";
  import Cover from "./Cover.svelte";
  import { availabilityBadge, detectCjkLangHint, formatArtists, isPlayable } from "./format.js";
  import HeroWash from "./HeroWash.svelte";
  import Icon from "./Icon.svelte";
  import QualityBadge from "./QualityBadge.svelte";

  interface Props {
    snapshot: AppSnapshot | null;
    /** The shown playback position: the seek preview while one holds, as in the player bar. */
    interpolatedPositionMs: number;
    liked: boolean | null;
    likeGate: Gate;
    likePending: boolean;
    canOpenAlbum: boolean;
    canOpenArtist: boolean;
    accent: TrackAccent | null;
    /** Reduced motion is on (setting or system); the track-change fade is skipped. */
    motionReduced?: boolean;
    /** Lyrics the app already holds for the current track; no synced lyrics, no preview. */
    lyrics?: LyricsResult | null;
    /**
     * The next track's lyrics are on their way and the last ones had synced lines: the stage keeps
     * its lyrics layout (with no lines in it) until the answer, so a track change does not re-lay it out twice.
     */
    holdLyricsLayout?: boolean;
    /** The cover to show: App's held artwork, which keeps the previous one while a new cover is cached. Defaults to the track's own. */
    artwork?: Artwork | null;
    /** What the track plays from (a playlist or album name), only when it is really known. */
    playingFrom?: string | null;
    onToggleLike: () => void;
    onOpenActions: () => void;
    /** Opens the playlist chooser for the track; the button shows only while `playlistGate` allows adding. */
    playlistGate?: Gate;
    onAddToPlaylist?: () => void;
    onOpenAlbum: (albumId: string) => void;
    onOpenArtist: (artistId: string) => void;
    onOpenLyrics: () => void;
    /** Opens the Queue page; the Up next heading leads there. */
    onOpenQueue?: () => void;
    /** Whether a queue entry can be started now: the same gate the Queue page applies. */
    selectGate?: Gate;
    /** Starts an explicit queue entry: the Queue page's own handler, passed by the app. */
    onSelectEntry?: (entry: QueueEntry) => Promise<CommandReceipt | null>;
  }

  let {
    snapshot,
    interpolatedPositionMs,
    liked,
    likeGate,
    likePending,
    canOpenAlbum,
    canOpenArtist,
    accent,
    motionReduced = false,
    lyrics = null,
    holdLyricsLayout = false,
    artwork,
    playingFrom = null,
    onToggleLike,
    onOpenActions,
    playlistGate = { enabled: false, reason: null },
    onAddToPlaylist,
    onOpenAlbum,
    onOpenArtist,
    onOpenLyrics,
    onOpenQueue,
    selectGate = { enabled: false, reason: null },
    onSelectEntry,
  }: Props = $props();

  const player = $derived(snapshot?.player ?? null);
  const track = $derived(player?.currentTrack ?? null);
  const albumId = $derived(track?.album?.id ?? track?.album?.browseId ?? null);

  interface PreviewLine {
    readonly index: number;
    readonly text: string;
    /** Lines from the current one: 0 is current, -1 the line before, 1 the next. */
    readonly distance: number;
  }

  /**
   * Synced lines round the current one, keyed by line. Each line sits in a one-row slot placed by
   * its distance (now-playing.css), so a new current line slides the column by one row and fades
   * the ends; nothing reflows. One hidden line past each visible end lets lines fade in and out.
   * Before the first line starts, the opening lines wait as "next".
   */
  const previewLines = $derived.by((): PreviewLine[] => {
    if (!track || lyrics?.kind !== "synced" || lyrics.videoId !== track.videoId) return [];
    const lines = lyrics.syncedLines ?? [];
    if (lines.length === 0) return [];
    const active = activeLyricIndex(lyrics, interpolatedPositionMs);
    const out: PreviewLine[] = [];
    for (let i = Math.max(0, active - 3); i <= Math.min(lines.length - 1, active + 4); i += 1) {
      out.push({ index: i, text: lines[i]!.text.trim() || "♪", distance: i - active });
    }
    return out;
  });

  const heroArt = $derived(track ? [{ cover: track.cover, seed: track.videoId, label: track.title }] : []);
  /** How many tracks Up next lists; below the ultrawide tier only the first three show. */
  const UP_NEXT_COUNT = 5;
  /** Longest wait for the new cover before the fade starts anyway, unless a held cover stands in. */
  const REVEAL_WAIT_MS = 500;
  /** A start still waiting for its outcome when no receipt or queue change settles it. */
  const START_SETTLE_TIMEOUT_MS = 15_000;

  interface UpNextRow {
    readonly key: string;
    readonly track: Track;
    readonly suggestion: boolean;
    /** The queue entry behind an explicit row; YouTube Music's suggestions have none. */
    readonly entry: QueueEntry | null;
  }

  const queue = $derived(snapshot?.queueSummary ?? null);
  const queueEntries = $derived(queue?.previewEntries ?? []);
  const queueIdx = $derived(
    queue?.currentEntryId ? queueEntries.findIndex((e) => e.entryId === queue.currentEntryId) : -1
  );
  const official = $derived(queue?.officialUpcoming ?? null);
  const selectedFilter = $derived(queue?.filters?.find((f) => f.selected)?.label ?? null);

  /**
   * The explicit tracks after the current occurrence; YouTube Music's own suggestions fill the
   * rest only when the preview reaches the end of the loaded list, so none jump ahead of queued
   * tracks.
   */
  const upNextRows = $derived.by((): UpNextRow[] => {
    if (!queue || queueIdx < 0) return [];
    const explicit = queueEntries
      .slice(queueIdx + 1, queueIdx + 1 + UP_NEXT_COUNT)
      .map((e) => ({ key: e.entryId, track: e.track, suggestion: false, entry: e }));
    const reachesEnd = (queue.previewStart ?? 0) + queueEntries.length >= queue.loadedCount;
    if (!reachesEnd || explicit.length >= UP_NEXT_COUNT || official?.status !== "observed") return explicit;
    const fill = (official.tracks ?? [])
      .slice(0, UP_NEXT_COUNT - explicit.length)
      .map((track, i) => ({ key: `official:${track.videoId}:${i}`, track, suggestion: true, entry: null }));
    return [...explicit, ...fill];
  });

  const upNextSource = $derived.by((): string | null => {
    if (queueIdx < 0) return null;
    switch (official?.status) {
      case "observed":
      case "loading":
        return selectedFilter ? `${t.nowPlaying.upNextAutoplay} · ${selectedFilter}` : t.nowPlaying.upNextAutoplay;
      case "off":
        return t.nowPlaying.upNextAutoplayOff;
      case "radio":
        return t.nowPlaying.upNextRadio;
      default:
        return null;
    }
  });

  /** The short-window line: the next track, or the source note when what follows is unknown. */
  const nextRow = $derived(upNextRows[0] ?? null);
  const nextNote = $derived(queueIdx >= 0 ? (upNextSource ?? t.nowPlaying.upNextEmpty) : null);

  /** A row's second line: why a queued track cannot play, else its artists (marked when suggested). */
  function rowSubline(row: UpNextRow): string {
    const badge = row.entry ? availabilityBadge(row.track) : null;
    if (badge) return badge;
    return row.suggestion ? `${t.nowPlaying.upNextSuggestion} · ${formatArtists(row.track)}` : formatArtists(row.track);
  }

  /** The one start this view sent and is waiting on; a newer press replaces it. */
  interface PendingStart {
    readonly entryId: string;
    readonly currentEntryId: string | null;
    readonly revision: number | null;
    readonly commandId: string | null;
  }
  let starting = $state.raw<PendingStart | null>(null);

  /** Why a row cannot be started now, or null when it can. */
  function startBlocked(track: Track): string | null {
    if (!isPlayable(track)) return availabilityBadge(track);
    return selectGate.enabled ? null : selectGate.reason;
  }

  function canStart(track: Track): boolean {
    return isPlayable(track) && selectGate.enabled;
  }

  /** Starts an explicit row with the Queue page's handler; the row shows the wait itself. */
  async function startEntry(entry: QueueEntry) {
    if (!onSelectEntry || !canStart(entry.track) || starting?.entryId === entry.entryId) return;
    const started: PendingStart = {
      entryId: entry.entryId,
      currentEntryId: queue?.currentEntryId ?? null,
      revision: queue?.queueRevision ?? null,
      commandId: null,
    };
    starting = started;
    const receipt = await onSelectEntry(entry);
    if (starting !== started) return;
    starting = !receipt || isTerminalCommandStatus(receipt.status) ? null : { ...started, commandId: receipt.commandId };
  }

  // Settled once: on this command's terminal receipt, or when the queue moved on (a new current
  // entry or revision). Nothing re-arms it, so a late receipt never flips the row back.
  $effect(() => {
    const p = starting;
    if (!p) return;
    const receipt = snapshot?.pending.lastReceipt ?? null;
    const receiptDone = p.commandId !== null && receipt?.commandId === p.commandId && isTerminalCommandStatus(receipt.status);
    const moved = (queue?.currentEntryId ?? null) !== p.currentEntryId || (queue?.queueRevision ?? null) !== p.revision;
    if (receiptDone || moved) starting = null;
  });

  $effect(() => {
    if (!starting) return;
    const timer = setTimeout(() => (starting = null), START_SETTLE_TIMEOUT_MS);
    return () => clearTimeout(timer);
  });

  let upNextEl = $state<HTMLElement | null>(null);
  let refocusUpNext = false;

  // A started row becomes the current track and leaves Up next; when focus was in the block,
  // it moves to the first row that can take it (the short line or the heading when rows are
  // hidden) instead of falling to the page.
  $effect.pre(() => {
    void upNextRows;
    const active = document.activeElement;
    refocusUpNext = !!upNextEl && !!active && upNextEl.contains(active);
  });
  $effect(() => {
    void upNextRows;
    if (!refocusUpNext) return;
    refocusUpNext = false;
    void tick().then(() => {
      const root = upNextEl;
      const active = document.activeElement;
      if (!root || (active && active !== document.body && active.isConnected)) return;
      // In this order; a hidden candidate (rows on a short window) does not take focus.
      for (const selector of [".np-up-next-list .np-up-next-start", ".np-up-next-line-start", ".np-up-next-link"]) {
        const target = root.querySelector<HTMLElement>(selector);
        target?.focus();
        if (target && document.activeElement === target) return;
      }
    });
  });

  // Track change fade: hide the cover and title until the track's own artwork decodes, then fade
  // them in. Not on first show and not under reduced motion. While App still holds the previous
  // track's cover (a stand-in), the stage keeps waiting: it reveals with the track's own cover, or
  // with the placeholder once the stand-in ends (the hold ran out), never with the previous cover.
  const trackInstance = $derived(player?.trackInstanceId ?? null);
  const mockMode = $derived(snapshot?.mockMode ?? false);
  const ownArt = $derived(track ? { cover: track.cover, seed: track.videoId, label: track.title } : null);
  const coverSrc = $derived(ownArt ? artworkSrc(ownArt, mockMode) : null);
  const coverArt = $derived(artwork !== undefined ? artwork : ownArt);
  /** The image shown is not this track's: App's held cover while this one is cached. */
  const standIn = $derived(coverSrc === null && coverArt !== null && artworkSrc(coverArt, mockMode) !== null);
  /** The page reported artwork that core may still be caching (no local asset yet). */
  const coverExpected = $derived(coverSrc !== null || track?.cover?.remoteUrl != null);
  let reveal = $state<"idle" | "wait" | "in">("idle");
  /** The short wait since the track change has passed: an uncached cover no longer holds the reveal. */
  let capPassed = $state(false);
  // During a faded change the stand-in is never what is shown (reduced motion keeps it, as the bar does).
  const shownArt = $derived(reveal !== "idle" && standIn ? ownArt : coverArt);
  let coverWrap = $state<HTMLElement | null>(null);
  let lastInstance: string | null | undefined;
  let revealSeq = 0;

  function showRevealed(seq: number) {
    if (seq === revealSeq && reveal === "wait") reveal = "in";
  }

  $effect(() => {
    const instance = trackInstance;
    const previous = lastInstance;
    lastInstance = instance;
    if (previous === undefined || instance === previous) return;
    const seq = ++revealSeq;
    if (instance === null || untrack(() => motionReduced)) {
      reveal = "idle";
      return;
    }
    reveal = "wait";
    capPassed = false;
    // A slow decode or an uncached cover reveals after a short wait, but not over a stand-in
    // (the effect below reveals when it ends); the hold time is the last word.
    const cap = setTimeout(() => {
      capPassed = true;
      if (!standIn) showRevealed(seq);
    }, REVEAL_WAIT_MS);
    const hold = setTimeout(() => showRevealed(seq), ARTWORK_HOLD_MS);
    return () => {
      clearTimeout(cap);
      clearTimeout(hold);
    };
  });

  // While waiting: reveal once the track's own cover decodes; at once when it has no artwork, or
  // when a stand-in ends after the short wait; otherwise keep waiting (see the caps above).
  $effect(() => {
    if (reveal !== "wait") return;
    const seq = revealSeq;
    if (coverSrc === null) {
      if (!coverExpected || (capPassed && !standIn)) showRevealed(seq);
      return;
    }
    void tick().then(() => {
      const img = untrack(() => coverWrap)?.querySelector("img");
      if (!img || typeof img.decode !== "function") showRevealed(seq);
      else img.decode().then(() => showRevealed(seq), () => showRevealed(seq));
    });
  });

  const accentStyle = $derived(
    accent ? `--accent: ${accent.accent}; --accent-soft: ${accent.accentSoft}; --text-on-accent: ${accent.onAccent};` : ""
  );
</script>

<!-- Cover, title and lyrics laid out by the content tier (see now-playing.css); no seek bar here,
     the player bar has it. -->
<section class="np" aria-labelledby="now-playing-heading" data-testid="view-now-playing" style={accentStyle}>
  <HeroWash art={heroArt} {mockMode} full />

  {#if !track}
    <div class="state-block" data-testid="now-playing-empty">
      <Icon name="disc" class="w-10 h-10" style="color: var(--text-muted);" />
      <h1 id="now-playing-heading" class="type-lg font-semibold m-0">{t.nowPlaying.noTrack}</h1>
      <p class="type-sm m-0" style="color: var(--text-secondary);">{t.nowPlaying.noTrackHint}</p>
    </div>
  {:else}
    <div class="np-stage" class:has-lyrics={previewLines.length > 0 || holdLyricsLayout}>
      <div
        class="np-cover"
        class:np-wait={reveal === "wait"}
        class:np-reveal={reveal === "in"}
        bind:this={coverWrap}
        data-testid="np-cover-wrap"
      >
        <Cover cover={shownArt?.cover} seed={shownArt?.seed ?? track.videoId} label={shownArt?.label ?? track.title} {mockMode} class="np-cover-art" />
      </div>

      <div class="np-info">
        <div class="np-head" class:np-wait={reveal === "wait"} class:np-reveal={reveal === "in"}>
          {#if playingFrom}
            <p class="np-from" data-testid="np-playing-from">{t.nowPlaying.playingFrom.replace("{context}", playingFrom)}</p>
          {/if}
          <!-- Two lines at most, always the room of two, so no title moves what is below it; the full title is its tooltip. -->
          <h1 id="now-playing-heading" class="np-title" lang={detectCjkLangHint(track.title)} title={track.title} data-testid="now-playing-title">
            <span class="np-title-text">{track.title}</span>
          </h1>
          <p class="np-sub">
            {#each track.artists ?? [] as artist, i (`${artist.id ?? artist.name}::${i}`)}
              {#if i > 0}<span aria-hidden="true">{" / "}</span>{/if}
              {#if artist.id && canOpenArtist}
                <button type="button" class="link" onclick={() => onOpenArtist(artist.id!)} data-testid="np-artist-link">
                  {artist.name}
                </button>
              {:else}
                <span>{artist.name}</span>
              {/if}
            {/each}
            {#if track.album}
              <span aria-hidden="true">{" · "}</span>
              {#if albumId && canOpenAlbum}
                <button type="button" class="link" onclick={() => onOpenAlbum(albumId)} data-testid="np-album-link">
                  {track.album.title}
                </button>
              {:else}
                <span>{track.album.title}</span>
              {/if}
            {/if}
          </p>
        </div>

        <div class="np-actions">
          {#if likeGate.enabled}
            <button
              type="button"
              class="btn-icon"
              class:btn-icon-active={liked === true}
              aria-pressed={liked === true}
              aria-busy={likePending}
              data-pending={likePending || undefined}
              aria-label={liked === true ? t.track.unlike : t.track.like}
              title={liked === true ? t.track.unlike : t.track.like}
              onclick={onToggleLike}
              data-testid="np-like"
            >
              <Icon name="heart" filled={liked === true} />
            </button>
          {/if}
          {#if playlistGate.enabled && onAddToPlaylist}
            <button
              type="button"
              class="btn-icon"
              aria-label={t.track.addToPlaylist}
              title={t.track.addToPlaylist}
              onclick={onAddToPlaylist}
              data-testid="np-add-to-playlist"
            >
              <Icon name="plus" />
            </button>
          {/if}
          <button
            type="button"
            class="btn-icon"
            aria-label={t.track.actions}
            title={t.track.actions}
            onclick={onOpenActions}
            data-testid="np-actions"
          >
            <Icon name="more" />
          </button>
          <QualityBadge quality={player?.audioQuality} codec={player?.codec} bitrateKbps={player?.audioBitrateKbps} size="lg" class="np-quality" />
        </div>
      </div>

      {#if previewLines.length > 0}
        <!-- The whole preview opens the Lyrics page; the lines inside are its picture, not its name. -->
        <button
          type="button"
          class="np-lyrics"
          aria-label={t.nowPlaying.openLyrics}
          onclick={onOpenLyrics}
          lang={lyrics?.language ?? undefined}
          data-testid="np-lyrics-preview"
        >
          {#each previewLines as line (line.index)}
            <span
              class="np-line"
              class:is-current={line.distance === 0}
              class:is-near={Math.abs(line.distance) === 1}
              data-distance={line.distance}
              style="--np-d: {line.distance};"
              data-testid="np-lyrics-line"
            >{line.text}</span>
          {/each}
        </button>
      {:else if holdLyricsLayout}
        <!-- The preview's place, empty, while the next track's lyrics load: its rows keep their size. -->
        <div class="np-lyrics" aria-hidden="true" data-testid="np-lyrics-hold"></div>
      {/if}

      <!-- Up next: explicit queue rows start their entry; YouTube Music's suggestions are listed
           only. On a short regular window the rows give way to one line (now-playing.css). -->
      <aside class="np-up-next" aria-label={t.nowPlaying.upNext} bind:this={upNextEl} data-testid="np-up-next">
        <h2 class="np-up-next-heading">
          {#if onOpenQueue}
            <button
              type="button"
              class="np-up-next-link"
              aria-label={t.nowPlaying.upNextOpenQueue}
              title={t.nowPlaying.upNextOpenQueue}
              onclick={onOpenQueue}
              data-testid="np-up-next-open-queue"
            >
              {t.nowPlaying.upNext}<Icon name="chevronRight" class="np-up-next-chevron" />
            </button>
          {:else}
            {t.nowPlaying.upNext}
          {/if}
        </h2>
        <p class="np-up-next-line" data-testid="np-up-next-line">
          {#if nextRow}
            {@const text = t.nowPlaying.upNextLine.replace("{track}", `${nextRow.track.title} · ${rowSubline(nextRow)}`)}
            {#if nextRow.entry && onSelectEntry}
              {@const entry = nextRow.entry}
              <button
                type="button"
                class="np-up-next-line-start"
                aria-disabled={canStart(entry.track) ? undefined : "true"}
                aria-busy={starting?.entryId === entry.entryId}
                title={startBlocked(entry.track) ?? text}
                onclick={() => void startEntry(entry)}
                data-testid="np-up-next-line-start"
              >
                <span class="truncate" lang={detectCjkLangHint(nextRow.track.title)}>{text}</span>
                {@render waitSlot(starting?.entryId === entry.entryId)}
              </button>
            {:else}
              <span class="truncate" lang={detectCjkLangHint(nextRow.track.title)} title={text}>{text}</span>
            {/if}
          {:else if nextNote}
            <span class="truncate" title={nextNote}>{nextNote}</span>
          {/if}
        </p>
        {#if upNextRows.length > 0}
          <ol class="np-up-next-list">
            {#each upNextRows as row (row.key)}
              <li class="np-up-next-row" data-testid="np-up-next-row" data-suggestion={row.suggestion ? "true" : "false"}>
                {#if row.entry && onSelectEntry}
                  {@const entry = row.entry}
                  {@const busy = starting?.entryId === entry.entryId}
                  <button
                    type="button"
                    class="np-up-next-start"
                    class:is-unavailable={!isPlayable(entry.track)}
                    aria-disabled={canStart(entry.track) ? undefined : "true"}
                    aria-busy={busy}
                    title={startBlocked(entry.track) ?? undefined}
                    onclick={() => void startEntry(entry)}
                    data-testid="np-up-next-start"
                  >
                    {@render rowBody(row)}
                    {@render waitSlot(busy)}
                  </button>
                {:else}
                  {@render rowBody(row)}
                {/if}
              </li>
            {/each}
          </ol>
        {:else if queueIdx >= 0}
          <p class="np-up-next-note" data-testid="np-up-next-empty">{t.nowPlaying.upNextEmpty}</p>
        {/if}
        {#if upNextSource}
          <p class="np-up-next-note" data-testid="np-up-next-source">{upNextSource}</p>
        {/if}
      </aside>
    </div>
  {/if}
</section>

<!-- A row's cover and two lines; an unavailable queued track shows why in place of the artist. -->
{#snippet rowBody(row: UpNextRow)}
  {@const badge = row.entry ? availabilityBadge(row.track) : null}
  <Cover cover={row.track.cover} seed={row.track.videoId} label={row.track.title} {mockMode} class="np-up-next-art" />
  <span class="min-w-0 flex-1">
    <span class="block truncate np-up-next-title" lang={detectCjkLangHint(row.track.title)} title={row.track.title}>{row.track.title}</span>
    <span class="block truncate np-up-next-artist" class:is-warning={!!badge}>{rowSubline(row)}</span>
  </span>
{/snippet}

<!-- The waiting dot's place is kept on every startable row, so the text never moves when it shows. -->
{#snippet waitSlot(busy: boolean)}
  <span class="np-up-next-wait" aria-hidden="true">
    {#if busy}<span class="wait-dot" data-testid="np-up-next-pending"></span>{/if}
  </span>
{/snippet}
