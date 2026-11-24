<script lang="ts">
  import type { MomentDesktopSnapshot, OwnerMomentConfigPatch, QueryErrorCode } from "@ytmcp/protocol";
  import { onDestroy, untrack } from "svelte";
  import { getLocale, t } from "../i18n/index.svelte.js";
  import { queryErrorDetail, queryErrorMessage } from "../i18n/query-errors.js";
  import { CoreRequestError, type YtmUiClient } from "../ipc/client.js";

  interface Props {
    client: Pick<YtmUiClient, "momentSettings">;
    scopeKey: string | null;
    revision: number;
  }

  let { client, scopeKey, revision }: Props = $props();
  const copy = $derived(t.settings.moment);

  let moment = $state.raw<MomentDesktopSnapshot | null>(null);
  let loadedScope = $state<string | null>(null);
  let previewPrincipal = $state<string>("");
  let sceneCityDraft = $state<string>("");
  /** The saved city the draft last took over; a typed but unsaved city survives other saves. */
  let syncedCity: string | null = null;
  interface RequestError {
    code?: QueryErrorCode;
    detail?: string;
  }
  let loadError = $state.raw<RequestError | null>(null);
  let saveError = $state.raw<RequestError | null>(null);
  let saving = $state(false);
  let requestSeq = 0;
  let lastAttemptedKey = "";
  let destroyed = false;

  onDestroy(() => {
    destroyed = true;
  });

  function requestError(err: unknown): RequestError {
    if (err instanceof CoreRequestError) return { code: err.error.code, detail: queryErrorDetail(err.error) };
    return { detail: err instanceof Error ? err.message : undefined };
  }

  function applySnapshot(next: MomentDesktopSnapshot, scope: string) {
    moment = next;
    loadedScope = scope;
    if (next.previewPrincipal) {
      previewPrincipal = next.previewPrincipal;
    } else if (next.registeredPrincipals?.length) {
      previewPrincipal = next.registeredPrincipals[0]!;
    } else {
      previewPrincipal = "";
    }
    const city = next.config.sceneCity ?? "";
    if (city !== syncedCity) {
      syncedCity = city;
      sceneCityDraft = city;
    }
  }

  async function load(scope: string, principal?: string) {
    const seq = ++requestSeq;
    loadError = null;
    try {
      const next = await client.momentSettings({
        action: "read",
        previewPrincipal: principal || previewPrincipal || null,
      });
      if (destroyed || seq !== requestSeq || scope !== scopeKey) return;
      applySnapshot(next, scope);
    } catch (err) {
      if (destroyed || seq !== requestSeq || scope !== scopeKey) return;
      loadError = requestError(err);
    }
  }

  /**
   * Saves one change. `revert` puts the control back when the save fails: a switch or menu the
   * user moved keeps showing their choice otherwise, since its bound value never changed.
   */
  async function updatePatch(patch: OwnerMomentConfigPatch, revert?: () => void) {
    const scope = scopeKey;
    if (!scope || saving) return;
    saving = true;
    saveError = null;
    try {
      const next = await client.momentSettings({
        action: "update",
        patch: {
          ...patch,
          previewPrincipal: patch.previewPrincipal ?? (previewPrincipal || null),
        },
      });
      if (destroyed || scope !== scopeKey) return;
      applySnapshot(next, scope);
    } catch (err) {
      if (destroyed || scope !== scopeKey) return;
      saveError = requestError(err);
      revert?.();
    } finally {
      if (!destroyed) saving = false;
    }
  }

  function toggle(e: Event & { currentTarget: HTMLInputElement }, patch: (on: boolean) => OwnerMomentConfigPatch) {
    const input = e.currentTarget;
    void updatePatch(patch(input.checked), () => (input.checked = !input.checked));
  }

  function choose(
    e: Event & { currentTarget: HTMLSelectElement },
    previous: string,
    patch: (value: string) => OwnerMomentConfigPatch
  ) {
    const select = e.currentTarget;
    void updatePatch(patch(select.value), () => (select.value = previous));
  }

  $effect(() => {
    const scope = scopeKey;
    const rev = revision;
    if (!scope) return;
    const key = `${scope}|${rev}`;
    if (key === lastAttemptedKey) return;
    lastAttemptedKey = key;
    if (loadedScope !== scope) syncedCity = null;
    untrack(() => void load(scope));
  });

  const countries = $derived(moment?.environment?.countries ?? []);
  const selectedCountryCode = $derived(moment?.config.holidayCountry ?? "");
  const selectedCountry = $derived(countries.find((c) => c.code === selectedCountryCode) ?? null);
  const states = $derived(selectedCountry?.states ?? []);
  const sharedPrincipals = $derived.by(() => {
    if (!moment) return new Set<string>();
    const map = moment.config.sharedPrincipalsByAccount ?? {};
    return new Set(map[moment.currentAccountKey] ?? []);
  });
  // An assistant whose registration is gone is still listed while shared, so it can be switched off.
  const principals = $derived([
    ...new Set([...(moment?.registeredPrincipals ?? []), ...sharedPrincipals]),
  ]);

  /** The parts of the projection the preview reads; the raw JSON below is the same object. */
  interface PreviewView {
    environment?: {
      status?: string;
      time?: { localDate: string; dayPart: string; timeZone: string };
      occasions?: { label: string; phase: string }[];
      sceneCity?: { label: string };
    };
    listeningIntents?: { status?: string; items?: unknown[] };
  }
  const preview = $derived((moment?.previewJson ?? null) as PreviewView | null);

  /** The preview in plain words: one line per thing the assistant would learn. */
  const previewLines = $derived.by(() => {
    const env = preview?.environment;
    const lines: string[] = [];
    if (env?.status === "notShared") lines.push(copy.previewNotShared);
    else if (env?.status === "unavailable") lines.push(copy.previewUnavailable);
    else if (env) {
      if (env.time) {
        const weekday = new Date(`${env.time.localDate}T12:00:00Z`).toLocaleDateString(getLocale(), {
          weekday: "long",
          timeZone: "UTC",
        });
        const dayPart = (copy.dayParts as Record<string, string>)[env.time.dayPart] ?? env.time.dayPart;
        lines.push(`${weekday} · ${dayPart} · ${env.time.timeZone}`);
      }
      for (const occasion of env.occasions ?? []) {
        lines.push(occasion.phase === "eve" ? `${occasion.label} · ${copy.previewEve}` : occasion.label);
      }
      if (env.sceneCity) lines.push(copy.previewCity.replace("{city}", env.sceneCity.label));
      if (lines.length === 0) lines.push(copy.previewNoSignals);
    }
    const intents = preview?.listeningIntents;
    if (intents?.status === "notShared") lines.push(copy.previewIntentsNotShared);
    else if (intents?.status === "unavailable") lines.push(copy.previewIntentsUnavailable);
    else if (intents) {
      const count = intents.items?.length ?? 0;
      lines.push(count === 0 ? copy.previewIntentsNone : copy.previewIntentsCount.replace("{count}", String(count)));
    }
    return lines;
  });
