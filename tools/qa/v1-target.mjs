#!/usr/bin/env node
// Run after Stage B. Uses the installed resources and the byte-identical Electron
// through the existing QA entry. Drives read-only views and a public playback sample.
// YTM_GLASS_ALLOW_LIVE=1 node tools/qa/v1-target.mjs
import { join } from "node:path";
import { AppInstance, ELECTRON, REPO, waitAppReady } from "./lib/app.mjs";
import { check, gateDoc, writeEvidence } from "./lib/evidence.mjs";
import { ensureNullSink, fileSha256, NULL_SINK, run, sha256Short, sinkInputsFor, unloadNullSink } from "./lib/sys.mjs";
import { navigate, pageMedia, sinceSeq } from "./gates/common.mjs";
import { quitViaMpris } from "./gates/g2.mjs";

if (process.env.YTM_GLASS_ALLOW_LIVE !== "1") {
  console.error("Refusing live v1 checks without YTM_GLASS_ALLOW_LIVE=1");
  process.exit(2);
}

const RESOURCES = "/opt/ytm-glass/resources";
const INSTALLED_ELECTRON = "/opt/ytm-glass/ytm-glass";
const MAIN = join(RESOURCES, "app/dist/main.mjs");
const DAEMON = join(RESOURCES, "bin/ytm-daemon");
const workDir = join(process.env.XDG_RUNTIME_DIR ?? "/tmp", "ytm-glass-qa", `v1-${Date.now()}`);
const SEARCH_VIEW = '[data-testid="view-search"]';
const SONGS = `${SEARCH_VIEW} button[data-row-index]:not([aria-disabled="true"])`;
const ROWS = '[data-testid="virtual-track-row"]';
const PUBLIC_TRACK = "Queen Bohemian Rhapsody";
const ALTERNATE_LYRIC_TRACK = "Adele Someone Like You";
const ARTIST_LINK = '[data-testid="album-artist-link"]';
const SEARCH_PLAYLIST = `${SEARCH_VIEW} button[data-testid^="search-playlist-"]`;
const PLAYLIST_ROW = '[data-testid="view-playlist"] button[data-row-index]:not([aria-disabled="true"])';

async function catalogExchange(app, since, kind, initiated, query = null) {
  const request = initiated ? await app.waitForEvent((e) => e.kind === "ui.catalogQuery" && e.query?.kind === kind &&
    (query === null || e.query.q === query), {
    since, types: ["ui->host"], timeoutMs: 8_000,
  }) : null;
  const reply = request?.requestId ? await app.waitForEvent((e) => e.requestId === request.requestId &&
    ["core.catalogResponse", "core.error"].includes(e.kind), {
    since, types: ["core->ui"], timeoutMs: 25_000,
  }) : null;

  return {
    request,
    reply,
    evidence: {
      querySent: Boolean(request?.requestId),
      queryAccepted: request?.ack?.ok === true,
      resultType: reply?.catalog?.resultType ?? null,
      errorCode: reply?.error?.code ?? reply?.catalog?.error?.code ?? null,
      freshness: reply?.catalog?.freshness ?? null,
      itemCount: reply?.catalog?.itemCount ?? null,
    },
  };
}

async function publicSearch(app, query) {
  await navigate(app, "search");
  const since = await sinceSeq(app);
  const input = await app.setInput('[data-testid="search-input"]', query, { change: false });
  const { evidence } = await catalogExchange(app, since, "searchSongs", input.set, query);
  const settled = await app.waitFor(() => app.evalMain(`(() => {
    const view = document.querySelector('[data-testid="view-search"]');
    if (!view || view.querySelector('[aria-busy="true"]')) return null;
    const rows = view.querySelectorAll('[data-testid="virtual-track-row"]').length;
    const albumTiles = view.querySelectorAll('button[data-testid^="search-album-"]').length;
    const artistTiles = view.querySelectorAll('button[data-testid^="search-artist-"]').length;
    const playlistTiles = view.querySelectorAll('button[data-testid^="search-playlist-"]').length;
    const settledState = view.querySelector('[data-testid$="-state-empty"], [data-testid$="-state-error"], [data-testid$="-state-unsupported"]');
    return rows || albumTiles || artistTiles || playlistTiles || settledState ? { rows, albumTiles, artistTiles, playlistTiles } : null;
  })()`), { timeoutMs: 6_000 });
  return { ...evidence, inputSet: input.set === true, render: settled.value ?? null };
}

