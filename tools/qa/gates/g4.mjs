// G4: search, library playlists (page 1 and 2), playlist track paging, offline cached
// browsing, account consistency, provider version, and failure isolation between the
// catalog path and the playback bridge. Private titles never leave the app process.
import { check } from "../lib/evidence.mjs";
import { cgroupOf, cutNetwork, describeProcesses, restoreNetwork, sha256Short, sinkInputsFor, sleep, NULL_SINK } from "../lib/sys.mjs";
import { catalogPage, ensurePlaying, navigate, nextCatalogPage, pageMedia, uiSearch } from "./common.mjs";

const LIBRARY_TAIL = '[data-testid="view-library"] [data-testid="library-tail"]';
const PLAYLIST_TAIL = '[data-testid="view-playlist"] [data-testid="track-list-tail"]';
const BRIDGE_ACTION = '[data-testid="toast-bridge-disconnected"] [data-testid="btn-open-official-player"]';

function dupes(a, b) {
  const set = new Set(a);
  return b.filter((x) => set.has(x)).length;
}

export async function searchCheck(app, query) {
  const r = await uiSearch(app, query);
  return {
    measured: r,
    check: check("G4.search", "search through the real search box returns songs rendered in the UI", {
      measured: { querySent: r.querySent, ack: r.queryAck, itemCount: r.response?.itemCount ?? null, freshness: r.response?.freshness ?? null, renderedRows: r.renderedRows, providerVersion: r.response?.providerVersion ?? null, error: r.response?.error ?? null },
      pass: r.querySent && (r.response?.itemCount ?? 0) > 0 && r.renderedRows > 0 && r.response?.freshness === "fresh",
    }),
  };
}

async function libraryTileCount(app, ids) {
  if (new Set(ids).size !== ids.length) return null;
  return (await app.waitFor(() => app.evalMain(`(() => {
    const tiles = [...document.querySelectorAll('[data-testid="view-library"] [data-testid^="playlist-card-"]')];
    const rendered = new Set(tiles.map((tile) => tile.dataset.testid.slice("playlist-card-".length)));
    const matches = ${JSON.stringify(ids)}.filter((id) => rendered.has(id)).length;
    return matches === ${ids.length} && tiles.length === rendered.size ? { matches, total: tiles.length } : null;
  })()`), { timeoutMs: 6_000 })).value;
}

function validFreshPage(exchange, kind, accountScope, playlistId = null) {
  const { request, page } = exchange;
  return request?.ack?.ok === true && page?.resultType === kind && page.freshness === "fresh" &&
    Boolean(accountScope) && page.accountScope === accountScope && page.playlistId === playlistId &&
    Array.isArray(page.ids) && page.ids.every(Boolean) && page.ids.length === page.itemCount;
}

/** Library page 1, then its automatic continuation when the provider has a cursor. */
export async function libraryPagingCheck(app) {
  await navigate(app, "home");
  const since = (await app.state()).seq;
  const start = await app.snapshot();
  const accountScope = sha256Short(start?.authState?.accountScope);
  await navigate(app, "library");
  let first = await catalogPage(app, { since, kind: "libraryPlaylists" });
  if (!first.request) {
    // A session that already has the page skips the first request until the UI reloads.
    first = (await readLibraryAfterUiReload(app)).exchange;
  }
  const p1 = first.page;
  const second = p1?.nextCursorId ? await nextCatalogPage(app, first, { kind: "libraryPlaylists", tail: LIBRARY_TAIL }) : null;
  const p2 = second?.page ?? null;
  const ids1 = p1?.ids ?? [];
  const ids2 = p2?.ids ?? [];
  const tiles = ids1.length ? await libraryTileCount(app, [...ids1, ...ids2]) : null;
  const end = await app.snapshot();
  const sameAccount = Boolean(start?.accountEpoch && end?.accountEpoch === start.accountEpoch && end.authState?.playerCatalogMatched);
  const measured = {
    page1: p1 ? { itemCount: p1.itemCount, hasNextCursor: p1.hasNextCursor, freshness: p1.freshness, providerVersion: p1.providerVersion, accountScope: p1.accountScope } : null,
    page2: p2 ? { itemCount: p2.itemCount, hasNextCursor: p2.hasNextCursor, freshness: p2.freshness } : null,
    cursorRequested: second?.request?.query?.cursor === true,
    scrolledToTail: second?.scrolledToTail ?? null,
    page2Error: second?.reply?.error?.code ?? p2?.error?.code ?? null,
    page2UiError: await app.exists('[data-testid="library-retry"]'),
    duplicatesAcrossPages: dupes(ids1, ids2),
    uiTiles: tiles,
    sameAccount,
  };
  const page2Needed = Boolean(p1?.hasNextCursor);
  return {
    measured,
    ids1,
    counts1: p1?.counts ?? [],
    check: check("G4.libraryPaging", "library playlists page 1 and (when a cursor exists) page 2 via the UI: no duplicates, UI shows p1+p2", {
      measured,
      // YouTube Music hands out a grid cursor even after the last playlist; that page comes back
      // empty and ends paging, which is a valid page 2.
      pass: Boolean(sameAccount && validFreshPage(first, "libraryPlaylists", accountScope) && p1.itemCount > 0 && tiles &&
        (page2Needed ? second && validFreshPage(second, "libraryPlaylists", accountScope) && measured.duplicatesAcrossPages === 0 &&
          (p2.itemCount > 0 || !p2.hasNextCursor) && (p2.hasNextCursor || tiles.total === ids1.length + ids2.length)
          : tiles.total === ids1.length)),
      note: !page2Needed
        ? "account has a single library page; real page-2 paging not exercised (fixture-only per M0_CHECKLIST)"
        : p2 && p2.itemCount === 0
          ? "page 2 was the provider's empty end-of-list page: the cursor round-trip is real, multi-page item paging is not exercised by this library"
          : null,
    }),
  };
}

