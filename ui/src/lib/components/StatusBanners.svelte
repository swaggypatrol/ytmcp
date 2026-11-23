<script lang="ts" module>
  /** Outcome of the latest explicit account write, shown until dismissed. */
  export interface MutationNotice {
    readonly tone: "pending" | "success" | "error" | "unknown";
    readonly text: string;
  }
</script>

<script lang="ts">
  import type { AppSnapshot, CommandReceipt, FakeScenario, QueryError } from "@ytmcp/protocol";
  import { FAKE_SCENARIO_PRESETS } from "../../fixtures/synthetic.js";
  import { t } from "../i18n/index.svelte.js";
  import type { ClientTransportMode } from "../ipc/client.js";

  interface Props {
    mode: ClientTransportMode;
    snapshot: AppSnapshot | null;
    connectionError: string | null;
    /** Transport-level failure of the latest player command (no receipt). */
    transportError: QueryError | null;
    lastReceipt: CommandReceipt | null;
    mutationNotice: MutationNotice | null;
    onDismissMutation: () => void;
    activeScenario: FakeScenario;
    onSelectScenario: (scenario: FakeScenario) => void;
    onOpenOfficialPlayer: () => void;
    onActivateMock: () => void;
    onRetryConnect?: () => void;
  }

  let {
    mode,
    snapshot,
    connectionError,
    transportError,
    lastReceipt,
    mutationNotice,
    onDismissMutation,
    activeScenario,
    onSelectScenario,
    onOpenOfficialPlayer,
    onActivateMock,
    onRetryConnect,
  }: Props = $props();

  let dismissedCommandKey = $state<string | null>(null);

  const isMock = $derived(mode === "mock" && snapshot?.mockMode === true);
  const isOffline = $derived(snapshot?.networkState === "offline");
  const isAuthExpired = $derived(
    snapshot?.authState.state === "expired" ||
      snapshot?.authState.state === "signedOut" ||
      snapshot?.authState.state === "blocked"
  );
  const isBridgeDisconnected = $derived(
    snapshot != null &&
      (snapshot.player.connection === "disconnected" ||
        snapshot.player.connection === "incompatible" ||
        snapshot.player.connection === "recovering")
  );
  const commandIndeterminate = $derived(lastReceipt?.status === "indeterminate");
  const commandError = $derived(
    transportError ?? (lastReceipt?.status === "failed" ? (lastReceipt.error ?? null) : null)
  );
  const commandKey = $derived(
    commandIndeterminate
      ? `ind:${lastReceipt?.commandId ?? ""}`
      : commandError
        ? `err:${lastReceipt?.commandId ?? ""}:${commandError.code}:${commandError.safeMessage}`
        : null
  );
  const showCommandBanner = $derived(commandKey !== null && commandKey !== dismissedCommandKey);
  const hasUnsupportedCore = $derived(
    snapshot != null &&
      (snapshot.capabilities.playPause === "unsupported" ||
        snapshot.capabilities.seek === "unsupported")
  );
  const hasConnectionError = $derived(mode === "preload" && connectionError !== null);
  const hasAnyBanner = $derived(
    mode === "unconnected" ||
      hasConnectionError ||
      isMock ||
      isAuthExpired ||
      isBridgeDisconnected ||
      isOffline ||
      hasUnsupportedCore ||
      showCommandBanner ||
      mutationNotice !== null
  );
</script>

