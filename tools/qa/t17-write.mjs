#!/usr/bin/env node
// T17 account-write acceptance on an existing private test playlist named "YTM Glass Test"
// (created on the reference account before the project was renamed; the name is account data).
// YTMCP_ALLOW_LIVE=1 YTMCP_ALLOW_WRITES=1 node tools/qa/t17-write.mjs
// YTMCP_ALLOW_LIVE=1 node tools/qa/t17-write.mjs --probe  # locate the playlist, read-only
import { join } from "node:path";
import { parseArgs } from "node:util";
import { AppInstance, REPO, waitAppReady } from "./lib/app.mjs";
import { check, gateDoc, writeEvidence } from "./lib/evidence.mjs";
import { ensureNullSink, sha256Short, sleep, unloadNullSink } from "./lib/sys.mjs";
import { navigate, uiSearch, waitCatalog } from "./gates/common.mjs";
import { quitViaMpris } from "./gates/g2.mjs";

const TEST_TITLE = "YTM Glass Test";
const { values: opt } = parseArgs({ options: { query: { type: "string", default: "Satie Gymnopedie No.1" }, probe: { type: "boolean", default: false } } });
if (process.env.YTMCP_ALLOW_LIVE !== "1") {
  console.error("Refusing without YTMCP_ALLOW_LIVE=1");
  process.exit(2);
}
if (!opt.probe && process.env.YTMCP_ALLOW_WRITES !== "1") {
  console.error("Refusing account writes without YTMCP_ALLOW_WRITES=1 (final run only)");
  process.exit(2);
}

// The Chinese alternatives in these matchers are YouTube Music's own labels when the page is in Chinese.
const PRIVATE_PLAYLIST = (playlistId) => `(() => {
  if (location.origin !== "https://music.youtube.com" || new URLSearchParams(location.search).get("list") !== ${JSON.stringify(playlistId)}) return false;
  const renderer = document.querySelector("ytmusic-editable-playlist-detail-header-renderer ytmusic-responsive-header-renderer, ytmusic-detail-header-renderer");
  const subtitle = renderer?.data?.subtitle?.runs?.map((run) => run.text)?.join(" • ") ?? renderer?.querySelector("#subtitle")?.textContent ?? "";
  return subtitle.split(/[•·・]/).some((label) => /^(Private|私享|私人|私密|仅限自己|仅自己可见|僅供本人|僅限自己)$/i.test(label.trim()));
})()`;

const REMOVE_ROW = (playlistId, videoId, click = false) => `(() => {
  if (!${PRIVATE_PLAYLIST(playlistId)}) return false;
  const rows = [...document.querySelectorAll("ytmusic-responsive-list-item-renderer")];
  const matches = rows.filter((row) => [...row.querySelectorAll("a[href]")].some((link) => new URL(link.href).searchParams.get("v") === ${JSON.stringify(videoId)}));
  if (matches.length !== 1) return false;
  const menu = matches[0].querySelector("ytmusic-menu-renderer");
  const buttons = [...(menu?.querySelectorAll("button[aria-label], tp-yt-paper-icon-button[aria-label]") ?? [])].filter(
    (button) => /^(Action menu|More actions|操作菜单|更多操作|動作選單|更多動作)$/i.test(button.getAttribute("aria-label")?.trim() ?? "")
  );
  const button = buttons.length === 1 ? buttons[0] : null;
  if (!button) return false;
  if (${click}) button.click();
  return true;
})()`;

const REMOVE_FROM_PLAYLIST = (playlistId, videoId, click = false) => `(() => {
  if (!${REMOVE_ROW(playlistId, videoId)}) return false;
  // The popup data is mounted even when the official player window is hidden.
  // Only the exact command in its popup is eligible; the row also has Like controls.
  const items = [...document.querySelectorAll("ytmusic-menu-popup-renderer ytmusic-menu-service-item-renderer[role=menuitem]")];
  const matches = items.filter((item) =>
    /^(Remove from playlist|从播放列表中移除|从播放列表中删除|从歌单中移除|从歌单中删除|從播放清單中移除)$/i.test(item.textContent.trim()) &&
    item.getAttribute("aria-disabled") !== "true"
  );
  const button = matches.length === 1 ? matches[0] : null;
  if (!button) return false;
  if (${click}) button.click();
  return true;
})()`;

