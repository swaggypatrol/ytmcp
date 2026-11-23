// Shared probes for the gate scripts: independent page facts (read from the official
// page's media element, not from our bridge), UI navigation and a small runner context.
import { sleep } from "../lib/sys.mjs";

/** Media facts read straight from the official page (QA observation only). */
export const PAGE_MEDIA_JS = `(() => {
  const v = document.querySelector("video");
  if (!v) return { present: false, origin: location.origin };
  let ahead = null;
  for (let i = 0; i < v.buffered.length; i++) {
    if (v.buffered.start(i) <= v.currentTime + 0.25 && v.currentTime <= v.buffered.end(i)) ahead = v.buffered.end(i) - v.currentTime;
  }
  const mp = document.querySelector("#movie_player");
  let stats = null, quality = null, apiVolume = null, apiMuted = null;
  try { stats = mp && typeof mp.getStatsForNerds === "function" ? mp.getStatsForNerds() : null; } catch (e) {}
  try { quality = mp && typeof mp.getPlaybackQuality === "function" ? mp.getPlaybackQuality() : null; } catch (e) {}
  try { apiVolume = mp && typeof mp.getVolume === "function" ? mp.getVolume() : null; } catch (e) {}
  try { apiMuted = mp && typeof mp.isMuted === "function" ? mp.isMuted() : null; } catch (e) {}
  const q = new URLSearchParams(location.search);
  let vid = null;
  try { vid = mp && typeof mp.getVideoData === "function" ? mp.getVideoData().video_id : null; } catch (e) {}
  return {
    present: true,
    origin: location.origin,
    videoId: vid || q.get("v"),
    t: Math.round(v.currentTime * 1000) / 1000,
    d: Number.isFinite(v.duration) ? Math.round(v.duration * 1000) / 1000 : null,
    paused: v.paused, ended: v.ended, readyState: v.readyState, networkState: v.networkState,
    elMuted: v.muted, elVolume: v.volume, apiVolume, apiMuted,
    bufferedAheadSec: ahead == null ? null : Math.round(ahead * 10) / 10,
    videoWidth: v.videoWidth, videoHeight: v.videoHeight,
    emeActive: v.mediaKeys != null,
    statsCodecs: stats && stats.codecs ? String(stats.codecs) : null,
    statsAudioFormat: stats && (stats.audio_format || stats.afmt) ? String(stats.audio_format || stats.afmt) : null,
    quality
  };
})()`;

export async function pageMedia(app) {
  try {
    return await app.evalPlayer(PAGE_MEDIA_JS, { timeoutMs: 8000 });
  } catch (err) {
    return { present: false, error: String(err?.message ?? err) };
  }
}

/** Core projection of the player (from the observed snapshot), reduced to public facts. */
export function corePlayer(snapshot) {
  const p = snapshot?.player;
  if (!p) return null;
  return {
    connection: p.connection,
    playback: p.playback,
    videoId: p.currentTrack?.videoId ?? null,
    trackInstanceId: p.trackInstanceId ?? null,
    positionMs: p.timeAnchor?.positionMs ?? null,
    durationMs: p.timeAnchor?.durationMs ?? p.currentTrack?.durationMs ?? null,
    extrapolating: p.timeAnchor?.extrapolating ?? null,
    volume: p.volume,
    muted: p.muted,
    codec: p.codec ?? null,
    audioMode: p.audioMode ?? null,
    qualityLabel: p.qualityLabel ?? null,
    drmProtected: p.drmProtected ?? null,
    bridgeEpoch: p.bridgeEpoch ?? null,
  };
}

export async function navigate(app, view) {
  const r = await app.click(`[data-testid="nav-${view}"]`);
  await sleep(400);
  return r;
}

/** Waits for a core.catalogResponse of `resultType` after `since`; returns its summary. */
export async function waitCatalog(app, since, resultType, timeoutMs = 25_000) {
  const ev = await app.waitForEvent((e) => e.type === "core->ui" && e.kind === "core.catalogResponse" && (!resultType || e.catalog?.resultType === resultType), {
    since,
    timeoutMs,
  });
  return ev?.catalog ?? null;
}

/** The song-search answer (or its error), ignoring other views' responses such as the home feed. */
export async function waitSearchResult(app, since, timeoutMs = 25_000) {
  const ev = await app.waitForEvent(
    (e) => e.type === "core->ui" && ((e.kind === "core.catalogResponse" && ["searchSongs", "error"].includes(e.catalog?.resultType)) || e.kind === "core.error"),
    { since, timeoutMs }
  );
  if (!ev) return null;
  return ev.kind === "core.error" ? { resultType: "error", error: ev.error ?? null } : ev.catalog;
}

/** Searches through the real search box (value + input event, then Enter-equivalent submit). */
export async function uiSearch(app, query) {
  await navigate(app, "search");
  const since = (await app.state()).seq;
  const set = await app.setInput('[data-testid="search-input"]', query, { change: false });
  const q = await app.waitForEvent((e) => e.type === "ui->host" && e.kind === "ui.catalogQuery" && e.query?.kind === "searchSongs", { since, timeoutMs: 6000 });
  const resp = await waitSearchResult(app, since);
  await sleep(500);
  const rows = await app.evalMain(`document.querySelectorAll('[data-testid="view-search"] [data-testid="virtual-track-row"]').length`);
  return { set, querySent: Boolean(q), queryAck: q?.ack ?? null, response: resp, renderedRows: rows };
}

export function sinceSeq(app) {
  return app.state().then((s) => s.seq);
}

export function nowIso() {
  return new Date().toISOString();
}

/**
 * Makes the real page play via the UI play button, retrying (each try is a new user
 * click) while a command ends indeterminate; returns every attempt for the evidence.
 */
export async function ensurePlaying(app, { tries = 3 } = {}) {
  const attempts = [];
  // Right after launch the core may be restoring the last track: it plays it behind the output
  // gate (player muted) and then pauses. That silent play is not "playing".
  const audiblyPlaying = async () => {
    const [page, wins, snap] = await Promise.all([pageMedia(app), app.call("windows").catch(() => []), app.snapshot()]);
    const player = wins.find((w) => w.role === "player");
    return page.present && page.paused === false && player && player.audioMuted === false && snap?.player?.playback === "playing";
  };
  const gated = async () => (await app.call("windows").catch(() => [])).find((w) => w.role === "player")?.audioMuted === true;
  for (let i = 0; i < tries; i++) {
    // Let a running gated restore settle first; a toggle now would act on its silent play.
    for (let t0 = Date.now(); (await gated()) && Date.now() - t0 < 15_000; ) await sleep(500);
    if (await audiblyPlaying()) {
      await sleep(1500);
      if (await audiblyPlaying()) return { playing: true, attempts };
    }
    const r = await app.clickCommand('[data-testid="btn-play-pause"]', { op: "toggle" });
    attempts.push({ finalStatus: r.finalStatus, error: r.error, statuses: r.statuses });
    await sleep(2500);
  }
  const page = await pageMedia(app);
  return { playing: page.present && page.paused === false, attempts };
}
