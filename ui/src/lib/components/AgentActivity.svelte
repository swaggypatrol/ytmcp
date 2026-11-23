<script lang="ts">
  import type {
    AgentActivityRequest,
    AgentActivitySnapshot,
    AgentOperationPhase,
    AgentOperationSummary,
    AgentPreferenceSummary,
  } from "@ytm-glass/protocol";
  import { onDestroy, untrack } from "svelte";
  import { t } from "../i18n/index.svelte.js";
  import type { YtmUiClient } from "../ipc/client.js";

  interface Props {
    client: Pick<YtmUiClient, "agentActivity">;
    scopeKey: string | null;
    revision: number;
    visible: boolean;
    /** Opens the official player page; only ever called from the user's click. */
    onOpenOfficialPlayer?: () => void;
  }

  interface ReadRequest {
    scope: string;
    revision: number;
    reload: number;
  }

  let { client, scopeKey, revision, visible, onOpenOfficialPlayer }: Props = $props();
  const copy = $derived(t.agentActivity);
  const phaseTone: Partial<Record<AgentOperationPhase, string>> = {
    verified: "var(--status-success)",
    unknown: "var(--status-warning)",
    partial: "var(--status-warning)",
    rejected: "var(--status-danger)",
  };

  let saved = $state.raw<{ scope: string; snapshot: AgentActivitySnapshot } | null>(null);
  let attempted = $state.raw<ReadRequest | null>(null);
  let readError = $state<string | null>(null);
  let actionError = $state<{ scope: string; stale: boolean } | null>(null);
  let reading = $state(false);
  let acting = $state(false);
  let reload = $state(0);
  let destroyed = false;

  /** An MCP client asked the user to sign in, renew or switch accounts and is waiting. */
  const awaitsAccountHandoff = (operation: AgentOperationSummary) =>
    operation.resource === "accountHandoff" && operation.phase === "acknowledged";

  function resourceLabel(resource: string): string {
    return (copy.resources as Record<string, string>)[resource] ?? resource;
  }

  function stanceLabel(preference: AgentPreferenceSummary): string {
    return copy.stances[preference.stance] ?? preference.stance;
  }

  const activity = $derived(saved && saved.scope === scopeKey ? saved.snapshot : null);
  const busy = $derived(reading || acting);
  const actionsDisabled = $derived(busy || !visible || (readError !== null && readError === scopeKey));

  async function read(request: ReadRequest) {
    attempted = request;
    reading = true;
    readError = null;
    try {
      const snapshot = await client.agentActivity({ action: "read" });
      if (!destroyed && request.scope === scopeKey) saved = { scope: request.scope, snapshot };
    } catch {
      if (!destroyed && request.scope === scopeKey) readError = request.scope;
    } finally {
      if (!destroyed) reading = false;
    }
  }

  $effect(() => {
    const scope = scopeKey;
    if (!visible || scope === null || busy) return;
    if (attempted?.scope === scope && attempted.revision === revision && attempted.reload === reload) return;
    const request = { scope, revision, reload };
    untrack(() => void read(request));
  });

  async function act(request: Exclude<AgentActivityRequest, { action: "read" }>) {
    const scope = scopeKey;
    if (scope === null || actionsDisabled) return;
    acting = true;
    actionError = null;
    try {
      const snapshot = await client.agentActivity(request);
      if (!destroyed && scope === scopeKey) saved = { scope, snapshot };
    } catch (error) {
      if (!destroyed && scope === scopeKey) {
        actionError = {
          scope,
          stale: typeof error === "object" && error !== null && "code" in error && error.code === "STALE_CONTEXT",
        };
        reload += 1;
      }
    } finally {
      if (!destroyed) acting = false;
    }
  }

  function refresh() {
    actionError = null;
    reload += 1;
  }

  onDestroy(() => { destroyed = true; });
</script>