const PLAYER_LIKE_STATUS = (videoId) => `(() => {
  if (location.origin !== "https://music.youtube.com") return null;
  if (document.querySelector("#movie_player")?.getVideoData?.()?.video_id !== ${JSON.stringify(videoId)}) return null;
  const button = document.querySelector("ytmusic-player-bar ytmusic-like-button-renderer");
  const status = button?.getAttribute("like-status") ?? button?.data?.likeStatus;
  return ["INDIFFERENT", "LIKE", "DISLIKE"].includes(status) ? status : null;
})()`;

const REMOVE_OFFICIAL_LIKE = (videoId, click = false) => `(() => {
  if (${PLAYER_LIKE_STATUS(videoId)} !== "LIKE") return false;
  const button = document.querySelector('ytmusic-player-bar ytmusic-like-button-renderer #button-shape-like button[aria-pressed="true"]');
  if (!button || button.disabled) return false;
  if (${click}) button.click();
  return true;
})()`;

async function playerGo(app, url) {
  await app.call("winop", { target: "player", op: "loadURL", args: [url] }, 45_000);
}

async function freshUi(app) {
  const snapCount = (await app.state()).snapshotCount;
  await app.call("winop", { target: "main", op: "reload" });
  return (await app.waitFor((_s, st) => st.snapshotCount > snapCount, { timeoutMs: 20_000 })).ok;
}

// Visit eligible pages via the app's UI. Only a fresh complete read can prove absence.
async function collectFreshPages(app, { open, kind, moreSelector, moreText = null, stopAt = null }) {
  const since = (await app.state()).seq;
  if (!(await open()).clicked) return null;
  let page = await waitCatalog(app, since, kind, 25_000);
  const ids = [];
  // Refuse to infer absence from a non-terminating or incomplete list.
  for (let remaining = 50; remaining > 0 && page?.freshness === "fresh" && Array.isArray(page.ids); remaining--) {
    ids.push(...page.ids);
    if (!page.hasNextCursor || (stopAt && ids.includes(stopAt))) return ids;
    const next = (await app.state()).seq;
    const more = await app.waitFor(async () => (await app.click(moreSelector, { textIncludes: moreText })).clicked, { timeoutMs: 5_000 });
    if (!more.ok) return null;
    page = await waitCatalog(app, next, kind, 25_000);
  }
  return null;
}

async function libraryIds(app) {
  return collectFreshPages(app, {
    open: () => navigate(app, "library"),
    kind: "libraryPlaylists",
    moreSelector: '[data-testid="view-library"] button',
    moreText: "Load more",
  });
}

async function playlistContents(app, playlistId, videoId) {
  if (!await freshUi(app) || !await libraryIds(app)) return null;
  return collectFreshPages(app, {
    open: () => app.click(`[data-testid="playlist-card-${playlistId}"]`),
    kind: "playlistTracks",
    moreSelector: '[data-testid="playlist-load-more"]',
    stopAt: videoId,
  });
}

async function likedContents(app, videoId) {
  if (!await freshUi(app)) return null;
  return collectFreshPages(app, {
    open: () => navigate(app, "liked"),
    kind: "likedSongs",
    moreSelector: '[data-testid="liked-load-more"]',
    stopAt: videoId,
  });
}

async function sameAccount(app, accountEpoch) {
  const snapshot = await app.snapshot().catch(() => null);
  return Boolean(accountEpoch && snapshot?.accountEpoch === accountEpoch && snapshot.authState?.playerCatalogMatched === true);
}

async function readPresence(read, videoId, expected) {
  const sample = async () => (await read().catch(() => null))?.includes(videoId) ?? null;
  const first = await sample();
  if (first === expected) return first;
  await sleep(750);
  return await sample() ?? first;
}