</script>

<div class="card p-5 flex flex-col gap-4" data-testid="moment-settings-section">
  <div class="flex flex-col gap-1.5">
    <h2 class="type-lg font-semibold m-0">{copy.title}</h2>
    <p class="type-xs m-0" style="color: var(--text-secondary);">{copy.disclosure}</p>
  </div>

  {#if loadError}
    <p class="type-xs m-0" style="color: var(--status-danger);" role="status" title={loadError.detail} data-testid="moment-load-error">
      {copy.loadFailed} {queryErrorMessage(loadError.code, "")}
    </p>
  {/if}
  {#if saveError}
    <p class="type-xs m-0" style="color: var(--status-danger);" role="status" title={saveError.detail} data-testid="moment-save-error">
      {copy.saveFailed} {queryErrorMessage(saveError.code, "")}
    </p>
  {/if}

  {#if moment && loadedScope === scopeKey}
    <div class="flex flex-col gap-3">
      <label class="setting-row">
        <span>{copy.shareTime}</span>
        <input
          type="checkbox"
          role="switch"
          class="toggle"
          disabled={saving}
          checked={Boolean(moment.config.shareTime)}
          onchange={(e) => toggle(e, (on) => ({ shareTime: on }))}
          data-testid="moment-share-time"
        />
      </label>

      <label class="setting-row">
        <span>{copy.holidaysEnabled}</span>
        <input
          type="checkbox"
          role="switch"
          class="toggle"
          disabled={saving}
          checked={Boolean(moment.config.holidaysEnabled)}
          onchange={(e) => toggle(e, (on) => ({ holidaysEnabled: on }))}
          data-testid="moment-holidays-enabled"
        />
      </label>

      <label class="setting-row" class:opacity-60={!moment.config.holidaysEnabled}>
        <span>{copy.includeCulturalObservances}</span>
        <input
          type="checkbox"
          role="switch"
          class="toggle"
          disabled={saving || !moment.config.holidaysEnabled}
          checked={Boolean(moment.config.includeCulturalObservances)}
          onchange={(e) => toggle(e, (on) => ({ includeCulturalObservances: on }))}
          data-testid="moment-cultural-observances"
        />
      </label>

      <label class="setting-row">
        <span>{copy.holidayCountry}</span>
        <select
          class="select"
          disabled={saving}
          value={selectedCountryCode}
          onchange={(e) => choose(e, selectedCountryCode, (code) => ({ holidayCountry: code, holidayState: "" }))}
          data-testid="moment-holiday-country"
        >
          <option value="">{copy.holidayCountryPlaceholder}</option>
          {#each countries as country (country.code)}
            <option value={country.code}>{country.name} ({country.code})</option>
          {/each}
        </select>
      </label>

      {#if states.length > 0}
        <label class="setting-row">
          <span>{copy.holidayState}</span>
          <select
            class="select"
            disabled={saving}
            value={moment.config.holidayState ?? ""}
            onchange={(e) => choose(e, moment?.config.holidayState ?? "", (code) => ({ holidayState: code }))}
            data-testid="moment-holiday-state"
          >
            <option value="">{copy.holidayStateNone}</option>
            {#each states as st (st.code)}
              <option value={st.code}>{st.name} ({st.code})</option>
            {/each}
          </select>
        </label>
      {/if}

      <div class="flex flex-col gap-1.5">
        <label class="type-sm" for="moment-scene-city-input">{copy.sceneCity}</label>
        <div class="flex flex-wrap items-center gap-2">
          <input
            id="moment-scene-city-input"
            type="text"
            maxlength="80"
            class="search-field px-3 py-1.5 type-sm"
            placeholder={copy.sceneCityPlaceholder}
            bind:value={sceneCityDraft}
            disabled={saving}
            data-testid="moment-scene-city-input"
          />
          <button
            type="button"
            class="btn-secondary"
            disabled={saving}
            onclick={() => void updatePatch({ sceneCity: sceneCityDraft.trim() })}
            data-testid="moment-scene-city-save"
          >
            {copy.sceneCitySave}
          </button>
          {#if moment.config.sceneCity}
            <button
              type="button"
              class="btn-ghost"
              disabled={saving}
              onclick={() => {
                sceneCityDraft = "";
                void updatePatch({ sceneCity: "" });
              }}
              data-testid="moment-scene-city-clear"
            >
              {copy.sceneCityClear}
            </button>
          {/if}
        </div>
      </div>

      <label class="setting-row">
        <span>{copy.shareListeningIntents}</span>
        <input
          type="checkbox"
          role="switch"
          class="toggle"
          disabled={saving}
          checked={moment.config.shareListeningIntents !== false}
          onchange={(e) => toggle(e, (on) => ({ shareListeningIntents: on }))}
          data-testid="moment-share-intents"
        />
      </label>
    </div>

    <div class="flex flex-col gap-2 pt-2 border-t" style="border-color: var(--border-subtle);">
      <h3 class="type-sm font-semibold m-0">{copy.allowedClientsTitle}</h3>
      {#if principals.length === 0}
        <p class="type-xs m-0" style="color: var(--text-secondary);" data-testid="moment-no-principals">{copy.noClients}</p>
      {:else}
        <div class="flex flex-col gap-2">
          {#each principals as principal (principal)}
            <label class="setting-row">
              <span>{copy.clientCheckboxLabel.replace("{principal}", principal)}</span>
              <input
                type="checkbox"
                role="switch"
                class="toggle"
                disabled={saving}
                checked={sharedPrincipals.has(principal)}
                onchange={(e) => toggle(e, (on) => ({ setPrincipalShared: { principal, shared: on } }))}
                data-testid="moment-principal-{principal}"
              />
            </label>
          {/each}
        </div>
      {/if}
    </div>

    <div class="flex flex-col gap-2 pt-2 border-t" style="border-color: var(--border-subtle);">
      <div class="flex flex-wrap items-center justify-between gap-2">
        <h3 class="type-sm font-semibold m-0">{copy.previewTitle}</h3>
        {#if principals.length > 0}
          <label class="flex items-center gap-2 type-xs">
            <span style="color: var(--text-secondary);">{copy.previewPrincipal}</span>
            <select
              class="select"
              value={previewPrincipal}
              onchange={(e) => {
                previewPrincipal = e.currentTarget.value;
                if (scopeKey) void load(scopeKey, e.currentTarget.value);
              }}
              data-testid="moment-preview-principal"
            >
              {#each principals as principal (principal)}
                <option value={principal}>{principal}</option>
              {/each}
            </select>
          </label>
        {/if}
      </div>
      <ul class="flex flex-col gap-1 m-0 p-0 list-none type-sm" data-testid="moment-preview-lines">
        {#each previewLines as line, index (index)}
          <li>{line}</li>
        {/each}
      </ul>
      <details class="type-xs">
        <summary class="cursor-pointer" style="color: var(--text-secondary);">{copy.previewRaw}</summary>
        <pre
          class="overflow-x-auto p-3 mt-2 rounded-lg m-0"
          style="background-color: var(--bg-subtle); border: 1px solid var(--border-subtle);"
          data-testid="moment-preview-json"
        >{JSON.stringify(moment.previewJson, null, 2)}</pre>
      </details>
    </div>
  {:else if !loadError}
    <p class="type-xs m-0" style="color: var(--text-secondary);" data-testid="moment-loading">{copy.loading}</p>
  {/if}
</div>