async function catalogView(app, view, queryKind, open) {
  const since = await sinceSeq(app);
  const clicked = (await open()).clicked === true;
  const { evidence, reply } = await catalogExchange(app, since, queryKind, clicked);
  const body = await app.waitFor(() => app.evalMain(`(() => {
    const view = document.querySelector('[data-testid="view-${view}"]');
    if (!view) return null;
    const renderedRows = view.querySelectorAll(${JSON.stringify(ROWS)}).length;
    const relatedAlbums = view.querySelectorAll('[data-testid^="artist-album-"]').length;
    const collectionTiles = view.querySelectorAll('[data-testid^="playlist-card-"]').length;
    const empty = Boolean(view.querySelector('[data-testid="${view}-state-empty"]'));
    const errorVisible = Boolean(view.querySelector('[data-testid$="-state-error"], [data-testid$="-state-unsupported"], [data-testid$="-state-auth-expired"]'));
    return renderedRows || relatedAlbums || collectionTiles || empty || errorVisible ? { renderedRows, relatedAlbums, collectionTiles, empty, errorVisible } : null;
  })()`), { timeoutMs: 6_000 });
  return { evidence: { clicked, ...evidence, render: body.value ?? null }, reply };
}

async function rowDetailAction(app, view, action) {
  const rows = await app.evalMain(`Array.from(document.querySelectorAll('[data-testid="view-${view}"] [data-row-index]'))
    .slice(0, 6).map((button) => Number(button.dataset.rowIndex))`);
  for (const index of rows) {
    const opened = await app.click(`[data-testid="view-${view}"] ${ROWS}:has([data-row-index="${index}"]) [data-testid="track-actions-btn"]`);
    if (!opened.clicked) continue;
    const dialog = await app.waitFor(() => app.exists('[data-testid="accessible-dialog"]'), { timeoutMs: 2_000 });
    if (!dialog.ok) continue;
    if (await app.exists(action)) return action;
    await app.click('[data-testid="dialog-close-btn"]');
  }
  return null;
}

async function detailCheck(app, view, search) {
  const kind = `${view}Detail`;
  let selector = view === "artist" && await app.exists(ARTIST_LINK) ? ARTIST_LINK : null;
  let entryPoint = selector ? "album-header" : null;
  if (!selector) {
    await navigate(app, "search");
    const tile = `${SEARCH_VIEW} button[data-testid^="search-${view}-"]`;
    if (search.resultType === "searchSongs" && await app.exists(tile)) {
      selector = tile;
      entryPoint = "search-tile";
    } else if (view === "album" && search.render?.rows > 0) {
      selector = await rowDetailAction(app, "search", '[data-testid="action-go-album"]');
      if (selector) entryPoint = "search-track-action";
    }
  }
  if (!selector && view === "artist") {
    // Filtered song search exposes artist names only. Liked track metadata can carry IDs.
    await navigate(app, "liked");
    const rows = await app.waitFor(() => app.exists(`[data-testid="view-liked"] ${ROWS}`), { timeoutMs: 8_000 });
    if (rows.ok) selector = await rowDetailAction(app, "liked", '[data-testid^="action-go-artist-"]');
    if (selector) entryPoint = "liked-track-action";
  }
  const measured = { search, entryPoint };
  if (!selector) {
    return check(`V1.${kind}`, `a supported UI entry opens populated ${view} details`, {
      measured, pass: false, blocked: "No usable catalog detail link was available from the tested views.",
    });
  }
  const { evidence } = await catalogView(app, view, kind, () => app.click(selector));
  const populated = (evidence.render?.renderedRows ?? 0) + (evidence.render?.relatedAlbums ?? 0) > 0;
  return check(`V1.${kind}`, `a supported UI entry opens populated ${view} details`, {
    measured: { ...measured, ...evidence },
    pass: evidence.queryAccepted && evidence.resultType === kind && evidence.freshness !== "stale" && populated,
  });
}