async function mutationVia(app, selectorSteps, accountEpoch, expected) {
  const since = (await app.state()).seq;
  for (const [step, selector] of selectorSteps.entries()) {
    const submits = step === selectorSteps.length - 1;
    // The like action toggles according to the app's current state. Match its label
    // immediately before clicking so a stale row cannot turn an unlike into a like.
    const label = submits && expected.kind === "setLike" ? (expected.liked ? "Like" : "Remove like") : null;
    const action = (click) => app.evalMain(`(() => {
      const matches = document.querySelectorAll(${JSON.stringify(selector)});
      if (matches.length !== 1 || matches[0].disabled) return false;
      const button = matches[0];
      const label = ${JSON.stringify(label)};
      if (label && button.querySelector("span.truncate")?.textContent.trim() !== label) return false;
      if (${click}) {
        button.scrollIntoView({ block: "center" });
        button.click();
      }
      return true;
    })()`);
    const ready = await app.waitFor(() => action(false), { timeoutMs: 5_000 });
    if (!ready.ok || (submits && !await sameAccount(app, accountEpoch))) return { attempted: false, step: step + 1 };
    const clicked = await action(true).catch(() => null);
    // A lost reply to the final click is an attempted write. Resolve it by reading.
    if (clicked === false || (!clicked && !submits)) return { attempted: false, step: step + 1 };
  }
  const sent = await app.waitForEvent((e) => e.type === "ui->host" && e.kind === "ui.mutation", { since, timeoutMs: 5_000 });
  const resp = sent?.requestId ? await app.waitForEvent((e) => e.type === "core->ui" && e.requestId === sent.requestId && e.kind === "core.mutationResponse", { since, timeoutMs: 30_000 }) : null;
  const targetMatches = sent ? Object.entries(expected).every(([key, value]) => sent.mutation?.[key] === value) : null;
  return { attempted: true, targetMatches, ack: sent?.ack ?? null, outcome: resp?.mutation?.outcome ?? null, error: resp?.mutation?.error ?? null };
}

function trackRow(view, videoId, row) {
  return `[data-testid="view-${view}"] [data-testid="virtual-track-row"][data-row-key=${JSON.stringify(`${videoId}::${row}`)}]`;
}

function actionsIn(view, videoId, row) {
  return `${trackRow(view, videoId, row)} [data-testid="track-actions-btn"]`;
}

async function playSearchTrack(app, videoId, row) {
  const command = await app.clickCommand(`${trackRow("search", videoId, row)} button[data-row-index]`, { op: "playTarget" });
  return command.finalStatus === "observed";
}

async function testAdd(app, playlistId, accountEpoch, checks) {
  const search = await uiSearch(app, opt.query);
  const videoId = search.response?.ids?.[0];
  const read = () => playlistContents(app, playlistId, videoId);
  const readBefore = async () => (await read().catch(() => null))?.includes(videoId) ?? null;
  const before = videoId ? await readBefore() ?? await readBefore() : null;
  if (before !== false) {
    checks.push(check("T17.addToPlaylist", "public search result is absent from the test playlist before writing", { measured: { hasTrack: before }, pass: false, blocked: "need a fresh playlist read and a search result not already on the playlist" }));
    return true;
  }
  const freshSearch = await uiSearch(app, opt.query);
  const row = freshSearch.response?.ids?.indexOf(videoId) ?? -1;
  if (row < 0) {
    checks.push(check("T17.addToPlaylist", "public search result is still available to add", { measured: { row }, pass: false }));
    return true;
  }
  const added = await mutationVia(app, [
    actionsIn("search", videoId, row),
    '[data-testid="action-add-to-playlist"]',
    `[data-testid=${JSON.stringify(`pick-playlist-${playlistId}`)}]`,
    '[data-testid="confirm-add-to-playlist"]',
  ], accountEpoch, { kind: "addToPlaylist", videoId, playlistId: sha256Short(playlistId) });
  const afterAdd = added.attempted ? await readPresence(read, videoId, true) : null;
  checks.push(check("T17.addToPlaylist", "OUR UI adds the public track to the test playlist and a fresh read contains it", { measured: { added, afterAdd }, pass: added.outcome === "confirmed" && added.targetMatches && afterAdd === true }));

  if (!added.attempted) {
    checks.push(check("T17.removeRestored", "restore the test playlist after the add", { measured: { added }, notRun: "the add was never submitted" }));
    return true;
  }
  const url = new URL("https://music.youtube.com/playlist");
  url.searchParams.set("list", playlistId);
  // The exact row on the verified official playlist can resolve an unavailable catalog read.
  const landed = added.targetMatches !== false && await playerGo(app, url.href).then(() => true, () => false);
  const officialRow = landed && (await app.waitFor(() => app.evalPlayer(REMOVE_ROW(playlistId, videoId)), { timeoutMs: 10_000 })).ok;
  const menu = officialRow && await app.evalPlayer(REMOVE_ROW(playlistId, videoId, true), { userGesture: true }).catch(() => false);
  const ready = menu && (await app.waitFor(() => app.evalPlayer(REMOVE_FROM_PLAYLIST(playlistId, videoId)), { timeoutMs: 5_000 })).ok;
  const removeAttempted = ready && await sameAccount(app, accountEpoch);
  const remove = removeAttempted ? await app.evalPlayer(REMOVE_FROM_PLAYLIST(playlistId, videoId, true), { userGesture: true }).catch(() => null) : false;
  if (removeAttempted) await sleep(1000);
  const afterRemove = await sameAccount(app, accountEpoch) ? await readPresence(read, videoId, false) : null;
  checks.push(check("T17.removeRestored", "the added track is removed via the official page and a fresh read proves it absent", { measured: { officialRow, removeAttempted, remove, afterRemove }, pass: remove === true && afterRemove === false }));
  return added.targetMatches !== false && afterRemove === false;
}

