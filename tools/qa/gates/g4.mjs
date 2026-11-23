// G4: search, library playlists (page 1 and 2), playlist track paging, offline cached
// browsing, account consistency, provider version, and failure isolation between the
// catalog path and the playback bridge. Private titles never leave the app process.
import { check } from "../lib/evidence.mjs";
import { cgroupOf, cutNetwork, describeProcesses, restoreNetwork, sha256Short, sinkInputsFor, sleep, NULL_SINK } from "../lib/sys.mjs";
import { ensurePlaying, navigate, pageMedia, uiSearch, waitCatalog, waitSearchResult } from "./common.mjs";

const LOAD_MORE = "Load more";

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

/** Library page 1, then page 2 through the UI's load-more button when the provider has a cursor. */
export async function libraryPagingCheck(app) {
  const since = (await app.state()).seq;
  await navigate(app, "home");
  await navigate(app, "library");
  let p1 = await waitCatalog(app, since, "libraryPlaylists", 25_000);
  if (!p1) {
    // already loaded in this UI session: force a reload through the page's retry path
    await app.call("winop", { target: "main", op: "reload" });
    await sleep(3000);
    const s2 = (await app.state()).seq;
    await navigate(app, "library");
    p1 = await waitCatalog(app, s2, "libraryPlaylists", 25_000);
  }
  const tiles1 = await app.evalMain(`document.querySelectorAll('[data-testid="view-library"] [data-testid^="playlist-card-"]').length`);
  let p2 = null;
  let tiles2 = null;
  let clicked = null;
  let page2Error = null;
  let page2UiError = null;
  if (p1?.hasNextCursor) {
    const s3 = (await app.state()).seq;
    clicked = await app.click('[data-testid="view-library"] button', { textIncludes: LOAD_MORE });
    const ev2 = await app.waitForEvent((e) => e.type === "core->ui" && (e.catalog?.resultType === "libraryPlaylists" || e.kind === "core.error"), { since: s3, timeoutMs: 25_000 });
    p2 = ev2?.catalog ?? null;
    page2Error = ev2?.kind === "core.error" ? ev2.error : null;
    page2UiError = await app.evalMain(`document.querySelector('[data-testid="view-library"] [role=alert]')?.textContent?.trim() ?? null`);
    await sleep(800);
    tiles2 = await app.evalMain(`document.querySelectorAll('[data-testid="view-library"] [data-testid^="playlist-card-"]').length`);
  }
  const ids1 = p1?.ids ?? [];
  const ids2 = p2?.ids ?? [];
  const measured = {
    page1: p1 ? { itemCount: p1.itemCount, hasNextCursor: p1.hasNextCursor, freshness: p1.freshness, providerVersion: p1.providerVersion, accountScope: p1.accountScope } : null,
    page2: p2 ? { itemCount: p2.itemCount, hasNextCursor: p2.hasNextCursor, freshness: p2.freshness } : null,
    loadMoreClicked: clicked,
    page2Error,
    page2UiError,
    duplicatesAcrossPages: dupes(ids1, ids2),
    uiTiles: { afterPage1: tiles1, afterPage2: tiles2 },
  };
  const page2Needed = Boolean(p1?.hasNextCursor);
  return {
    measured,
    ids1,
    check: check("G4.libraryPaging", "library playlists page 1 and (when a cursor exists) page 2 via the UI: no duplicates, UI shows p1+p2", {
      measured,
      // YouTube Music hands out a grid cursor even after the last playlist; that page comes back
      // empty and ends paging, which is a valid page 2.
      pass: Boolean(p1 && p1.itemCount > 0 && (!page2Needed || (p2 && !page2Error && measured.duplicatesAcrossPages === 0 && tiles2 === tiles1 + p2.itemCount && (p2.itemCount > 0 || !p2.hasNextCursor)))),
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
  if (!best) best = { id: playlistIds.find(Boolean), count: null };
  const since = (await app.state()).seq;
  const opened = await app.click(`[data-testid="playlist-card-${best.id}"]`);
  const p1 = await waitCatalog(app, since, "playlistTracks", 30_000);
  await sleep(800);
  const rows1 = await app.evalMain(`document.querySelector('[data-testid="view-playlist"]')?.textContent?.match(/(\\d+) songs loaded/)?.[1] ?? null`);
  let p2 = null;
  if (p1?.hasNextCursor) {
    const s2 = (await app.state()).seq;
    await app.click('[data-testid="playlist-load-more"]');
    p2 = await waitCatalog(app, s2, "playlistTracks", 30_000);
  }
  const ids1 = p1?.ids ?? [];
  const ids2 = p2?.ids ?? [];
  const boundaryDupes = dupes(ids1.slice(-20), ids2.slice(0, 20));
  const measured = {
    playlist: sha256Short(best.id),
    declaredTrackCount: best.count,
    opened,
    page1: p1 ? { itemCount: p1.itemCount, hasNextCursor: p1.hasNextCursor, freshness: p1.freshness } : null,
    page2: p2 ? { itemCount: p2.itemCount, hasNextCursor: p2.hasNextCursor } : null,
    boundaryDuplicates: boundaryDupes,
    totalDuplicatesAcrossPages: dupes(ids1, ids2),
    uiLoadedCountText: rows1,
  };
  return {
    measured,
    check: check("G4.playlistPaging", "playlist tracks page 1 and page 2 (when present): no boundary duplicates", {
      measured,
      pass: Boolean(p1 && p1.itemCount > 0 && (!p1.hasNextCursor || (p2 && p2.itemCount > 0 && boundaryDupes === 0))),
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

async function browseLibraryAfterUiReload(app) {
  const snapCount = (await app.state()).snapshotCount;
  await app.call("winop", { target: "main", op: "reload" });
  const subscribed = await app.waitFor((s, st) => st.snapshotCount > snapCount, { timeoutMs: 15_000 });
  const since = (await app.state()).seq;
  const nav = await navigate(app, "library");
  const request = await app.waitForEvent((e) => e.type === "ui->host" && e.query?.kind === "libraryPlaylists", { since, timeoutMs: 6_000 });
  const reply = request?.requestId ? await app.waitForEvent((e) => e.type === "core->ui" && e.requestId === request.requestId && ["core.catalogResponse", "core.error"].includes(e.kind), { since, timeoutMs: 25_000 }) : null;
  const sentQ = (await app.events(since, ["ui->host"])).filter((e) => e.kind === "ui.catalogQuery").map((e) => e.query?.kind);
  const resp = reply?.catalog ?? null;
  await sleep(800);
  const dom = await app.evalMain(`({
    cachedNotice: Boolean(document.querySelector('[data-testid="library-state-cached"]')),
    offlineState: Boolean(document.querySelector('[data-testid="library-state-offline"]')),
    errorState: Boolean(document.querySelector('[data-testid="library-state-error"]')),
    tiles: document.querySelectorAll('[data-testid="view-library"] [data-testid^="playlist-card-"]').length,
    offlineBanner: Boolean(document.querySelector('[data-testid="banner-offline"]')),
    bridgeBanner: Boolean(document.querySelector('[data-testid="banner-bridge-disconnected"]')),
    playDisabled: document.querySelector('[data-testid="btn-play-pause"]')?.disabled ?? null
  })`);
  return { subscribed: subscribed.ok, nav, queriesSent: sentQ, requestAck: request?.ack ?? null, response: resp ? { resultType: resp.resultType, freshness: resp.freshness, itemCount: resp.itemCount, error: resp.error } : null, coreError: reply?.kind === "core.error" ? reply.error : null, dom };
}

/** Runs inside a whole-app network cut: reload the UI and browse the library from cache. */
export function offlineBrowseProbe(app) {
  return () => browseLibraryAfterUiReload(app);
}

export function offlineBrowseCheck(netCut) {
  const d = netCut?.during ?? null;
  return check("G4.offlineCachedBrowse", "with the app offline, the library is served from cache and marked stale in the UI", {
    measured: d,
    pass: Boolean(d?.response?.freshness === "stale" && d.dom.tiles > 0 && d.dom.cachedNotice),
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
        const since = (await app.state()).seq;
        await navigate(app, "search");
        await app.setInput('[data-testid="search-input"]', `${query} live`, { change: false });
        const resp = await waitSearchResult(app, since, 45_000);
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
  const clicked = await app.click('[data-testid="banner-bridge-disconnected"] button');
  const rec = await app.waitFor((s, st) => st.lastBridgeReady?.at > (gone?.t ?? crashRequestedAt) && s?.player?.connection === "ready", { timeoutMs: 30_000 });
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
    recoveredViaUi: rec.ok,
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
        pass: Boolean(library.response?.freshness === "fresh" && library.dom.tiles > 0 && noticed && dom.bridgeBanner && dom.playDisabled && dom.tiles > 0 && !out.bridgeCrash.orphanStream && libResp?.resultType === "libraryPlaylists" && !libResp.error),
      }),
      check("G4.bridgeRecoveryViaUi", "the banner's 'open official player' action recovers the bridge", {
        measured: { recoveredViaUi: rec.ok, recoveredViaHarnessReload: harnessReload },
        pass: rec.ok,
      }),
    ],
  };
}