async function likedCheck(app) {
  const { evidence, reply } = await catalogView(app, "liked", "likedSongs", () => navigate(app, "liked"));
  const snapshot = await app.snapshot();
  const accountScope = sha256Short(snapshot?.authState?.accountScope ?? null);
  const accountMatched = Boolean(accountScope && accountScope === reply?.catalog?.accountScope &&
    snapshot.authState.playerCatalogMatched === true);
  const count = evidence.itemCount;
  const consistentRender = Number.isInteger(count) &&
    (count > 0 ? evidence.render?.renderedRows > 0 : evidence.render?.empty === true);
  return check("V1.likedSongs", "current-account liked songs returns a fresh typed result and the UI reflects its populated or empty state", {
    measured: { ...evidence, accountMatched, hasNextCursor: reply?.catalog?.hasNextCursor ?? null },
    pass: evidence.queryAccepted && evidence.resultType === "likedSongs" &&
      evidence.freshness === "fresh" && accountMatched && consistentRender,
  });
}

async function likedContinuationCheck(app, firstPage) {
  const control = '[data-testid="liked-load-more"]';
  const snapshot = await app.snapshot();
  const accountScope = sha256Short(snapshot?.authState?.accountScope ?? null);
  const pages = [];
  let hasNext = firstPage.measured.hasNextCursor === true;
  for (let page = 2; hasNext && page <= 5; page++) {
    const ready = await app.waitFor(() => app.evalMain(`Boolean(document.querySelector(${JSON.stringify(control + ":enabled")}))`),
      { timeoutMs: 5_000 });
    const since = await sinceSeq(app);
    const clicked = ready.ok && (await app.click(control)).clicked === true;
    const { request, reply, evidence } = await catalogExchange(app, since, "likedSongs", clicked);
    const current = await app.snapshot();
    const accountMatched = Boolean(accountScope && current?.accountEpoch === snapshot?.accountEpoch &&
      current?.authState?.playerCatalogMatched === true && reply?.catalog?.accountScope === accountScope);
    hasNext = reply?.catalog?.hasNextCursor ?? null;
    const cursorRequested = request?.query?.cursor === true;
    const pass = cursorRequested && evidence.queryAccepted && evidence.resultType === "likedSongs" &&
      evidence.freshness === "fresh" && accountMatched && Number.isInteger(evidence.itemCount) && typeof hasNext === "boolean";
    pages.push({ page, ...evidence, cursorRequested, accountMatched, hasNext, pass });
    if (!pass) break;
  }
  return check("V1.likedContinuation", "read up to five fresh liked songs pages from the same account", {
    measured: { pages },
    pass: pages.length > 0 && pages.every((page) => page.pass),
    notRun: pages.length ? null : "The tested liked songs response had no continuation.",
  });
}