async function testLike(app, accountEpoch, checks) {
  const search = await uiSearch(app, opt.query);
  const ids = search.response?.ids ?? [];
  let candidate = null;
  for (const videoId of ids.slice(0, 3).filter(Boolean)) {
    const row = ids.indexOf(videoId);
    // Navigation alone may leave the hidden player bar on the previous song.
    if (!await playSearchTrack(app, videoId, row)) continue;
    const status = await app.waitFor(() => app.evalPlayer(PLAYER_LIKE_STATUS(videoId)), { timeoutMs: 8_000 });
    if (status.value === "INDIFFERENT") {
      candidate = { videoId, row };
      break;
    }
  }
  if (!candidate) {
    checks.push(check("T17.likeUnlike", "select a public track with a confirmed neutral like state", { measured: { candidates: Math.min(ids.length, 3) }, pass: false, blocked: "the official player did not confirm an indifferent candidate" }));
    return;
  }
  const { videoId, row } = candidate;
  const likeButton = '[data-testid="action-like"]';
  const liked = await mutationVia(app, [actionsIn("search", videoId, row), likeButton], accountEpoch, { kind: "setLike", videoId, liked: true });
  const read = () => likedContents(app, videoId);
  let likedIds = liked.attempted ? await read().catch(() => null) : null;
  if (liked.attempted && !likedIds?.includes(videoId)) {
    await sleep(750);
    likedIds = await read().catch(() => null) ?? likedIds;
  }
  const likedRow = likedIds?.indexOf(videoId) ?? -1;
  const unliked = likedRow >= 0 && liked.targetMatches !== false
    ? await mutationVia(app, [actionsIn("liked", videoId, likedRow), likeButton], accountEpoch, { kind: "setLike", videoId, liked: false })
    : null;
  let officialRestore = null;
  // Use the official control only when the app did not send an unlike or the service
  // definitively refused it. An uncertain unlike is resolved by observation alone.
  const canUseOfficialControl = !unliked?.attempted || (unliked.outcome === "rejected" && unliked.targetMatches);
  if (liked.attempted && liked.targetMatches !== false && unliked?.targetMatches !== false && await sameAccount(app, accountEpoch)) {
    // Read the exact song on a new official document so the renderer cannot
    // report its old pre-write like state. Liked Music can be many pages long.
    const reloadAt = Date.now();
    const reload = await app.call("winop", { target: "player", op: "appReloadPlayer" }).then(() => true, () => false);
    const fresh = reload && (await app.waitFor((snapshot, state) =>
      state.lastBridgeReady?.at >= reloadAt && snapshot?.player?.connection === "ready", { timeoutMs: 15_000 })).ok;
    const currentSearch = fresh ? await uiSearch(app, opt.query) : null;
    const currentRow = currentSearch?.response?.ids?.indexOf(videoId) ?? -1;
    const landed = currentRow >= 0 && await playSearchTrack(app, videoId, currentRow).catch(() => false);
    const button = landed && canUseOfficialControl && (await app.waitFor(() => app.evalPlayer(REMOVE_OFFICIAL_LIKE(videoId)), { timeoutMs: 6000 })).ok;
    const attempted = button && await sameAccount(app, accountEpoch);
    const clicked = attempted ? await app.evalPlayer(REMOVE_OFFICIAL_LIKE(videoId, true), { userGesture: true }).catch(() => null) : false;
    const indifferent = landed && (await app.waitFor(async () => (await app.evalPlayer(PLAYER_LIKE_STATUS(videoId))) === "INDIFFERENT", { timeoutMs: 6000 })).ok && await sameAccount(app, accountEpoch);
    officialRestore = { freshDocument: fresh, attempted, clicked, indifferent };
  }
  const catalogAfterUnlike = !officialRestore?.indifferent && unliked?.attempted && await sameAccount(app, accountEpoch)
    ? await readPresence(read, videoId, false)
    : null;
  const restored = officialRestore?.indifferent === true || catalogAfterUnlike === false;
  checks.push(check("T17.likeUnlike", "OUR UI likes the verified neutral track, reads it in Liked Music, then unlikes it and confirms restoration", {
    measured: { before: "INDIFFERENT", liked, afterLike: likedIds ? likedRow >= 0 : null, unliked, catalogAfterUnlike, officialRestore, restored },
    pass: liked.outcome === "confirmed" && liked.targetMatches && likedRow >= 0 && unliked?.outcome === "confirmed" && unliked.targetMatches && restored,
  }));
}