/** Opens the largest library playlist and pages its tracks once. */
export async function playlistPagingCheck(app, playlistIds, counts) {
  let best = null;
  for (let i = 0; i < playlistIds.length; i++) {
    if (!playlistIds[i] || playlistIds[i] === "LM" || playlistIds[i].startsWith("VL")) continue;
    const c = counts?.[i] ?? 0;
    if (!best || c > best.count) best = { id: playlistIds[i], count: c };
  }
  if (!best && playlistIds.length) best = { id: playlistIds.find(Boolean), count: null };
  if (!best?.id) return {
    measured: null,
    check: check("G4.playlistPaging", "playlist tracks page 1 and page 2 (when present): no boundary duplicates", {
      measured: null, blocked: "no library playlist was read",
    }),
  };
  const start = await app.snapshot();
  const accountScope = sha256Short(start?.authState?.accountScope);
  const since = (await app.state()).seq;
  const opened = await app.click(`[data-testid=${JSON.stringify(`playlist-card-${best.id}`)}]`);
  const playlistId = sha256Short(best.id);
  const first = await catalogPage(app, { since, kind: "playlistTracks", playlistId, timeoutMs: 30_000 });
  const p1 = first.page;
  const second = p1?.nextCursorId ? await nextCatalogPage(app, first, {
    kind: "playlistTracks", playlistId, tail: PLAYLIST_TAIL, timeoutMs: 30_000,
  }) : null;
  const p2 = second?.page ?? null;
  const ids1 = p1?.ids ?? [];
  const ids2 = p2?.ids ?? [];
  const expectedCount = ids1.length + ids2.length;
  const uiLoadedCount = (await app.waitFor(() => app.evalMain(`(() => {
    const list = document.querySelector('[data-testid="view-playlist"] [data-testid="virtual-track-list"]');
    const rowHeight = list?.querySelector('[data-testid="virtual-track-row"]')?.offsetHeight ?? 0;
    const count = rowHeight > 0 ? Math.round(list.offsetHeight / rowHeight) : 0;
    return count > 0 && count >= ${expectedCount} ? count : null;
  })()`), { timeoutMs: 6_000 })).value;
  const end = await app.snapshot();
  const sameAccount = Boolean(start?.accountEpoch && end?.accountEpoch === start.accountEpoch && end.authState?.playerCatalogMatched);
  const boundaryDupes = dupes(ids1.slice(-20), ids2.slice(0, 20));
  const measured = {
    playlist: sha256Short(best.id),
    declaredTrackCount: best.count,
    opened,
    page1: p1 ? { itemCount: p1.itemCount, hasNextCursor: p1.hasNextCursor, freshness: p1.freshness } : null,
    page2: p2 ? { itemCount: p2.itemCount, hasNextCursor: p2.hasNextCursor } : null,
    cursorRequested: second?.request?.query?.cursor === true,
    scrolledToTail: second?.scrolledToTail ?? null,
    boundaryDuplicates: boundaryDupes,
    totalDuplicatesAcrossPages: dupes(ids1, ids2),
    uiLoadedCount,
    sameAccount,
  };
  return {
    measured,
    check: check("G4.playlistPaging", "playlist tracks page 1 and page 2 (when present): no boundary duplicates", {
      measured,
      pass: Boolean(opened.clicked && sameAccount && uiLoadedCount && validFreshPage(first, "playlistTracks", accountScope, playlistId) && p1.itemCount > 0 &&
        (!p1.hasNextCursor || (second && validFreshPage(second, "playlistTracks", accountScope, playlistId) && p2.itemCount > 0 && boundaryDupes === 0))),
      note: p1?.hasNextCursor ? null : "chosen playlist fits in one page",
    }),
  };
}