async function queueCheck(app, track) {
  await navigate(app, "queue");
  const projected = await app.waitFor((snapshot) => {
    const queue = snapshot?.queueSummary;
    const entries = queue?.previewEntries ?? [];
    const current = entries.find((entry) => entry.entryId === queue.currentEntryId);
    if (current?.videoId !== track) return null;
    return {
      currentMatches: true,
      previewCount: entries.length,
      uniqueEntryIds: new Set(entries.map((entry) => entry.entryId)).size === entries.length,
      loadedCount: queue.loadedCount,
      revision: queue.queueRevision,
      completeness: queue.completeness,
      hasMore: queue.hasMore ?? null,
    };
  }, { timeoutMs: 12_000 });
  const rendered = await app.waitFor(() => app.evalMain(`(() => {
    const view = document.querySelector('[data-testid="view-queue"]');
    if (!view) return null;
    const rows = view.querySelectorAll('[data-testid="virtual-track-row"]').length;
    return rows ? { rows, markedCurrent: view.querySelectorAll('button[aria-current="true"]').length } : null;
  })()`), { timeoutMs: 6_000 });
  const queue = projected.value;
  return check("V1.queueMirror", "real playback produces a core queue projection with distinct entries and a rendered current row", {
    measured: { projection: queue, render: rendered.value },
    pass: Boolean(queue?.uniqueEntryIds && queue.loadedCount >= queue.previewCount &&
      queue.revision > 0 && queue.completeness && rendered.value?.markedCurrent === 1),
    note: "The duplicate-video and queue edit cases are outside this sample.",
  });
}

async function lyricsChecks(app, track, since) {
  await navigate(app, "lyrics");
  const trackChanged = await app.waitForEvent((e) => e.deltaType === "playerChanged" && e.videoId === track, {
    since, types: ["core->ui"], timeoutMs: 5_000,
  });
  const request = trackChanged ? await app.waitForEvent((e) => e.kind === "ui.lyricsQuery", {
    since: trackChanged.seq, types: ["ui->host"], timeoutMs: 5_000,
  }) : null;
  const reply = request?.requestId ? await app.waitForEvent((e) => e.requestId === request.requestId && e.kind === "core.lyricsResponse", {
    since, types: ["core->ui"], timeoutMs: 20_000,
  }) : null;
  const view = await app.waitFor(() => app.evalMain(`(() => {
    const root = document.querySelector('[data-testid="view-lyrics"]');
    const read = (id) => root?.querySelector('[data-testid="' + id + '"]');
    if (!root || root.querySelector('[aria-busy="true"]')) return null;
    const kind = {
      "Synced lyrics": "synced",
      "Plain lyrics": "plain",
      "Instrumental, no lyrics": "instrumental",
      "No matching lyrics found": "notFound",
      "The lyrics service is unavailable": "unavailable",
      "Lyrics matching is turned off in Settings": "disabled",
    }[read("lyrics-kind-badge")?.textContent?.trim()] ?? null;
    const source = read("lyrics-source")?.textContent?.trim() ?? "";
    return {
      kind,
      providerSource: /lrclib/i.test(source),
      disabledSource: /disabled/i.test(source),
      seekableLineCount: root.querySelectorAll('[data-testid="lyrics-seek-line"]:enabled').length,
      plainLineCount: read("lyrics-plain-text")?.querySelectorAll("p").length ?? 0,
      reasonVisible: Boolean(read("lyrics-status-empty")?.textContent?.trim()),
    };
  })()`), { timeoutMs: 6_000 });
  const snapshot = await app.snapshot();
  const currentMatches = snapshot?.player?.currentTrack?.videoId === track;
  const measured = { ...view.value, responseObserved: Boolean(reply), providerResult: reply?.lyrics ?? null, currentMatches };
  const synced = measured.kind === "synced" && measured.seekableLineCount > 0;
  const plain = measured.kind === "plain" && measured.plainLineCount > 0 && measured.seekableLineCount === 0;
  const fallback = ["instrumental", "notFound", "unavailable", "disabled"].includes(measured.kind) &&
    measured.reasonVisible && measured.seekableLineCount === 0;
  const validSource = measured.kind === "disabled" ? measured.disabledSource : measured.providerSource;
  return [
    check("V1.lyricsView", "live lyrics displays text or a typed fallback reason for the current public track", {
      measured, pass: Boolean(reply && currentMatches && validSource && (synced || plain || fallback)),
    }),
    check("V1.lyricsContent", "live provider returns actual lyrics content for the public sample", {
      measured,
      pass: Boolean(reply && currentMatches && measured.providerSource && (synced || plain)),
      blocked: measured.kind === "disabled" ? "Current preferences disable lyrics matching." : null,
      notRun: fallback && measured.kind !== "disabled" ? "The provider did not return lyric text for this sample." : null,
    }),
  ];
}