{#if hasAnyBanner}
  <div class="flex flex-col gap-2 pt-4" data-testid="status-banners">
    {#if mode === "unconnected"}
      <div
        role="alert"
        class="flex flex-wrap items-center justify-between gap-3 px-4 py-3 surface-rounded-md border"
        style="background-color: var(--bg-elevated); border-color: var(--status-warning);"
        data-testid="banner-unconnected"
      >
        <div class="min-w-0 flex-1 type-sm" style="color: var(--text-primary);">
          {t.states.bridgeNotDetectedBanner}
          {#if connectionError && connectionError !== "BRIDGE_NOT_DETECTED"}
            <span class="ml-2 type-xs" style="color: var(--text-muted);">({connectionError})</span>
          {/if}
        </div>
        <button
          type="button"
          class="btn-primary shrink-0"
          onclick={onActivateMock}
        >
          {t.states.enableMockPreview}
        </button>
      </div>
    {:else if hasConnectionError}
      <div
        role="alert"
        class="flex flex-wrap items-center justify-between gap-3 px-4 py-3 surface-rounded-md border"
        style="background-color: var(--bg-elevated); border-color: var(--status-danger);"
        data-testid="banner-connection-error"
      >
        <div class="min-w-0 flex-1 type-sm" style="color: var(--text-primary);">
          <span class="font-semibold" style="color: var(--status-danger);">
            {t.states.connectionFailedBanner}
          </span>
          {#if connectionError}
            <span class="ml-2 type-xs" style="color: var(--text-secondary);">({connectionError})</span>
          {/if}
        </div>
        {#if onRetryConnect}
          <button
            type="button"
            class="btn-primary shrink-0"
            onclick={onRetryConnect}
            data-testid="btn-reconnect-core"
          >
            {t.states.reconnect}
          </button>
        {/if}
      </div>
    {/if}

    {#if isMock}
      <div
        role="status"
        class="flex flex-wrap items-center justify-between gap-3 px-4 py-2.5 surface-rounded-md border"
        style="background: linear-gradient(var(--accent-soft), var(--accent-soft)), var(--bg-elevated); border-color: var(--accent);"
        data-testid="banner-mock-mode"
      >
        <div class="min-w-0 flex-1">
          <span class="font-semibold type-sm" style="color: var(--accent);">
            {t.mock.bannerTitle}
          </span>
          <span class="ml-2 type-xs" style="color: var(--text-secondary);">
            {t.mock.bannerDescription}
          </span>
        </div>

        <label class="flex items-center gap-2 type-xs" style="color: var(--text-primary);">
          <span>{t.mock.scenarioLabel}</span>
          <select
            class="select py-1 px-2 type-xs"
            value={activeScenario}
            onchange={(e) => onSelectScenario((e.currentTarget as HTMLSelectElement).value as FakeScenario)}
            data-testid="mock-scenario-select"
          >
            {#each FAKE_SCENARIO_PRESETS as preset (preset.id)}
              <option value={preset.id}>{preset.label}</option>
            {/each}
          </select>
        </label>
      </div>
    {/if}

    {#if isAuthExpired}
      <div
        role="alert"
        class="flex flex-wrap items-center justify-between gap-3 px-4 py-3 surface-rounded-md border"
        style="background-color: var(--bg-elevated); border-color: var(--status-danger);"
        data-testid="banner-auth-expired"
      >
        <div class="min-w-0 flex-1 type-sm" style="color: var(--text-primary);">
          <strong class="font-semibold" style="color: var(--status-danger);">
            {snapshot?.authState.state === "blocked"
              ? t.states.authBlockedBanner
              : t.states.authExpiredBanner}
          </strong>
          {#if snapshot?.authState.reason}
            <span class="ml-2 type-xs" style="color: var(--text-secondary);">
              {snapshot.authState.reason}
            </span>
          {/if}
        </div>
        <button
          type="button"
          class="btn-primary shrink-0"
          onclick={onOpenOfficialPlayer}
          data-testid="btn-open-official-player"
        >
          {t.states.openOfficialPlayer}
        </button>
      </div>
    {/if}

    {#if isBridgeDisconnected}
      <div
        role="alert"
        class="flex flex-wrap items-center justify-between gap-3 px-4 py-3 surface-rounded-md border"
        style="background-color: var(--bg-elevated); border-color: var(--status-danger);"
        data-testid="banner-bridge-disconnected"
      >
        <div class="min-w-0 flex-1 type-sm" style="color: var(--text-primary);">
          <span class="font-semibold" style="color: var(--status-danger);">
            {t.states.bridgeDisconnectedBanner}
          </span>
          {#if snapshot?.hostHealth.lastError}
            <span class="ml-2 type-xs" style="color: var(--text-secondary);">
              ({snapshot.hostHealth.lastError})
            </span>
          {/if}
        </div>
        <button
          type="button"
          class="btn-secondary shrink-0"
          onclick={onOpenOfficialPlayer}
        >
          {t.states.openOfficialPlayer}
        </button>
      </div>
    {/if}

    {#if isOffline}
      <div
        role="status"
        class="flex items-center gap-2 px-4 py-2.5 surface-rounded-md border type-sm"
        style="background-color: var(--bg-elevated); border-color: var(--status-warning); color: var(--text-primary);"
        data-testid="banner-offline"
      >
        <span>{t.states.offlineBanner}</span>
      </div>
    {/if}

    {#if hasUnsupportedCore}
      <div
        role="status"
        class="px-4 py-2.5 surface-rounded-md border type-sm"
        style="background-color: var(--bg-elevated); border-color: var(--status-warning); color: var(--text-primary);"
        data-testid="banner-unsupported"
      >
        {t.states.unsupportedBanner}
      </div>
    {/if}

    {#if showCommandBanner && commandIndeterminate}
      <div
        role="alert"
        class="flex flex-wrap items-center justify-between gap-3 px-4 py-2.5 surface-rounded-md border type-sm"
        style="background-color: var(--bg-elevated); border-color: var(--status-warning); color: var(--text-primary);"
        data-testid="banner-command-indeterminate"
      >
        <div class="min-w-0 flex-1">
          {t.states.commandUnknown}
          {#if lastReceipt?.error}
            <span class="ml-1.5 type-xs" style="color: var(--text-secondary);">{lastReceipt.error.safeMessage}</span>
          {/if}
        </div>
        <button
          type="button"
          class="btn-ghost shrink-0"
          onclick={() => (dismissedCommandKey = commandKey)}
          data-testid="dismiss-command-banner"
        >
          {t.common.dismiss}
        </button>
      </div>
    {:else if showCommandBanner && commandError}
      <div
        role="alert"
        class="flex flex-wrap items-center justify-between gap-3 px-4 py-2.5 surface-rounded-md border type-sm"
        style="background-color: var(--bg-elevated); border-color: var(--status-warning); color: var(--text-primary);"
        data-testid="banner-command-error"
      >
        <div class="min-w-0 flex-1">
          <span class="font-semibold" style="color: var(--status-warning);">
            {t.states.commandFailed} [{commandError.code}]
          </span>
          <span class="ml-1.5">{commandError.safeMessage}</span>
        </div>
        <button
          type="button"
          class="btn-ghost shrink-0"
          onclick={() => (dismissedCommandKey = commandKey)}
          data-testid="dismiss-command-banner"
        >
          {t.common.dismiss}
        </button>
      </div>
    {/if}

    {#if mutationNotice}
      <div
        role="status"
        class="flex flex-wrap items-center justify-between gap-3 px-4 py-2.5 surface-rounded-md border type-sm"
        style="background-color: var(--bg-elevated); border-color: {mutationNotice.tone === 'success'
          ? 'var(--status-success)'
          : mutationNotice.tone === 'pending'
            ? 'var(--status-info)'
            : 'var(--status-warning)'};"
        data-testid="banner-mutation"
        data-tone={mutationNotice.tone}
      >
        <span>{mutationNotice.text}</span>
        {#if mutationNotice.tone !== "pending"}
          <button type="button" class="btn-ghost" onclick={onDismissMutation}>{t.common.dismiss}</button>
        {/if}
      </div>
    {/if}
  </div>
{/if}