async function main() {
  const sink = await ensureNullSink();
  let app;
  const checks = [];
  const ev = {};
  try {
    app = await AppInstance.launch({ label: "t17", workDir: join(process.env.XDG_RUNTIME_DIR ?? "/tmp", "ytmcp-qa", `t17-${Date.now()}`) });
    const ready = await waitAppReady(app, { timeoutMs: 90_000 });
    await app.waitFor((s) => s?.authState?.playerCatalogMatched === true, { timeoutMs: 20_000 });
    const snap = await app.snapshot();
    ev.preconditions = { ready: ready.ok, accountEpochPresent: Boolean(snap?.accountEpoch), playerCatalogMatched: snap?.authState?.playerCatalogMatched ?? null, likeMutation: snap?.capabilities?.likeMutation, playlistAddMutation: snap?.capabilities?.playlistAddMutation };
    if (!ready.ok || !ev.preconditions.accountEpochPresent || ev.preconditions.playerCatalogMatched !== true) {
      checks.push(check("T17.preconditions", "player is ready and catalog identity matches", { measured: ev.preconditions, pass: false, blocked: "the player and catalog account identity must be matched" }));
      return;
    }
    await app.call("winop", { target: "main", op: "show" });

    const freshIds = await libraryIds(app);
    const ids = freshIds ? await app.call("findPlaylistByTitle", { title: TEST_TITLE, ids: freshIds }) : [];
    let privateOnOfficialPage = false;
    if (ids.length === 1) {
      const url = new URL("https://music.youtube.com/playlist");
      url.searchParams.set("list", ids[0]);
      await playerGo(app, url.href);
      privateOnOfficialPage = (await app.waitFor(() => app.evalPlayer(PRIVATE_PLAYLIST(ids[0])), { timeoutMs: 10_000 })).ok;
    }
    const target = { matches: ids.length, libraryFresh: freshIds !== null, privateOnOfficialPage };
    const matched = ids.length === 1 && privateOnOfficialPage;
    checks.push(check("T17.testPlaylist", `reuse the existing private playlist titled "${TEST_TITLE}"`, {
      measured: target, pass: matched, blocked: matched ? null : "need exactly one existing title match with private status on the official page",
    }));
    if (opt.probe || !matched) return;

    if (await testAdd(app, ids[0], snap.accountEpoch, checks)) {
      await testLike(app, snap.accountEpoch, checks);
    } else {
      checks.push(check("T17.likeUnlike", "verify like and unlike on the neutral public track", { measured: {}, notRun: "the test playlist could not be confirmed restored" }));
    }
  } finally {
    try {
      if (app) {
        await app.call("winop", { target: "player", op: "loadURL", args: ["https://music.youtube.com/"] }, 30_000).catch(() => {});
        const q = await quitViaMpris(app);
        if (q.leftoverPids.length) await app.kill();
      }
    } finally {
      await unloadNullSink(sink.created ? sink.moduleId : null);
      if (opt.probe) {
        console.log(JSON.stringify({ checks, evidence: ev }, null, 2));
      } else {
        const doc = gateDoc({ gate: "T17", name: "Real account write acceptance", run: "FINAL", meta: { query: opt.query }, checks, evidence: ev });
        console.log(writeEvidence(join(REPO, "artifacts/live/t17-write.json"), doc));
        if (doc.result !== "PASS_TARGET") process.exitCode = 1;
      }
    }
  }
}

await main();