async function playbackObserved(app, track) {
  if (!track) return false;
  const observation = await app.waitFor(async (snapshot) => {
    if (snapshot?.player?.currentTrack?.videoId !== track) return false;
    const [player, audio] = await Promise.all([pageMedia(app), sinkInputsFor(app.pids())]);
    return player.videoId === track && player.paused === false &&
      audio.entries.some((entry) => entry.sink === NULL_SINK && !entry.corked);
  }, { timeoutMs: 12_000 });
  return observation.ok;
}

async function playlistContext(app, playlist, title, catalogRows, rowIndex) {
  const expected = catalogRows.slice(rowIndex, rowIndex + 4);
  const observe = `(() => {
    const playlist = ${JSON.stringify(playlist)};
    const title = ${JSON.stringify(title)};
    const expected = ${JSON.stringify(expected)};
    const normalizeId = (value) => typeof value === "string" ? value.trim().replace(/^VL/, "") : null;
    const samePlaylist = (value) => normalizeId(value) === normalizeId(playlist);
    const tidyTitle = (value) => String(value ?? "").replace(/\\s+/g, " ").trim();
    const urlList = new URLSearchParams(location.search).get("list");
    const queue = document.querySelector("ytmusic-player-bar")?.getState?.()?.queue;
    const items = Array.isArray(queue?.items) ? queue.items : [];
    const selectedIndex = Number.isInteger(queue?.selectedItemIndex) ? queue.selectedItemIndex : -1;
    const item = (index) => items[index]?.playlistPanelVideoRenderer ??
      items[index]?.playlistPanelVideoWrapperRenderer?.primaryRenderer?.playlistPanelVideoRenderer;
    const selected = item(selectedIndex);
    const queuePlaylistId = selected?.navigationEndpoint?.watchEndpoint?.playlistId ?? queue?.playlistId;
    const titleInQueue = document.querySelector("ytmusic-player-queue #queue-header #title, ytmusic-player-queue ytmusic-queue-header-renderer #title")?.textContent;
    const pageTitle = document.title.replace(/\\s+[-–|]\\s+YouTube Music$/, "");
    let orderedCatalogItems = 0;
    if (selectedIndex >= 0) for (const [offset, videoId] of expected.entries()) {
      if (!videoId || item(selectedIndex + offset)?.videoId !== videoId) break;
      orderedCatalogItems++;
    }
    return {
      urlListPresent: Boolean(urlList), urlListMatches: samePlaylist(urlList),
      urlListUsesBrowsePrefix: urlList?.startsWith("VL") ?? false,
      officialTitleMatches: Boolean(title) && [pageTitle, titleInQueue].some((label) => tidyTitle(label) === tidyTitle(title)),
      officialQueueLoaded: items.length,
      officialQueueSelectedMatches: Boolean(expected[0]) && selected?.videoId === expected[0],
      officialQueuePlaylistPresent: Boolean(queuePlaylistId),
      officialQueuePlaylistMatches: samePlaylist(queuePlaylistId),
      orderedCatalogItems,
      distinctOrderedCatalogItems: new Set(expected.slice(0, orderedCatalogItems)).size,
    };
  })()`;
  let latest = null;
  await app.waitFor(async (snapshot) => {
    const page = await app.evalPlayer(observe);
    if (!page) return false;
    const queue = snapshot?.queueSummary;
    const projectedCurrent = queue?.previewEntries?.find((entry) => entry.entryId === queue?.currentEntryId);
    const directContext = page.officialQueuePlaylistMatches ||
      (page.urlListMatches && catalogRows.length === 1);
    const enoughItems = page.officialQueueLoaded >= Math.min(catalogRows.length, 2);
    const requiredDistinctItems = page.urlListMatches ? 2 : 3;
    const orderedContext = !page.officialQueuePlaylistPresent &&
      page.distinctOrderedCatalogItems >= requiredDistinctItems;
    const retained = page.officialQueueSelectedMatches && enoughItems && (directContext || orderedContext);
    latest = {
      ...page, retained,
      projectedQueueLoaded: queue?.loadedCount ?? null,
      projectedQueueSelectedMatches: Boolean(expected[0]) && projectedCurrent?.videoId === expected[0],
    };
    return retained;
  }, { timeoutMs: 8_000 });
  return latest;
}