export async function accountConsistencyCheck(app) {
  const snap = await app.snapshot();
  const evs = await app.events(0);
  const scopes = [...new Set(evs.filter((e) => e.catalog?.accountScope).map((e) => e.catalog.accountScope))];
  const providers = [...new Set(evs.filter((e) => e.catalog?.providerVersion).map((e) => e.catalog.providerVersion))];
  const authScope = sha256Short(snap?.authState?.accountScope ?? null);
  const caps = snap?.capabilities ?? {};
  return [
    check("G4.accountConsistency", "every catalog response is for the current account scope and player/catalog identity is matched", {
      measured: { authScope, responseScopes: scopes, playerCatalogMatched: snap?.authState?.playerCatalogMatched ?? null },
      pass: scopes.length > 0 && scopes.every((s) => s === authScope) && snap?.authState?.playerCatalogMatched === true,
    }),
    check("G4.writesGatedWhenUnmatched", "write capabilities are not 'supported' unless identity is matched", {
      measured: { playerCatalogMatched: snap?.authState?.playerCatalogMatched ?? null, likeMutation: caps.likeMutation, playlistAddMutation: caps.playlistAddMutation },
      pass: snap?.authState?.playerCatalogMatched === true || (caps.likeMutation !== "supported" && caps.playlistAddMutation !== "supported"),
    }),
    check("G4.providerVersion", "provider and locked version recorded", {
      measured: { providers, capabilities: caps },
      pass: providers.length > 0,
      support: true,
    }),
  ];
}

async function readLibraryAfterUiReload(app) {
  const snapCount = (await app.state()).snapshotCount;
  await app.call("winop", { target: "main", op: "reload" });
  const subscribed = await app.waitFor((s, st) => st.snapshotCount > snapCount, { timeoutMs: 15_000 });
  const since = (await app.state()).seq;
  const nav = await navigate(app, "library");
  const exchange = await catalogPage(app, { since, kind: "libraryPlaylists" });
  return { subscribed: subscribed.ok, nav, since, exchange };
}

async function browseLibraryAfterUiReload(app) {
  const start = await app.snapshot();
  const { subscribed, nav, since, exchange: { request, reply, page: resp } } = await readLibraryAfterUiReload(app);
  const sentQ = (await app.events(since, ["ui->host"])).filter((e) => e.kind === "ui.catalogQuery").map((e) => e.query?.kind);
  const tilesForReply = resp?.ids?.length && resp.ids.every(Boolean) ? await libraryTileCount(app, resp.ids) : null;
  const dom = await app.evalMain(`({
    refreshFailedToast: Boolean(document.querySelector('[data-testid="toast-refresh-failed"]')),
    offlineState: Boolean(document.querySelector('[data-testid="library-state-offline"]')),
    errorState: Boolean(document.querySelector('[data-testid="library-state-error"]')),
    tiles: document.querySelectorAll('[data-testid="view-library"] [data-testid^="playlist-card-"]').length,
    offlineToast: Boolean(document.querySelector('[data-testid="toast-offline"]')),
    bridgeToast: Boolean(document.querySelector('[data-testid="toast-bridge-disconnected"]')),
    playDisabled: document.querySelector('[data-testid="btn-play-pause"]')?.disabled ?? null
  })`);
  const snapshot = await app.snapshot();
  const sameAccount = Boolean(start?.accountEpoch && snapshot?.accountEpoch === start.accountEpoch &&
    snapshot.authState?.playerCatalogMatched && sha256Short(snapshot.authState.accountScope) === resp?.accountScope);
  return {
    subscribed, nav, queriesSent: sentQ, requestAck: request?.ack ?? null,
    response: resp ? {
      resultType: resp.resultType, freshness: resp.freshness, itemCount: resp.itemCount, hasNextCursor: resp.hasNextCursor, error: resp.error,
    } : null,
    coreError: reply?.kind === "core.error" ? reply.error?.code ?? null : null,
    sameAccount, networkState: snapshot?.networkState ?? null, tilesForReply, dom,
  };
}