<section class="flex flex-col gap-5" aria-labelledby="agent-activity-heading" data-testid="view-agent-activity">
  <div class="flex flex-wrap items-center justify-between gap-3">
    <h1 id="agent-activity-heading" class="type-xl font-bold tracking-tight">{copy.title}</h1>
    <button type="button" class="btn-secondary" disabled={busy || !visible} onclick={refresh}>
      {reading ? copy.refreshing : copy.refresh}
    </button>
  </div>

  {#if actionError?.scope === scopeKey}
    <p class="type-sm" role="status" style="color: var(--status-warning);">
      {actionError.stale ? copy.stale : copy.actionUnconfirmed}
    </p>
  {/if}
  {#if readError !== null && readError === scopeKey}
    <p class="type-sm" role="status" style="color: var(--status-warning);">{copy.readFailed}</p>
  {/if}

  {#if activity}
    <div class="flex flex-col gap-4">
      <section class="card min-w-0 p-5 flex flex-col gap-3" aria-labelledby="agent-operations-heading">
        <h2 id="agent-operations-heading" class="type-lg font-semibold">{copy.operations}</h2>
        {#if activity.operations.length === 0}
          <p class="type-sm" style="color: var(--text-secondary);">{copy.noOperations}</p>
        {:else}
          <ul class="operation-list">
            {#each activity.operations as operation (JSON.stringify([operation.principal, operation.operationId]))}
              <li class="flex flex-col gap-2 py-3">
                <div class="flex flex-wrap items-start justify-between gap-x-4 gap-y-2">
                  <div class="min-w-0 flex flex-col gap-0.5">
                    <p class="type-sm font-medium wrap-anywhere">{operation.operationId}</p>
                    <p class="type-xs wrap-anywhere" style="color: var(--text-secondary);">
                      {operation.principal}{#if operation.resource} · {resourceLabel(operation.resource)}{/if}
                    </p>
                  </div>
                  <span class="chip" style:color={phaseTone[operation.phase]} data-testid="agent-operation-phase">
                    {copy.phases[operation.phase]}
                  </span>
                </div>
                {#if awaitsAccountHandoff(operation)}
                  <div class="flex flex-wrap items-center gap-x-4 gap-y-2" data-testid="agent-account-handoff">
                    <p class="type-sm" style="color: var(--status-warning);">{copy.accountHandoffPrompt}</p>
                    {#if onOpenOfficialPlayer}
                      <button type="button" class="btn-secondary" onclick={() => onOpenOfficialPlayer()}>
                        {copy.openOfficialPlayer}
                      </button>
                    {/if}
                  </div>
                {/if}
                {#if operation.totalItems > 0 || operation.canCancel}
                  <div class="flex items-center flex-wrap gap-x-4 gap-y-2">
                    {#if operation.totalItems > 0}
                      <p class="type-xs tabular-nums" style="color: var(--text-secondary);">
                        {copy.itemProgress.replace("{verified}", String(operation.verifiedItems)).replace("{total}", String(operation.totalItems))}
                      </p>
                    {/if}
                    {#if operation.canCancel}
                      <button
                        type="button"
                        class="btn-ghost"
                        disabled={actionsDisabled}
                        aria-label={copy.cancelFor.replace("{principal}", operation.principal).replace("{operation}", operation.operationId)}
                        onclick={() => void act({ action: "cancel", principal: operation.principal, operationId: operation.operationId })}
                      >{copy.cancelOperation}</button>
                    {/if}
                  </div>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </section>

      <section class="card min-w-0 p-5 flex flex-col gap-3" aria-labelledby="agent-preferences-heading">
        <h2 id="agent-preferences-heading" class="type-lg font-semibold">{copy.preferences}</h2>
        {#if (activity.preferences ?? []).length === 0}
          <p class="type-sm" style="color: var(--text-secondary);">{copy.noPreferences}</p>
        {:else}
          <ul class="operation-list">
            {#each activity.preferences ?? [] as preference (preference.preferenceId)}
              {@const name = preference.label || preference.subjectId}
              <li class="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 py-3" data-testid="agent-preference">
                <div class="min-w-0 flex flex-col gap-1">
                  <p class="type-sm font-medium wrap-anywhere">{name}</p>
                  <div class="flex flex-wrap items-center gap-2 type-xs" style="color: var(--text-secondary);">
                    <span class="chip" data-testid="agent-preference-stance">{stanceLabel(preference)}</span>
                    <span class="wrap-anywhere">{copy.recordedBy.replace("{client}", preference.recordedBy)}</span>
                  </div>
                </div>
                <button
                  type="button"
                  class="btn-ghost"
                  disabled={actionsDisabled}
                  aria-label={copy.deletePreferenceFor.replace("{name}", name)}
                  onclick={() => void act({ action: "clearDjPreference", preferenceId: preference.preferenceId })}
                >{copy.deletePreference}</button>
              </li>
            {/each}
          </ul>
          {#if (activity.preferencesTotal ?? 0) > (activity.preferences ?? []).length}
            <p class="type-xs" style="color: var(--text-secondary);" data-testid="agent-preferences-truncated">
              {copy.preferencesTruncated
                .replace("{shown}", String((activity.preferences ?? []).length))
                .replace("{total}", String(activity.preferencesTotal))}
            </p>
          {/if}
        {/if}
      </section>
    </div>
  {:else if readError === null || readError !== scopeKey}
    <div class="state-block" role="status">{t.states.loading}</div>
  {/if}
</section>

<style>
  .operation-list > li {
    border-top: 1px solid var(--border-subtle);
  }

  .operation-list > li:last-child {
    padding-bottom: 0;
  }
</style>