async function playlistPlaybackCheck(app) {
  const search = await publicSearch(app, "Queen greatest hits playlist");
  let playlist = search.resultType === "searchSongs" && search.render?.playlistTiles > 0 ?
    await app.evalMain(`document.querySelector(${JSON.stringify(SEARCH_PLAYLIST)})?.dataset.testid?.slice("search-playlist-".length) ?? null`) : null;
  let tile = SEARCH_PLAYLIST;
  let entryPoint = playlist ? "search-tile" : null;
  let library = null;
  if (!playlist) {
    const { evidence, reply } = await catalogView(app, "library", "libraryPlaylists", () => navigate(app, "library"));
    library = evidence;
    if (evidence.queryAccepted && evidence.resultType === "libraryPlaylists" && evidence.freshness === "fresh") {
      const ids = reply.catalog.ids ?? [];
      const counts = reply.catalog.counts ?? [];
      let mostTracks = -1;
      for (const [index, id] of ids.entries()) {
        const count = counts[index] ?? 0;
        if (typeof id !== "string" || !/^[A-Za-z0-9_-]+$/.test(id) || count <= mostTracks) continue;
        playlist = id;
        mostTracks = count;
      }
      if (playlist) {
        tile = `[data-testid="view-library"] [data-testid="playlist-card-${playlist}"]`;
        entryPoint = "library-tile";
      }
    }
  }
  const measured = { search, library, entryPoint };
  const available = playlist && (await app.waitFor(() => app.exists(tile), { timeoutMs: 4_000 })).ok;
  if (!available) {
    return check("V1.playlistPlayback", "a real catalog playlist opens and plays its selected row with the original playlist context", {
      measured, pass: false, blocked: "No playable playlist entry was available from the tested views.",
    });
  }
  const { evidence, reply } = await catalogView(app, "playlist", "playlistTracks", () => app.click(tile));
  const rows = evidence.render?.renderedRows;
  const selection = rows ? await app.evalMain(`(() => {
    const row = document.querySelector(${JSON.stringify(PLAYLIST_ROW)});
    return row ? { index: Number(row.dataset.rowIndex), title: document.getElementById("playlist-heading")?.textContent ?? "" } : null;
  })()`) : null;
  const since = await sinceSeq(app);
  const command = selection ? await app.clickCommand(PLAYLIST_ROW, { op: "playTarget" }) : null;
  const dispatch = command?.commandId ?
    (await app.events(since, ["core->bridge"])).find((event) => event.kind === "player.command" && event.commandId === command.commandId) : null;
  const contextSent = { ui: command?.args?.playlistContext ?? null, bridge: dispatch?.playlistContext ?? null };
  const contextForwarded = Boolean(contextSent.ui?.matchesLastCatalogPlaylist && contextSent.ui.hasIndex &&
    contextSent.bridge?.matchesLastCatalogPlaylist && contextSent.bridge.hasIndex);
  const catalogRows = reply?.catalog?.ids ?? [];
  const selectedCatalogRow = Number.isSafeInteger(selection?.index) && Boolean(command?.args?.videoId) &&
    command.args.videoId === catalogRows[selection.index];
  const playback = command?.finalStatus === "observed" && await playbackObserved(app, command.args?.videoId);
  const pageContext = playback && selectedCatalogRow ?
    await playlistContext(app, playlist, selection.title, catalogRows, selection.index) : null;
  const context = contextForwarded && pageContext?.retained === true;
  return check("V1.playlistPlayback", "a real catalog playlist opens and plays its selected row with the original playlist context", {
    measured: { ...measured, ...evidence, playback, receipt: command?.finalStatus ?? null, selectedCatalogRow,
      contextSent, pageContext, playlistContextRetained: context },
    pass: evidence.queryAccepted && evidence.resultType === "playlistTracks" && evidence.freshness === "fresh" &&
      rows > 0 && playback && context,
  });
}