/** Runs inside a whole-app network cut: reload the UI and browse the library from cache. */
export function offlineBrowseProbe(app) {
  return () => browseLibraryAfterUiReload(app);
}

export function offlineBrowseCheck(netCut) {
  const d = netCut?.during ?? null;
  return check("G4.offlineCachedBrowse", "offline reload renders same-account cached library tiles and shows the offline toast", {
    measured: d,
    pass: Boolean(d?.subscribed && d.nav.clicked && d.requestAck?.ok && d.sameAccount &&
      d.response?.resultType === "libraryPlaylists" && d.response.freshness === "stale" && d.response.itemCount > 0 &&
      d.tilesForReply?.matches === d.response.itemCount && (d.response.hasNextCursor || d.tilesForReply.total === d.response.itemCount) &&
      d.networkState === "offline" &&
      d.dom.tiles > 0 && d.dom.offlineToast && !d.dom.offlineState && !d.dom.errorState),
    blocked: netCut ? (netCut.ok === false ? netCut.error : null) : "network cut did not run",
  });
}

/**
 * Failure isolation (separate launch with ytm-daemon in its own scope): cut only the
 * daemon's egress -> catalog fails while the page keeps playing; then crash the player
 * renderer -> bridge down while catalog browsing still works.
 */
export async function failureIsolationChecks(app, { query }) {
  const daemon = describeProcesses(app.pids()).find((p) => p.role === "core:ytm-daemon");
  const daemonCg = daemon ? cgroupOf(daemon.pid) : null;
  const separated = Boolean(daemonCg && daemonCg.includes("ytmcp-qa-daemon") && describeProcesses(app.pids()).filter((p) => p.cgroup === daemonCg).length === 1);
  const out = { daemonOwnScope: separated };
  out.ensurePlaying = await ensurePlaying(app);
  const library = await browseLibraryAfterUiReload(app);
  out.libraryBeforeFailures = library;
  if (separated) {
    const cut = await cutNetwork([daemonCg]);
    if (cut.ok) {
      try {
        await sleep(3000);
        const p0 = await pageMedia(app);
        const { response: resp } = await uiSearch(app, `${query} live`, 45_000);
        await sleep(1000);
        const dom = await app.evalMain(`({
          error: Boolean(document.querySelector('[data-testid="search-state-error"]')),
          offline: Boolean(document.querySelector('[data-testid="search-state-offline"]')),
          loading: Boolean(document.querySelector('[data-testid="search-state-loading"]'))
        })`);
        const p1 = await pageMedia(app);
        const audio = await sinkInputsFor(app.pids());
        out.catalogCut = {
          response: resp ? { resultType: resp.resultType, error: resp.error, freshness: resp.freshness, itemCount: resp.itemCount } : null,
          dom,
          pagePlayedSec: p0.videoId === p1.videoId ? Math.round((p1.t - p0.t) * 10) / 10 : "track changed",
          pagePaused: p1.paused,
          streamOnNullSink: audio.entries.some((e) => e.sink === NULL_SINK && !e.corked),
        };
      } finally {
        out.catalogCut = { ...(out.catalogCut ?? {}), restored: await restoreNetwork(cut) };
      }
    } else out.catalogCut = { error: cut.error };
  }
  // bridge failure: crash the player renderer, then browse the library through the catalog path
  const since = (await app.state()).seq;
  const crashRequestedAt = Date.now();
  await app.call("winop", { target: "player", op: "crashRenderer" });
  const gone = await app.waitForEvent((e) => e.type === "renderer-gone" && e.role === "player", { since, timeoutMs: 20_000 });
  const noticed = gone && await app.waitForEvent((e) => e.deltaType === "playerChanged" && e.playerConnection === "disconnected", { since: gone.seq, timeoutMs: 20_000 });
  const afterCrash = await app.snapshot();
  const afterCrashLibrary = await browseLibraryAfterUiReload(app);
  const { dom, response: libResp } = afterCrashLibrary;
  const audio = await sinkInputsFor(app.pids());
  // recovery through the UI's "open official player" action
  const beforeRecovery = await app.state();
  const recoveryClickedAt = Date.now();
  const clicked = beforeRecovery.snapshot?.player?.connection === "disconnected"
    ? await app.click(BRIDGE_ACTION) : { clicked: false, reason: "the core no longer reports a disconnected bridge" };
  const recoveryRequest = clicked.clicked ? await app.waitForEvent((e) => e.kind === "ui.openPlayerWindow", {
    since: beforeRecovery.seq, types: ["ui->host"], timeoutMs: 5_000,
  }) : null;
  const ready = recoveryRequest?.ack?.ok ? await app.waitForEvent((e) =>
    e.kind === "bridge.ready" && e.ack === true && e.at >= recoveryClickedAt && Boolean(e.bridgeEpoch),
  { since: beforeRecovery.seq, types: ["bridge->host"], timeoutMs: 30_000 }) : null;
  const rec = ready ? await app.waitFor((s) =>
    s?.player?.bridgeEpoch === ready.bridgeEpoch && s.player.connection === "ready",
  { timeoutMs: 6_000 }) : { ok: false };
  const recoveredViaUi = Boolean(clicked.clicked && recoveryRequest?.ack?.ok === true && rec.ok);
  let harnessReload = null;
  if (!rec.ok) {
    await app.call("winop", { target: "player", op: "reload" });
    harnessReload = (await app.waitFor((s, st) => st.lastBridgeReady && st.lastBridgeReady.at > Date.now() - 40_000 && s?.player?.connection === "ready", { timeoutMs: 40_000 })).ok;
  }
  await app.call("winop", { target: "player", op: "hide" }).catch(() => {});
  out.bridgeCrash = {
    rendererGone: gone?.reason ?? null,
    rendererGoneAfterRequestMs: gone ? gone.t - crashRequestedAt : null,
    connectionAfterCrash: afterCrash?.player?.connection ?? null,
    coreNoticedCrashMs: noticed ? noticed.t - gone.t : null,
    networkStateAfterCrash: afterCrash?.networkState ?? null,
    libraryQueriesSent: afterCrashLibrary.queriesSent,
    libraryQueryAck: afterCrashLibrary.requestAck,
    libraryCoreError: afterCrashLibrary.coreError,
    playbackAfterCrash: afterCrash?.player?.playback ?? null,
    libraryResponse: libResp,
    dom,
    orphanStream: audio.entries.some((e) => !e.corked),
    sinkInputsAfterCrash: audio.entries.map((e) => ({ sink: e.sink, corked: e.corked, role: describeProcesses([e.processId])[0]?.role ?? null })),
    uiRecoveryClicked: clicked,
    recoveryActionAccepted: recoveryRequest?.ack?.ok ?? null,
    recoveredViaUi,
    recoveredViaHarnessReload: harnessReload,
  };
  const cc = out.catalogCut;
  return {
    measured: out,
    checks: [
      check("G4.catalogFailurePlaybackContinues", "catalog (daemon egress) cut: query fails visibly while the page keeps playing on the null sink", {
        measured: cc ?? null,
        pass: Boolean(cc && (cc.response?.error || cc.dom?.error || cc.dom?.offline) && typeof cc.pagePlayedSec === "number" && cc.pagePlayedSec > 5 && !cc.pagePaused && cc.streamOnNullSink),
        blocked: separated ? (cc?.error ?? null) : "ytm-daemon not in its own scope; cannot cut catalog egress alone",
      }),
      check("G4.bridgeFailureBrowseWorks", "player renderer crashed: bridge shown as disconnected, library browsing still works, no orphan audio", {
        measured: out.bridgeCrash,
        pass: Boolean(library.sameAccount && library.response?.freshness === "fresh" && library.dom.tiles > 0 &&
          noticed && afterCrashLibrary.sameAccount && afterCrashLibrary.requestAck?.ok && dom.bridgeToast && dom.playDisabled &&
          dom.tiles > 0 && !out.bridgeCrash.orphanStream && libResp?.resultType === "libraryPlaylists" && !libResp.error),
      }),
      check("G4.bridgeRecoveryViaUi", "the disconnected toast's Open player action recovers the bridge", {
        measured: { recoveredViaUi, recoveryActionAccepted: out.bridgeCrash.recoveryActionAccepted, recoveredViaHarnessReload: harnessReload },
        pass: recoveredViaUi,
      }),
    ],
  };
}
