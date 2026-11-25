<script lang="ts">
  import { untrack } from "svelte";
  import type { Toast, ToastStore } from "../feedback/toasts.svelte.js";
  import { glass } from "../glass/liquid-glass.js";
  import { afterExit, overlayExitDuration, type OverlayExit } from "../glass/overlay-motion.svelte.js";

  interface Props {
    store: ToastStore;
    /** The current account or connection problem, else the page's failed refresh; shown whenever no transient toast is. */
    sticky: Toast | null;
  }

  let { store, sticky }: Props = $props();

  const shown = $derived(store.current ?? sticky);

  interface Entry {
    readonly key: number;
    readonly toast: Toast;
    readonly leaving: OverlayExit | null;
    readonly sticky: boolean;
    /** Live role: alert for a failure, status otherwise, none for a sticky problem coming back. */
    readonly live: "alert" | "status" | null;
  }
  let entries = $state.raw<Entry[]>([]);
  let entrySeq = 0;
  /** The sticky problem already announced; it is not announced again when it comes back. */
  let announcedSticky: string | null = null;

  $effect(() => {
    const id = sticky?.id ?? null;
    untrack(() => {
      if (id !== announcedSticky) announcedSticky = null;
    });
  });

  /** A transient failure interrupts; anything else is polite, and a returning problem is silent. */
  function liveRole(toast: Toast): Entry["live"] {
    if (toast.id !== sticky?.id) return toast.tone === "danger" ? "alert" : "status";
    if (announcedSticky === toast.id) return null;
    announcedSticky = toast.id;
    return "status";
  }
  /** Cancels the pending removal of each leaving toast. */
  const exitWaits = new Set<() => void>();
  const entryElements = new Map<number, HTMLElement>();

  function trackEntry(el: HTMLElement, key: number) {
    entryElements.set(key, el);
    return () => { entryElements.delete(key); };
  }

  // One toast at a time: the shown one enters while the previous one leaves in the same spot.
  $effect(() => {
    const next = shown;
    untrack(() => {
      const top = entries.at(-1);
      const current = top && !top.leaving ? top : null;
      if (next && current && current.toast.id === next.id) {
        if (current.toast !== next) entries = [...entries.slice(0, -1), { ...current, toast: next }];
        return;
      }
      if (!next && !current) return;
      // The pointer or focus was on the old toast; the new one counts down until held again.
      store.release();
      const exit: OverlayExit = current && (store.expiredId === current.toast.id || (!next && current.sticky))
        ? "dissolve" : "dismiss";
      entries = [
        ...entries.map((entry) => (entry.leaving ? entry : { ...entry, leaving: exit })),
        ...(next ? [{ key: ++entrySeq, toast: next, leaving: null, sticky: next === sticky, live: liveRole(next) }] : []),
      ];
      if (!current) return;
      // The keyed element is still mounted. Read the exit after Svelte marks it leaving, and
      // remove it once that exit has run.
      queueMicrotask(() => {
        const el = entryElements.get(current.key);
        if (!el) return;
        const cancel = afterExit(el, overlayExitDuration(el, exit === "dismiss" ? 160 : 360), () => {
          exitWaits.delete(cancel);
          entries = entries.filter((entry) => entry.key !== current.key);
        });
        exitWaits.add(cancel);
      });
    });
  });

  $effect(() => () => {
    for (const cancel of exitWaits) cancel();
    exitWaits.clear();
    store.release();
  });

  function runAction(toast: Toast) {
    toast.action?.run(toast);
    // A transient toast has done its job; a sticky one stays until its problem is resolved.
    store.dismiss(toast.id);
  }

  function onFocusOut(e: FocusEvent) {
    const region = e.currentTarget as HTMLElement;
    if (!(e.relatedTarget instanceof Node && region.contains(e.relatedTarget))) store.release("focus");
  }
</script>

<!-- L4, bottom centre of the content area, 12px above the player bar; never in the layout flow. -->
<div class="toast-region" data-testid="toast-region" onfocusin={() => store.hold("focus")} onfocusout={onFocusOut}>
  {#each entries as entry (entry.key)}
    {@const toast = entry.toast}
    <div
      class="toast lg lg--popover lg-overlay"
      role={entry.leaving ? undefined : entry.live}
      aria-hidden={entry.leaving ? "true" : undefined}
      data-testid={entry.leaving ? undefined : `toast-${toast.kind}`}
      data-tone={toast.tone}
      data-leaving={entry.leaving || undefined}
      inert={Boolean(entry.leaving)}
      onpointerenter={() => store.hold("hover")}
      onpointerleave={() => store.release("hover")}
      {@attach (el) => trackEntry(el, entry.key)}
      {@attach glass}
    >
      <span class="toast-mark lg-overlay-content" aria-hidden="true"></span>
      <span class="toast-text lg-overlay-content" title={toast.detail ? `${toast.text}\n${toast.detail}` : toast.text}>{toast.text}</span>
      {#if toast.action}
        <button
          type="button"
          class="toast-action lg-press lg-overlay-content"
          onclick={() => runAction(toast)}
          disabled={toast.action.pending === true}
          aria-busy={toast.action.pending === true || undefined}
          data-pending={toast.action.pending || undefined}
          data-testid={entry.leaving ? undefined : (toast.action.testId ?? "toast-action")}
        >
          {toast.action.label}
        </button>
      {/if}
    </div>
  {/each}
</div>