async function playSearchResult(app, search) {
  await navigate(app, "search");
  const available = search.queryAccepted && search.resultType === "searchSongs" && search.render?.rows > 0 &&
    (await app.waitFor(() => app.exists(SONGS), { timeoutMs: 4_000 })).ok;
  const since = await sinceSeq(app);
  const command = available ? await app.clickCommand(SONGS, { op: "playTarget" }) : null;
  const track = command?.args?.videoId;
  const playback = command?.finalStatus === "observed" && await playbackObserved(app, track);
  return { playback, track, since, status: command?.finalStatus ?? null, available: Boolean(available) };
}

async function readPreferences(app) {
  await navigate(app, "settings");
  const settings = (await app.snapshot())?.settings;
  if (!settings) return null;
  const visible = await app.evalMain(`(() => {
    const read = (id) => document.querySelector('[data-testid="' + id + '"]');
    return {
      theme: read("setting-theme-select")?.value,
      uiScalePercent: Number(read("setting-ui-scale")?.value),
      reduceMotion: read("setting-reduce-motion")?.checked,
      reduceTransparency: read("setting-reduce-transparency")?.checked,
      lrclibEnabled: read("setting-lrclib")?.checked,
      autoplayOnLaunch: read("setting-autoplay")?.checked,
    };
  })()`);
  const fields = ["theme", "uiScalePercent", "reduceMotion", "reduceTransparency", "lrclibEnabled", "autoplayOnLaunch"];
  return { values: fields.map((field) => settings[field]), matched: fields.filter((field) => visible[field] === settings[field]).length, total: fields.length };
}

async function stop(app) {
  const quit = await quitViaMpris(app).catch(() => null);
  if (!quit?.exited || quit.leftoverPids.length) {
    await app.kill();
    return false;
  }
  return true;
}

