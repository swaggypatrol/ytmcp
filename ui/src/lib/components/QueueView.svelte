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
  import { t } from "../i18n/index.svelte.js";
  import { isTerminalCommandStatus } from "../seek-preview.js";
  import Icon from "./Icon.svelte";
  import VirtualizedTrackList from "./VirtualizedTrackList.svelte";

  /** A queue command still waiting for its outcome when no receipt or queue change settles it. */
  const QUEUE_EDIT_SETTLE_TIMEOUT_MS = 15_000;

  interface Props {
    queueSummary: QueueSummary | null;
    capabilities: CapabilitySet | null;
    connection?: PlayerConnectionState | null;
    player?: PlayerObservedState | null;
    lastReceipt?: CommandReceipt | null;
    mockMode: boolean;
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
    onSelectEntry,
    onTrackActions,
    onCommand,
  }: Props = $props();

  /** The one in-flight edit (edits are serialized: each one is bound to a queue revision). */
  interface PendingEdit {
    readonly key: string;
    readonly revision: number;
    readonly autoplay: boolean | null | undefined;
    readonly commandId: string | null;
  }
  let pending = $state.raw<PendingEdit | null>(null);

  const entries = $derived(queueSummary?.previewEntries ?? []);
  const tracks = $derived(entries.map((e) => e.track));
  const entryIds = $derived(entries.map((e) => e.entryId));
  const currentEntryId = $derived(queueSummary?.currentEntryId ?? null);
  const currentIdx = $derived(currentEntryId ? entries.findIndex((e) => e.entryId === currentEntryId) : -1);
  const selectGate = $derived(commandGate(capabilities?.queueMirror, connection));

  const removeSupported = $derived(capabilities?.queueRemove === "supported");
  const reorderSupported = $derived(capabilities?.queueReorder === "supported");
  const editSupported = $derived(removeSupported || reorderSupported);

  /**
   * Core accepts an edit only on a complete, finite queue at its exact revision, anchored on the
   * current occurrence of the current track instance. The preview must also hold every entry,
   * or "last row" and move targets would not be the real ones.
   */
  const editGate = $derived.by((): Gate => {
    const connected = commandGate(editSupported ? "supported" : undefined, connection);
    if (!connected.enabled) return editSupported ? connected : { enabled: false, reason: t.queue.editUnsupported };
    if (
      queueSummary?.completeness !== "complete" ||
      queueSummary.hasMore !== false ||
      queueSummary.loadedCount !== entries.length
    ) {
      return { enabled: false, reason: t.queue.editNeedsCompleteQueue };
    }
    if (currentIdx < 0 || player?.currentQueueEntryId !== currentEntryId || !player?.trackInstanceId) {
      return { enabled: false, reason: t.queue.editNeedsCurrent };
    }
    if (pending) return { enabled: false, reason: t.queue.editPending };
    return { enabled: true, reason: null };
  });

  const hasUpcoming = $derived(currentIdx >= 0 && currentIdx < entries.length - 1);
  // Clearing follows its own page capability: the page refuses it on a radio queue or while
  // autoplay suggestions play, even when single edits work.
  const clearGate = $derived.by((): Gate => {
    if (!editGate.enabled) return editGate;
    if (capabilities?.queueClear !== "supported") return { enabled: false, reason: t.queue.clearUnavailable };
    return editGate;
  });

  const autoplaySupport = $derived(capabilities?.queueAutoplay ?? undefined);
  const autoplayGate = $derived.by((): Gate => {
    const gate = commandGate(autoplaySupport, connection);
    if (!gate.enabled) return autoplaySupport ? gate : { enabled: false, reason: t.queue.autoplayUnsupported };
    if (player?.autoplay == null) return { enabled: false, reason: t.queue.autoplayUnknown };
    if (pending) return { enabled: false, reason: t.queue.editPending };
    return gate;
  });

  // Settle the pending edit on its terminal receipt, on a queue revision change (every edit
  // bumps it once observed), or, for autoplay, once the observed value changes.
  $effect(() => {
    const p = pending;
    if (!p) return;
    const receiptDone = p.commandId !== null && lastReceipt?.commandId === p.commandId && isTerminalCommandStatus(lastReceipt.status);
    const observed = p.key === "autoplay" ? player?.autoplay !== p.autoplay : queueSummary?.queueRevision !== p.revision;
    if (receiptDone || observed) pending = null;
  });

  $effect(() => {
    if (!pending) return;
    const timer = setTimeout(() => (pending = null), QUEUE_EDIT_SETTLE_TIMEOUT_MS);
    return () => clearTimeout(timer);
  });

  async function send(key: string, op: PlayerOp, args: PlayerCommandArgs) {
    if (!onCommand || !queueSummary) return;
    const started: PendingEdit = { key, revision: queueSummary.queueRevision, autoplay: player?.autoplay, commandId: null };
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

  function toggleAutoplay() {
    if (player?.autoplay == null || !queueSummary) return;
    void send("autoplay", "setAutoplay", { autoplay: !player.autoplay, queueRevision: queueSummary.queueRevision });
  }

  const pendingKey = $derived(pending?.key ?? null);
</script>

<section class="flex flex-col gap-4" aria-labelledby="queue-heading" data-testid="view-queue">
  <div class="flex flex-wrap items-center justify-between gap-3">
    <h1 id="queue-heading" class="type-xl font-bold tracking-tight">{t.queue.title}</h1>
    {#if queueSummary}
      <div class="flex flex-wrap items-center gap-2">
        {#if currentIdx >= 0}
          <span class="chip chip-accent tabular-nums">
            {t.queue.positionOf.replace("{current}", String(currentIdx + 1)).replace("{total}", String(entries.length))}
          </span>
        {/if}
        <span class="chip tabular-nums">{t.queue.revisionLabel} #{queueSummary.queueRevision}</span>
        <span class="chip">{t.queue.completeness[queueSummary.completeness]}</span>
        {#if queueSummary.hasMore}
          <span class="chip">{t.queue.hasMore}</span>
        {/if}
        <button
          type="button"
          role="switch"
          class="chip"
          class:chip-accent={player?.autoplay === true}
          aria-checked={player?.autoplay === true}
          aria-busy={pendingKey === "autoplay"}
          disabled={!autoplayGate.enabled}
          title={autoplayGate.reason ?? t.queue.autoplay}
          onclick={toggleAutoplay}
          data-testid="queue-autoplay-toggle"
        >
          {t.queue.autoplay}{player?.autoplay == null ? ` · ${t.common.unknown}` : ""}
        </button>
        {#if removeSupported}
          <button
            type="button"
            class="btn-ghost"
            disabled={!clearGate.enabled || !hasUpcoming}
            aria-busy={pendingKey === "clear"}
            title={clearGate.reason ?? t.queue.clearUpcoming}
            onclick={clearUpcoming}
            data-testid="queue-clear-upcoming"
          >
            {t.queue.clearUpcoming}
          </button>
        {/if}
      </div>
    {/if}
  </div>

  <p class="type-xs m-0" style="color: var(--text-muted);" data-testid="queue-note">
    {t.queue.duplicateNote}
    {#if editGate.reason} {editGate.reason}{/if}
    {#if selectGate.reason && selectGate.reason !== editGate.reason} {selectGate.reason}{/if}
  </p>

  {#if entries.length === 0}
    <div class="state-block" data-testid="queue-empty">
      <Icon name="queue" class="w-9 h-9" style="color: var(--text-muted);" />
      <span class="type-sm" style="color: var(--text-secondary);">{t.queue.empty}</span>
    </div>
  {:else}
    <VirtualizedTrackList
      {tracks}
      {entryIds}
      {currentEntryId}
      {mockMode}
      showHeader
      ariaLabel={t.queue.title}
      onPlayTrack={(_track, idx) => {
        const entry = entries[idx];
        if (entry && selectGate.enabled) onSelectEntry(entry);
      }}
      onTrackActions={(track) => onTrackActions(track)}
      rowActions={editSupported ? queueRowActions : undefined}
    />
  {/if}
</section>

<!-- Only upcoming occurrences get edit controls; the current one and history never do. -->
{#snippet queueRowActions(index: number, tabindex: number)}
  {@const entry = entries[index]}
  {#if entry && currentIdx >= 0 && index > currentIdx}
    {@const busy = pendingKey !== null && pendingKey.endsWith(`:${entry.entryId}`)}
    <div class="flex items-center shrink-0" data-testid="queue-row-actions" aria-busy={busy}>
      {#if reorderSupported}
        <button
          type="button"
          class="btn-icon"
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
          class="btn-icon"
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
          class="btn-icon"
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
        <span class="wait-dot" style="color: var(--text-secondary);" role="img" aria-label={t.player.pending} data-testid="queue-row-pending"></span>
      {/if}
    </div>
  {/if}
{/snippet}
