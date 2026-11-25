<script lang="ts">
  import { tick } from "svelte";
  import type { ProblemEntry } from "../feedback/problems.svelte.js";
  import { getLocale, t } from "../i18n/index.svelte.js";

  interface Props {
    /** Newest first. */
    entries: readonly ProblemEntry[];
    /** The entry to bring into view and focus; a new `seq` asks again for the same entry. */
    focus?: { readonly id: string; readonly seq: number } | null;
    onMarkChecked: (id: string) => void;
    onOpenOfficialPlayer: () => void;
  }

  let { entries, focus = null, onMarkChecked, onOpenOfficialPlayer }: Props = $props();

  let listEl = $state<HTMLElement | null>(null);

  function timeOf(atMs: number): string {
    return new Intl.DateTimeFormat(getLocale(), { hour: "2-digit", minute: "2-digit", second: "2-digit" }).format(atMs);
  }

  // Lands on the entry a toast's "View diagnostics" named: after the dialog has put focus on its
  // first control (a microtask after opening), so this focus is the one that stays.
  $effect(() => {
    const target = focus;
    if (!target) return;
    void tick().then(() =>
      setTimeout(() => {
        const el = listEl?.querySelector<HTMLElement>(`[data-problem-id="${target.id}"]`);
        if (!el) return;
        el.scrollIntoView?.({ block: "nearest" });
        el.focus({ preventScroll: true });
      }, 0)
    );
  });
</script>

<!-- Failures and unclear outcomes of this session and account, so a missed toast can be found again. -->
<section class="flex flex-col gap-2" aria-labelledby="recent-problems-heading" data-testid="diagnostics-problems">
  <h3 id="recent-problems-heading" class="type-sm font-semibold m-0">{t.dialog.recentProblems}</h3>
  {#if entries.length === 0}
    <p class="type-xs m-0" style="color: var(--text-muted);" data-testid="diagnostics-problems-empty">{t.dialog.noRecentProblems}</p>
  {:else}
    <ol class="recent-problems" bind:this={listEl}>
      {#each entries as entry (entry.id)}
        <li
          class="recent-problem p-3 rounded-lg border"
          data-tone={entry.tone}
          data-problem-id={entry.id}
          data-unchecked={entry.unknownOutcome && !entry.checked ? "" : undefined}
          tabindex="-1"
          data-testid="problem-entry"
        >
          <div class="recent-problem-head">
            <span class="recent-problem-mark" aria-hidden="true"></span>
            <p class="type-sm m-0 min-w-0">{entry.text}</p>
            <time class="type-xs shrink-0" style="color: var(--text-muted);" datetime={new Date(entry.atMs).toISOString()}>
              {timeOf(entry.atMs)}
            </time>
          </div>
          {#if entry.subject && !entry.text.includes(entry.subject)}
            <p class="type-xs m-0 break-words">“{entry.subject}”</p>
          {/if}
          {#if entry.advice}
            <p class="type-xs m-0" style="color: var(--text-secondary);">{entry.advice}</p>
          {/if}
          {#if entry.unknownOutcome}
            <div class="flex flex-wrap items-center gap-2">
              {#if entry.checked}
                <span class="type-xs" style="color: var(--text-muted);" data-testid="problem-checked">{t.dialog.problemChecked}</span>
              {:else}
                <span class="type-xs" style="color: var(--status-warning);" data-testid="problem-not-checked">{t.dialog.problemNotChecked}</span>
                <button type="button" class="btn-ghost" onclick={onOpenOfficialPlayer}>{t.toast.openPlayer}</button>
                <button type="button" class="btn-secondary" onclick={() => onMarkChecked(entry.id)} data-testid="problem-mark-checked">
                  {t.dialog.markProblemChecked}
                </button>
              {/if}
            </div>
          {/if}
          {#if entry.technical}
            <details class="type-xs">
              <summary class="cursor-pointer" style="color: var(--text-secondary);">{t.dialog.problemDetails}</summary>
              <p class="font-mono m-0 mt-1 break-all" data-testid="problem-technical">{entry.technical}</p>
            </details>
          {/if}
        </li>
      {/each}
    </ol>
  {/if}
</section>

<style>
  .recent-problems {
    display: flex;
    flex-direction: column;
    gap: var(--space-8);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .recent-problem {
    display: flex;
    flex-direction: column;
    gap: var(--space-8);
    border-color: var(--border-subtle);
  }
  /* Landed on from a toast, by keyboard or pointer: the entry's edge shows where it is. */
  .recent-problem:focus {
    border-color: var(--focus-ring);
  }
  .recent-problem-head {
    display: flex;
    align-items: baseline;
    gap: var(--space-8);
  }
  .recent-problem-head > p {
    flex: 1;
  }
  /* The toast's mark: Peach for a failure, Lemon for unclear or unavailable. */
  .recent-problem-mark {
    flex: none;
    align-self: center;
    width: 8px;
    height: 8px;
    border-radius: var(--radius-pill);
    background-color: var(--status-warning);
  }
  .recent-problem[data-tone="danger"] .recent-problem-mark {
    background-color: var(--status-danger);
  }
</style>