const checks = [];
const hashes = { installedElectron: fileSha256(INSTALLED_ELECTRON), qaElectron: fileSha256(ELECTRON), main: fileSha256(MAIN), daemon: fileSha256(DAEMON) };
const meta = {
  gitCommit: (await run("git", ["rev-parse", "HEAD"])).stdout.trim() || null,
  sessionType: process.env.XDG_SESSION_TYPE ?? null,
  target: "installed resources through the QA entry",
  hashes,
};
const canLaunch = Boolean(hashes.installedElectron && hashes.installedElectron === hashes.qaElectron && hashes.main && hashes.daemon);
checks.push(check("V1.installedResources", "installed resources and byte-identical QA Electron are available", {
  measured: hashes, pass: canLaunch, blocked: canLaunch ? null : "Install the candidate or rebuild the matching QA runtime.",
}));
if (canLaunch) {
  let sink;
  let app;
  const launch = (label) => AppInstance.launch({
    label, workDir, daemonBin: DAEMON,
    extraEnv: {
      YTM_GLASS_QA_MAIN: MAIN,
      YTM_GLASS_UI_DIST_DIR: join(RESOURCES, "ui-dist"),
      YTM_GLASS_BRIDGE_DIST_DIR: join(RESOURCES, "bridge-dist"),
      YTM_GLASS_IDENTITY_PATH: join(RESOURCES, "config/app-identity.json"),
    },
  });
  try {
    sink = await ensureNullSink();
    app = await launch("v1-target");
    const ready = await waitAppReady(app, { timeoutMs: 90_000 });
    const live = ready.ok && ready.value?.mockMode === false;
    checks.push(check("V1.authenticated", "the installed resources are running with a real authenticated player", {
      measured: { ready: ready.ok, mockMode: ready.value?.mockMode ?? null },
      pass: live, blocked: live ? null : "An authenticated, non-mock player is required.",
    }));
    if (live) {
      const liked = await likedCheck(app);
      checks.push(liked, await likedContinuationCheck(app, liked));
      const songSearch = await publicSearch(app, PUBLIC_TRACK);
      checks.push(await detailCheck(app, "album", songSearch));
      checks.push(await detailCheck(app, "artist", songSearch));

      const { playback, track, since, status, available } = await playSearchResult(app, songSearch);
      checks.push(check("V1.publicPlayback", "search playback is observed by the real player and routed to the temporary null sink", {
        measured: { status, available, observedOnTarget: playback },
        pass: playback,
      }));
      if (playback) {
        checks.push(await queueCheck(app, track));
        const first = await lyricsChecks(app, track, since);
        let lyrics = first;
        if (first[1].status !== "PASS_TARGET" && first[1].status !== "BLOCKED") {
          const alternate = await playSearchResult(app, await publicSearch(app, ALTERNATE_LYRIC_TRACK));
          if (alternate.playback) {
            const second = await lyricsChecks(app, alternate.track, alternate.since);
            lyrics = first.map((original, index) => ({
              ...(original.status === "PASS_TARGET" ? original : second[index]),
              measured: { firstSample: original.measured, alternateSample: second[index].measured },
            }));
          } else {
            lyrics = first.map((original) => ({
              ...original, measured: { firstSample: original.measured, alternatePlayback: false },
            }));
          }
        }
        checks.push(...lyrics);
      } else {
        for (const id of ["queueMirror", "lyricsView", "lyricsContent"]) {
          checks.push(check(`V1.${id}`, "inspect live data for the public playback sample", {
            measured: null, pass: false, blocked: "Public playback did not become observable.",
          }));
        }
      }

      checks.push(await playlistPlaybackCheck(app));
      const before = await readPreferences(app);
      const quitClean = await stop(app);
      app = null;
      app = await launch("v1-target-relaunch");
      const relaunched = await waitAppReady(app, { timeoutMs: 60_000 });
      const after = relaunched.ok ? await readPreferences(app) : null;
      const sameSettings = Boolean(before && after && before.values.every((value, index) => Object.is(value, after.values[index])));
      checks.push(check("V1.preferencesColdRead", "existing preferences agree with their controls across a clean restart", {
        measured: { quitClean, sameSettings, matchedBefore: before?.matched ?? null, matchedAfter: after?.matched ?? null, fieldCount: before?.total ?? null },
        pass: quitClean && sameSettings && before.matched === before.total && after.matched === after.total,
        support: true,
        note: "The run leaves preferences unchanged, so this is supporting cold-read evidence.",
      }));
    }
  } catch {
    checks.push(check("V1.probe", "the supplemental QA steps completed without a harness error", { measured: null, pass: false }));
    console.error("A supplemental QA step failed. Completed checks are retained in the evidence file.");
  } finally {
    try {
      if (app) await stop(app);
    } finally {
      if (sink?.created) await unloadNullSink(sink.moduleId);
    }
  }
}

const doc = gateDoc({ gate: "V1-supplemental", name: "v1 on-device browsing, queue and lyrics supplemental checks", run: "TARGET", meta, checks });
console.log(writeEvidence(join(REPO, "artifacts/live/v1-target.json"), doc));
console.log(JSON.stringify(Object.fromEntries(checks.map(({ id, status }) => [id, status]))));
if (doc.result !== "PASS_TARGET") process.exitCode = 1;
