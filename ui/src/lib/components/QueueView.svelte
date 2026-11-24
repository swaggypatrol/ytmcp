<script lang="ts">
  import type {
    CapabilitySet,
    CommandReceipt,
    PlayerCommandArgs,
    PlayerConnectionState,
    PlayerObservedState,
    PlayerOp,
    QueueEntry,
    QueueSummary,
    Track,
  } from "@ytmcp/protocol";
  import { commandGate, type Gate } from "../capabilities.js";
  import { tick } from "svelte";
  import { t } from "../i18n/index.svelte.js";
  import { isTerminalCommandStatus } from "../seek-preview.js";
  import Cover from "./Cover.svelte";
  import { detectCjkLangHint, formatArtists, formatDurationMs } from "./format.js";
  import Icon from "./Icon.svelte";
  import PlaybackIndicator, { indicatorState } from "./PlaybackIndicator.svelte";
  import VirtualizedTrackList, { type TrackRowLike } from "./VirtualizedTrackList.svelte";

  /** A queue command still waiting for its outcome when no receipt or queue change settles it. */
  const QUEUE_EDIT_SETTLE_TIMEOUT_MS = 15_000;

  interface Props {
    queueSummary: QueueSummary | null;
    capabilities: CapabilitySet | null;
    connection?: PlayerConnectionState | null;
    player?: PlayerObservedState | null;
    lastReceipt?: CommandReceipt | null;
    mockMode: boolean;
    /** The app's decorative motion policy, for the current row's playback indicator. */
    motion?: boolean;
    /** Whether Played is expanded; bound by the app so it survives leaving the page and coming back. */
    playedOpen?: boolean;
    /** Like toggles on the queue's own rows (not on YouTube Music's suggestions). */
    like?: TrackRowLike;
    onSelectEntry: (entry: QueueEntry) => void;
    onTrackActions: (track: Track) => void;
    /** Sends a queue edit or autoplay command; failures surface through the shared banners. */
    onCommand?: (op: PlayerOp, args: PlayerCommandArgs) => Promise<CommandReceipt | null>;
  }

  let {
    queueSummary,
    capabilities,
    connection = null,
    player = null,
    lastReceipt = null,
    mockMode,
    motion = false,
    playedOpen = $bindable(false),
    like,
    onSelectEntry,
    onTrackActions,
    onCommand,
  }: Props = $props();

  /** The one in-flight edit (edits are serialized: each one is bound to a queue revision). */
  interface PendingEdit {
    readonly key: string;
    readonly revision: number;
    readonly autoplay: boolean | null | undefined;
    /** The Up Next filter being selected, for the `filter` key. */
    readonly filterId?: string;
    readonly commandId: string | null;
  }
  let pending = $state.raw<PendingEdit | null>(null);

  const entries = $derived(queueSummary?.previewEntries ?? []);
  const tracks = $derived(entries.map((e) => e.track));
  const entryIds = $derived(entries.map((e) => e.entryId));
  const currentEntryId = $derived(queueSummary?.currentEntryId ?? null);
  const currentIdx = $derived(currentEntryId ? entries.findIndex((e) => e.entryId === currentEntryId) : -1);
  const selectGate = $derived(commandGate(capabilities?.queueMirror, connection));

  /** Absolute queue position (1-based) of the first preview entry. */
  const firstPosition = $derived((queueSummary?.loadedStart ?? 0) + (queueSummary?.previewStart ?? 0) + 1);
  const earlierHidden = $derived(firstPosition > 1);
  const anchored = $derived(currentIdx >= 0);
  const played = $derived(anchored ? entries.slice(0, currentIdx) : []);
  const upNext = $derived(anchored ? entries.slice(currentIdx + 1) : []);
  const currentPosition = $derived(firstPosition + currentIdx);
  /** "N of M" only when the loaded window is the whole, finite queue. */
  const totalKnown = $derived(
    queueSummary?.completeness === "complete" && queueSummary.hasMore === false && (queueSummary.loadedStart ?? 0) === 0
  );
  /** The current occurrence is the one the player instance is on, so its row shows the indicator. */
  const playerOnCurrent = $derived(
    anchored && player?.currentQueueEntryId === currentEntryId && !!player?.trackInstanceId
  );
  const showIndicator = $derived(playerOnCurrent && indicatorState(player?.playback, connection) !== null);

  /** Loaded entries after the preview's last row; YouTube Music's suggestions only play after them. */
  const laterHidden = $derived(
    Math.max(0, (queueSummary?.loadedCount ?? 0) - (queueSummary?.previewStart ?? 0) - entries.length)
  );
  const official = $derived(queueSummary?.officialUpcoming ?? null);
  const officialTracks = $derived(official?.status === "observed" ? (official.tracks ?? []) : []);
  // Suggestions (and their loading state) are listed only right after the queue's last loaded
  // entry; whether autoplay is off or a radio keeps going holds either way.
  const showOfficial = $derived.by(() => {
    if (official === null || official.status === "none") return false;
    if (official.status === "observed" || official.status === "loading") {
      return laterHidden === 0 && !(official.status === "observed" && officialTracks.length === 0);
    }
    return true;
  });
  const filters = $derived(queueSummary?.filters ?? []);
  const selectedFilter = $derived(filters.find((f) => f.selected) ?? null);
  // Only a proven capability turns the label into buttons; otherwise the selection stays read-only.
  const filterSupported = $derived(capabilities?.queueFilter === "supported");
  const filtersShown = $derived(official?.status === "observed" || official?.status === "loading");
  // A filter switch leaves the explicit queue alone, so its wait is not called a queue change.
  const pendingReason = $derived(pending?.key === "filter" ? t.queue.filterPending : t.queue.editPending);
  const filterGate = $derived.by((): Gate => {
    const gate = commandGate(capabilities?.queueFilter ?? undefined, connection);
    if (gate.enabled && pending) return { enabled: false, reason: pendingReason };
    return gate;
  });

  const removeSupported = $derived(capabilities?.queueRemove === "supported");
  const reorderSupported = $derived(capabilities?.queueReorder === "supported");
  const editSupported = $derived(removeSupported || reorderSupported);
  const editActionCount = $derived((reorderSupported ? 2 : 0) + (removeSupported ? 1 : 0));

  /**
   * Core accepts an edit only on a complete, finite queue, and the preview must hold every entry
   * or "last row" and move targets would not be the real ones. On any other queue the move and
   * remove buttons are not drawn at all (the capability stays in Diagnostics).
   */
  const editable = $derived(
    editSupported &&
      queueSummary?.completeness === "complete" &&
      queueSummary.hasMore === false &&
      queueSummary.loadedCount === entries.length
  );

  /**
   * An editable queue is also edited only at its exact revision, anchored on the current
   * occurrence of the current track instance, one edit at a time.
   */
  const editGate = $derived.by((): Gate => {
    if (!editable) return { enabled: false, reason: null };
    const connected = commandGate("supported", connection);
    if (!connected.enabled) return connected;
    if (currentIdx < 0 || player?.currentQueueEntryId !== currentEntryId || !player?.trackInstanceId) {
      return { enabled: false, reason: t.queue.editNeedsCurrent };
    }
    if (pending) return { enabled: false, reason: pendingReason };
    return { enabled: true, reason: null };
  });

  const hasUpcoming = $derived(currentIdx >= 0 && currentIdx < entries.length - 1);
  const clearSupport = $derived(capabilities?.queueClear ?? undefined);
  // Drawn only while the page can clear; a page that refuses clearing gets no Clear button.
  const showClear = $derived(clearSupport !== undefined && clearSupport !== "unsupported");

  /**
   * Mirrors what core needs for a clear: a usable connection, the queue's revision and the
   * current occurrence of the current track instance, in the loaded entries. Unlike single
   * edits it works on a partial queue with more to load: core removes the loaded upcoming
   * entries after the current one (at most one batch) and keeps autoplay suggestions.
   */
  const clearGate = $derived.by((): Gate => {
    const connected = commandGate(clearSupport, connection);
    if (!connected.enabled) return connected;
    if (!queueSummary || !currentEntryId || currentIdx < 0 || player?.currentQueueEntryId !== currentEntryId || !player?.trackInstanceId) {
      return { enabled: false, reason: t.queue.editNeedsCurrent };
    }
    if (pending) return { enabled: false, reason: pendingReason };
    if (!hasUpcoming) return { enabled: false, reason: t.queue.clearNothingUpcoming };
    return { enabled: true, reason: connected.reason };
  });

  const autoplaySupport = $derived(capabilities?.queueAutoplay ?? undefined);
  // The switch is drawn only while the page can switch autoplay and has said whether it is on.
  const showAutoplay = $derived(
    autoplaySupport !== undefined && autoplaySupport !== "unsupported" && player?.autoplay != null
  );
  const autoplayGate = $derived.by((): Gate => {
    const gate = commandGate(autoplaySupport, connection);
    if (gate.enabled && pending) return { enabled: false, reason: pendingReason };
    return gate;
  });

  // Settle the pending edit on its terminal receipt, on a queue revision change (every edit
  // bumps it once observed), for autoplay once the observed value changes, and for a filter
  // once the summary shows it selected (the page keeps the explicit queue and its revision).
  $effect(() => {
    const p = pending;
    if (!p) return;
    const receiptDone = p.commandId !== null && lastReceipt?.commandId === p.commandId && isTerminalCommandStatus(lastReceipt.status);
    const observed =
      p.key === "autoplay"
        ? player?.autoplay !== p.autoplay
        : p.key === "filter"
          ? filters.some((f) => f.filterId === p.filterId && f.selected)
          : queueSummary?.queueRevision !== p.revision;
    if (receiptDone || observed) pending = null;
  });

  $effect(() => {
    if (!pending) return;
    const timer = setTimeout(() => (pending = null), QUEUE_EDIT_SETTLE_TIMEOUT_MS);
    return () => clearTimeout(timer);
  });

  async function send(key: string, op: PlayerOp, args: PlayerCommandArgs, filterId?: string) {
    if (!onCommand || !queueSummary) return;
    const started: PendingEdit = { key, revision: queueSummary.queueRevision, autoplay: player?.autoplay, filterId, commandId: null };
    pending = started;
    const receipt = await onCommand(op, args);
    if (pending !== started) return;
    pending = !receipt || isTerminalCommandStatus(receipt.status) ? null : { ...started, commandId: receipt.commandId };
  }

  function remove(index: number) {
    const entry = entries[index];
    if (!entry || !queueSummary) return;
    void send(`remove:${entry.entryId}`, "removeQueueEntry", {
      queueEntryId: entry.entryId,
      queueRevision: queueSummary.queueRevision,
    });
  }

  /**
   * Core removes the entry first and then inserts it before `beforeQueueEntryId` (or at the
   * end when absent), so moving down skips one entry and moving up anchors on the previous one.
   */
  function move(index: number, direction: "up" | "down") {
    const entry = entries[index];
    if (!entry || !queueSummary) return;
    const before = direction === "up" ? entries[index - 1]?.entryId : entries[index + 2]?.entryId;
    void send(`move-${direction}:${entry.entryId}`, "moveQueueEntry", {
      queueEntryId: entry.entryId,
      queueRevision: queueSummary.queueRevision,
      ...(before ? { beforeQueueEntryId: before } : {}),
    });
  }

  function clearUpcoming() {
    if (!queueSummary || !currentEntryId || !player?.trackInstanceId) return;
    void send("clear", "clearUpcoming", {
      queueEntryId: currentEntryId,
      queueRevision: queueSummary.queueRevision,
      trackInstanceId: player.trackInstanceId,
    });
  }

  function selectFilter(filterId: string) {
    if (!filterGate.enabled || filters.find((f) => f.filterId === filterId)?.selected !== false) return;
    void send("filter", "setQueueFilter", { queueFilterId: filterId }, filterId);
  }

  function toggleAutoplay() {
    if (player?.autoplay == null || !queueSummary) return;
    void send("autoplay", "setAutoplay", { autoplay: !player.autoplay, queueRevision: queueSummary.queueRevision });
  }

  let rootEl = $state<HTMLElement | null>(null);
  /** The occurrence whose row had focus when the current entry changed, until it is refocused. */
  let refocusEntryId: string | null = null;

  // A change of current occurrence moves rows between Played, Now playing and Up next (separate
  // lists), so note the focused row before the update and follow it into its new list after.
  $effect.pre(() => {
    void currentEntryId;
    const row = document.activeElement?.closest<HTMLElement>("[data-row-key]");
    refocusEntryId = row && rootEl?.contains(row) ? (row.dataset.rowKey ?? null) : null;
  });
  $effect(() => {
    void currentEntryId;
    const entryId = refocusEntryId;
    refocusEntryId = null;
    if (entryId) void tick().then(() => refocusRow(entryId));
  });

  /** Focuses the row of `entryId`, or the Now playing row when it is not shown (collapsed Played). */
  function refocusRow(entryId: string) {
    const active = document.activeElement;
    if (!rootEl || (active && active !== document.body && active.isConnected)) return;
    const rows = Array.from(rootEl.querySelectorAll<HTMLElement>("[data-row-key]"));
    const row = rows.find((r) => r.dataset.rowKey === entryId) ?? rootEl.querySelector<HTMLElement>('[data-testid="queue-now"] [data-row-key]');
    row?.querySelector<HTMLElement>("[data-row-index]")?.focus();
  }

  const pendingKey = $derived(pending?.key ?? null);
  const pendingFilterId = $derived(pending?.key === "filter" ? (pending.filterId ?? null) : null);
</script>

<section class="queue-page" aria-labelledby="queue-heading" data-testid="view-queue" bind:this={rootEl}>
  <div class="queue-head">
    <div class="flex flex-wrap items-end justify-between gap-3">
      <h1 id="queue-heading" class="collection-title">{t.queue.title}</h1>
      {#if queueSummary}
        <div class="flex flex-wrap items-center gap-2">
          {#if anchored && totalKnown}
            <span class="type-sm tabular-nums" style="color: var(--text-secondary);" data-testid="queue-position">
              {t.queue.positionOf.replace("{current}", String(currentPosition)).replace("{total}", String(queueSummary.loadedCount))}
            </span>
          {/if}
          {#if showAutoplay}
            <button
              type="button"
              role="switch"
              class="chip queue-chip"
              aria-checked={player?.autoplay === true}
              aria-busy={pendingKey === "autoplay"}
              disabled={!autoplayGate.enabled}
              title={autoplayGate.reason ?? t.queue.autoplay}
              onclick={toggleAutoplay}
              data-testid="queue-autoplay-toggle"
            >
              {t.queue.autoplay}
            </button>
          {/if}
          {#if showClear}
            <button
              type="button"
              class="btn-ghost queue-chip"
              disabled={!clearGate.enabled}
              aria-busy={pendingKey === "clear"}
              title={clearGate.reason ?? t.queue.clearHint}
              onclick={clearUpcoming}
              data-testid="queue-clear-upcoming"
            >
              {t.queue.clearUpcoming}
            </button>
          {/if}
        </div>
      {/if}
    </div>
  </div>

  {#if entries.length === 0}
    <div class="state-block" data-testid="queue-empty">
      <Icon name="queue" class="w-9 h-9" style="color: var(--text-muted);" />
      <span class="type-sm" style="color: var(--text-secondary);">{t.queue.empty}</span>
    </div>
  {:else if !anchored}
    <div class="queue-section">
      <VirtualizedTrackList
        {tracks}
        {entryIds}
        {currentEntryId}
        {mockMode}
        {like}
        firstNumber={firstPosition}
        showHeader
        ariaLabel={t.queue.title}
        onPlayTrack={(_track, idx) => {
          const entry = entries[idx];
          if (entry && selectGate.enabled) onSelectEntry(entry);
        }}
        onTrackActions={(track) => onTrackActions(track)}
        playDisabled={selectGate.enabled ? null : selectGate.reason}
        rowActionCount={editable ? editActionCount : 0}
      />
      {@render laterNote()}
    </div>
  {:else}
    {#if played.length > 0 || earlierHidden}
      <section class="queue-section" aria-labelledby="queue-played-heading" data-testid="queue-played">
        <h2 id="queue-played-heading" class="list-section-title">
          {#if played.length > 0}
            <button
              type="button"
              class="flex items-center gap-1.5"
              aria-expanded={playedOpen}
              onclick={() => (playedOpen = !playedOpen)}
              data-testid="queue-played-toggle"
            >
              <Icon name={playedOpen ? "chevronDown" : "chevronRight"} />
              <span>{t.queue.played}</span>
              <span class="chip tabular-nums" data-testid="queue-played-count">{played.length}</span>
            </button>
          {:else}
            {t.queue.played}
          {/if}
        </h2>
        {#if earlierHidden}
          <p class="type-xs m-0" style="color: var(--text-muted);" data-testid="queue-earlier-hidden">{t.queue.earlierHidden}</p>
        {/if}
        {#if playedOpen && played.length > 0}
          <VirtualizedTrackList
            tracks={played.map((e) => e.track)}
            entryIds={played.map((e) => e.entryId)}
            {currentEntryId}
            {mockMode}
            {like}
            firstNumber={firstPosition}
            returnFrame={false}
            ariaLabel={t.queue.played}
            onPlayTrack={(_track, idx) => {
              const entry = played[idx];
              if (entry && selectGate.enabled) onSelectEntry(entry);
            }}
            onTrackActions={(track) => onTrackActions(track)}
            playDisabled={selectGate.enabled ? null : selectGate.reason}
            rowActionCount={editable ? editActionCount : 0}
          />
        {/if}
      </section>
    {/if}

    <section class="queue-section" aria-labelledby="queue-now-heading" data-testid="queue-now">
      <h2 id="queue-now-heading" class="list-section-title">{t.queue.nowPlaying}</h2>
      <VirtualizedTrackList
        tracks={[entries[currentIdx]!.track]}
        entryIds={[entries[currentIdx]!.entryId]}
        {currentEntryId}
        {mockMode}
        {like}
        firstNumber={currentPosition}
        returnFrame={upNext.length === 0}
        ariaLabel={t.queue.nowPlaying}
        onPlayTrack={() => {
          const entry = entries[currentIdx];
          if (entry && selectGate.enabled) onSelectEntry(entry);
        }}
        onTrackActions={(track) => onTrackActions(track)}
        playDisabled={selectGate.enabled ? null : selectGate.reason}
        rowActionCount={editable ? editActionCount : 0}
        currentMarker={currentRowMarker}
      />
    </section>

    <section class="queue-section" aria-labelledby="queue-up-next-heading" data-testid="queue-up-next">
      <h2 id="queue-up-next-heading" class="list-section-title">{t.queue.upNext}</h2>
      {#if upNext.length > 0}
        <VirtualizedTrackList
          tracks={upNext.map((e) => e.track)}
          entryIds={upNext.map((e) => e.entryId)}
          {currentEntryId}
          {mockMode}
          {like}
          firstNumber={currentPosition + 1}
          ariaLabel={t.queue.upNext}
          onPlayTrack={(_track, idx) => {
            const entry = upNext[idx];
            if (entry && selectGate.enabled) onSelectEntry(entry);
          }}
          onTrackActions={(track) => onTrackActions(track)}
          playDisabled={selectGate.enabled ? null : selectGate.reason}
          rowActionCount={editable ? editActionCount : 0}
          rowActions={editable ? upNextRowActions : undefined}
        />
      {:else if laterHidden === 0}
        <p class="type-xs m-0" style="color: var(--text-muted);" data-testid="queue-up-next-empty">{t.queue.upNextEmpty}</p>
      {/if}
      {@render laterNote()}
    </section>
  {/if}

  {#if showOfficial && official}
    <section class="queue-section" aria-labelledby="queue-official-heading" data-testid="queue-official">
      <div class="flex flex-wrap items-center gap-2">
        <h2 id="queue-official-heading" class="list-section-title">{t.queue.officialTitle}</h2>
        {#if filtersShown}
          {#if filterSupported && filters.length > 0}
            <div class="flex flex-wrap items-center gap-1.5" role="group" aria-label={t.queue.filterGroupLabel} data-testid="queue-filters">
              {#each filters as filter (filter.filterId)}
                <button
                  type="button"
                  class="chip queue-chip"
                  aria-pressed={filter.selected}
                  aria-busy={pendingFilterId === filter.filterId}
                  aria-disabled={!filterGate.enabled}
                  title={filterGate.reason ?? t.queue.filterHint}
                  onclick={() => selectFilter(filter.filterId)}
                  data-testid="queue-filter"
                >
                  {#if pendingFilterId === filter.filterId}<span class="wait-dot" aria-hidden="true"></span>{/if}{filter.label}
                </button>
              {/each}
            </div>
          {:else if selectedFilter}
            <span class="chip" title={t.queue.officialFilterHint} data-testid="queue-official-filter">{selectedFilter.label}</span>
          {/if}
        {/if}
      </div>
      {#if official.status === "observed"}
        <ul
          class="queue-official-list track-list has-more-actions"
          class:has-like={!!like}
          class:has-row-actions={editable}
          style:--tl-action-count={editActionCount}
          aria-label={t.queue.officialListLabel}
          data-testid="queue-official-list"
        >
          {#each officialTracks as track, i (`${track.videoId}::${i}`)}
            <li class="tl-row" data-testid="queue-official-row">
              <span class="tl-main">
                <span class="tl-num" aria-hidden="true"></span>
                <span class="tl-title">
                  <Cover cover={track.cover} seed={track.videoId} label={track.title} {mockMode} class="tl-cover" />
                  <span class="tl-text">
                    <span class="tl-title-text" lang={detectCjkLangHint(track.title)} title={track.title}>{track.title}</span>
                    <span class="tl-sub">{formatArtists(track)}</span>
                  </span>
                </span>
                <span class="tl-album" lang={detectCjkLangHint(track.album?.title ?? "")}>{track.album?.title ?? ""}</span>
              </span>
              {#if editable}<span class="tl-actions" aria-hidden="true"></span>{/if}
              {#if like}<span class="tl-like" aria-hidden="true"></span>{/if}
              <span class="tl-dur">{formatDurationMs(track.durationMs)}</span>
              <span class="tl-more">
                <button
                  type="button"
                  class="btn-icon btn-icon-sm"
                  aria-label={t.track.actionsFor.replace("{title}", track.title)}
                  title={t.track.actions}
                  onclick={() => onTrackActions(track)}
                  data-testid="track-actions-btn"
                >
                  <Icon name="more" />
                </button>
              </span>
            </li>
          {/each}
        </ul>
      {:else}
        <p class="type-xs m-0" style="color: var(--text-secondary);" data-testid="queue-official-status">
          {official.status === "loading"
            ? t.queue.officialLoading
            : official.status === "off"
              ? t.queue.officialOff
              : t.queue.officialRadio}
        </p>
      {/if}
    </section>
  {/if}
</section>

<!-- Loaded entries past the preview, under the list they follow. -->
{#snippet laterNote()}
  {#if laterHidden > 0}
    <p class="type-xs m-0" style="color: var(--text-muted);" data-testid="queue-later-hidden">
      {t.queue.laterHidden.replace("{count}", String(laterHidden))}
    </p>
  {/if}
{/snippet}

{#snippet currentRowMarker(position: number)}
  {#if showIndicator}
    <PlaybackIndicator playback={player?.playback} {connection} {motion} />
  {:else}
    {position}
  {/if}
{/snippet}

<!-- Up next rows are numbered from the list's start; edits address the full preview. -->
{#snippet upNextRowActions(local: number, tabindex: number)}
  {@render queueRowActions(local + currentIdx + 1, tabindex)}
{/snippet}

<!-- Only upcoming occurrences get edit controls; the current one and history never do. -->
{#snippet queueRowActions(index: number, tabindex: number)}
  {@const entry = entries[index]}
  {#if entry && currentIdx >= 0 && index > currentIdx}
    {@const busy = pendingKey !== null && pendingKey.endsWith(`:${entry.entryId}`)}
    <div class="relative flex items-center shrink-0" data-testid="queue-row-actions" aria-busy={busy}>
      {#if reorderSupported}
        <button
          type="button"
          class="btn-icon btn-icon-sm"
          {tabindex}
          disabled={!editGate.enabled || index <= currentIdx + 1}
          aria-label={t.queue.moveUpFor.replace("{title}", entry.track.title)}
          title={editGate.reason ?? t.queue.moveUp}
          onclick={() => move(index, "up")}
          data-testid="queue-move-up"
        >
          <Icon name="chevronUp" />
        </button>
        <button
          type="button"
          class="btn-icon btn-icon-sm"
          {tabindex}
          disabled={!editGate.enabled || index >= entries.length - 1}
          aria-label={t.queue.moveDownFor.replace("{title}", entry.track.title)}
          title={editGate.reason ?? t.queue.moveDown}
          onclick={() => move(index, "down")}
          data-testid="queue-move-down"
        >
          <Icon name="chevronDown" />
        </button>
      {/if}
      {#if removeSupported}
        <button
          type="button"
          class="btn-icon btn-icon-sm"
          {tabindex}
          disabled={!editGate.enabled}
          aria-label={t.queue.removeFor.replace("{title}", entry.track.title)}
          title={editGate.reason ?? t.queue.remove}
          onclick={() => remove(index)}
          data-testid="queue-remove"
        >
          <Icon name="close" />
        </button>
      {/if}
      {#if busy}
        <!-- Beside the buttons, not after them, so the row keeps the header's column widths. -->
        <span class="wait-dot absolute right-full top-1/2 -translate-y-1/2" style="color: var(--text-secondary);" role="img" aria-label={t.player.pending} data-testid="queue-row-pending"></span>
      {/if}
    </div>
  {/if}
{/snippet}
