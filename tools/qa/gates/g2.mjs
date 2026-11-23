// G2: logged-in identity, real cold restart keeping the same account, renderer reload
// re-handshake, secret-store backend and log hygiene. Never reads cookies; identities
// are compared as hashes of the app's own opaque fields.
import { check, scanForSecrets } from "../lib/evidence.mjs";
import { findAppProcesses } from "../lib/app.mjs";
import { run, sha256Short, sinkInputsFor, sleep } from "../lib/sys.mjs";
import { ensurePlaying, pageMedia } from "./common.mjs";

/** Account facts of a running instance (hashed). */
export async function accountFacts(app) {
  const st = await app.state();
  const a = st.snapshot?.authState ?? {};
  const evs = await app.events(0);
  const catalogScopes = [...new Set(evs.filter((e) => e.catalog?.accountScope).map((e) => e.catalog.accountScope))];
  const winList = await app.call("windows");
  const player = winList.find((w) => w.role === "player");
  return {
    authState: a.state ?? null,
    accountScope: sha256Short(a.accountScope ?? null),
    accountEpoch: sha256Short(a.accountEpoch ?? null),
    playerCatalogMatched: a.playerCatalogMatched ?? null,
    authReason: a.reason ?? null,
    bridge: st.lastBridgeReady ? { loggedInObserved: st.lastBridgeReady.loggedInObserved, accountHint: st.lastBridgeReady.accountHint, bridgeEpoch: st.lastBridgeReady.bridgeEpoch } : null,
    catalogResponseScopes: catalogScopes,
    playerOrigin: player?.origin ?? null,
    secretStore: { backend: st.snapshot?.hostHealth?.secretStoreBackend ?? null, safeStorageAvailable: st.snapshot?.hostHealth?.safeStorageAvailable ?? null },
  };
}

/** Quits through the MPRIS Quit method (a real user path that does not depend on close-to-tray). */
export async function quitViaMpris(app, timeoutMs = 25_000) {
  app.knownCgroups = app.cgroups();
  const t0 = Date.now();
  const r = await run("busctl", ["--user", "call", "org.mpris.MediaPlayer2.ytm_glass", "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2", "Quit"]);
  const res = await app.waitExit(timeoutMs, t0);
  return { mprisQuitCall: r.ok, ...res };
}

export function quitChecks(prefix, quit) {
  // only this run's instance counts; other lanes' mock instances are not ours
  const strays = findAppProcesses().filter((p) => p.realProfile);
  return [
    check(`${prefix}.quitClean`, "quit leaves zero app processes (scope cgroups empty, no ytm-daemon/electron of the app)", {
      measured: { exited: quit.exited, exitCode: quit.exit?.code ?? null, quitMs: quit.quitMs, leftover: quit.leftover, straysAfter: strays },
      pass: quit.exited && quit.leftoverPids.length === 0 && strays.length === 0,
    }),
  ];
}

export function restartChecks(before, after, relaunch) {
  return [
    check("G2.loggedIn", "before restart: authenticated, bridge saw a logged-in page", {
      measured: { authState: before.authState, loggedInObserved: before.bridge?.loggedInObserved ?? null },
      pass: before.authState === "authenticated" && before.bridge?.loggedInObserved === true,
    }),
    check("G2.accountMatched", "player (bridge accountHint) and catalog identity match: playerCatalogMatched", {
      measured: { before: before.playerCatalogMatched, after: after.playerCatalogMatched, reasonBefore: before.authReason, reasonAfter: after.authReason },
      pass: before.playerCatalogMatched === true && after.playerCatalogMatched === true,
    }),
    check("G2.sameAccountAfterRestart", "after a full quit and relaunch: same stable accountScope, still authenticated, no sign-in page", {
      measured: {
        accountScopeBefore: before.accountScope,
        accountScopeAfter: after.accountScope,
        accountHintBefore: before.bridge?.accountHint ?? null,
        accountHintAfter: after.bridge?.accountHint ?? null,
        authStateAfter: after.authState,
        loggedInObservedAfter: after.bridge?.loggedInObserved ?? null,
        playerOriginAfter: after.playerOrigin,
        relaunchReadyMs: relaunch.waitedMs,
      },
      pass:
        before.accountScope != null &&
        before.accountScope === after.accountScope &&
        before.bridge?.accountHint != null &&
        before.bridge.accountHint === after.bridge?.accountHint &&
        after.authState === "authenticated" &&
        after.bridge?.loggedInObserved === true &&
        after.playerOrigin === "https://music.youtube.com",
    }),
    check("G2.readOnlyQuery", "a logged-in read-only catalog query answered for the same account scope", {
      measured: { scopesBefore: before.catalogResponseScopes, scopesAfter: after.catalogResponseScopes },
      pass: before.catalogResponseScopes.length > 0 && after.catalogResponseScopes.includes(before.accountScope),
      note: "catalog responses carry the hashed accountScope of the core session",
    }),
    check("G2.secretStore", "secret-store backend recorded; Linux basic_text would not count as encrypted", {
      measured: after.secretStore,
      pass: after.secretStore.backend != null && after.secretStore.backend !== "basic_text" && after.secretStore.safeStorageAvailable === true,
    }),
  ];
}

/** Player renderer reload must produce a new bridge epoch and a fresh logged-in handshake; UI reload must resubscribe. */
export async function reloadChecks(app) {
  // While playing, the official page's beforeunload blocks a reload nobody in the app asked
  // for; the app's own reload and logout blanking must still go through.
  const whilePlaying = await reloadWhilePlaying(app);
  const blanking = await blankWhilePlaying(app);
  if ((await app.snapshot())?.player?.playback === "playing") await app.clickCommand('[data-testid="btn-play-pause"]', { op: "toggle" });
  await sleep(1500);
  const st0 = await app.state();
  const epoch0 = st0.lastBridgeReady?.bridgeEpoch ?? null;
  await app.call("winop", { target: "player", op: "reload" });
  const re = await app.waitFor((s, st) => st.lastBridgeReady && st.lastBridgeReady.bridgeEpoch !== epoch0 && s?.player?.connection === "ready" && st.lastBridgeReady, { timeoutMs: 45_000 });
  const playerReload = {
    oldEpoch: epoch0,
    newEpoch: re.value?.bridgeEpoch ?? null,
    loggedInObserved: re.value?.loggedInObserved ?? null,
    accountHintSame: re.value ? re.value.accountHint === st0.lastBridgeReady?.accountHint : null,
    waitedMs: re.waitedMs,
  };
  const snapCount0 = (await app.state()).snapshotCount;
  const since = (await app.state()).seq;
  await app.call("winop", { target: "main", op: "reload" });
  const ui = await app.waitFor((s, st) => st.snapshotCount > snapCount0, { timeoutMs: 20_000 });
  await sleep(1500);
  const evs = await app.events(since, ["ui->host"]);
  const title = await app.text('[data-testid="player-track-title"]').catch(() => null);
  const snap = await app.snapshot();
  const uiReload = {
    resubscribed: evs.some((e) => e.kind === "ui.subscribeSnapshot"),
    snapshotDelivered: ui.ok,
    playerBarShowsTrack: Boolean(title) && snap?.player?.currentTrack != null ? title !== "" : null,
    waitedMs: ui.waitedMs,
  };
  return {
    measured: { playerReload, uiReload, whilePlaying, blanking },
    checks: [
      check("G2.playerReloadWhilePlaying", "the app's player reload (WindowManager.reloadPlayer) goes through while playing; a reload the app did not initiate stays subject to the page's beforeunload", {
        measured: whilePlaying,
        pass: whilePlaying.playbackBefore === "playing" && whilePlaying.appReload.reloaded === true,
      }),
      check("G2.blankWhilePlaying", "logout blanking path (WindowManager.blankPlayer only; the profile is not cleared) unloads the playing page: no stream left, core shows the bridge as disconnected, reload recovers", {
        measured: blanking,
        pass: blanking.playbackBefore === "playing" && blanking.pageLeft && !blanking.streamAfter && blanking.coreDisconnected && blanking.recovered,
      }),
      check("G2.playerReload", "player renderer reload: new bridgeEpoch, logged-in, same account hint", {
        measured: playerReload,
        pass: re.ok && playerReload.newEpoch !== epoch0 && playerReload.loggedInObserved === true && playerReload.accountHintSame === true,
      }),
      check("G2.uiReload", "UI renderer reload resubscribes and receives a fresh snapshot", {
        measured: uiReload,
        pass: uiReload.resubscribed && uiReload.snapshotDelivered,
      }),
    ],
  };
}

export function logHygieneCheck(id, logsText) {
  const counts = scanForSecrets(logsText);
  return check(id, "app logs contain no cookie/SAPISID/DATASYNC/authorization material", {
    measured: counts,
    pass: Object.values(counts).every((n) => n === 0),
  });
}

async function reloadWhilePlaying(app) {
  await ensurePlaying(app);
  const playing = (await app.snapshot())?.player?.playback;
  const newEpoch = async (since, epoch, timeoutMs) => {
    const re = await app.waitFor((s, st) => st.lastBridgeReady && st.lastBridgeReady.bridgeEpoch !== epoch && st.lastBridgeReady, { timeoutMs });
    const prompts = (await app.events(since)).filter((e) => e.type === "will-prevent-unload").length;
    return { reloaded: re.ok, waitedMs: re.ok ? re.waitedMs : null, willPreventUnloadEvents: prompts };
  };
  const st0 = await app.state();
  await app.call("winop", { target: "player", op: "reload" });
  const harnessReload = await newEpoch(st0.seq, st0.lastBridgeReady?.bridgeEpoch ?? null, 8000);
  await ensurePlaying(app);
  const playingBeforeApp = (await app.snapshot())?.player?.playback;
  const st1 = await app.state();
  await app.call("winop", { target: "player", op: "appReloadPlayer" });
  const appReload = await newEpoch(st1.seq, st1.lastBridgeReady?.bridgeEpoch ?? null, 20_000);
  await app.waitFor((s) => s?.player?.connection === "ready", { timeoutMs: 15_000 });
  return { playbackBefore: playingBeforeApp ?? playing, harnessReload, appReload };
}

async function blankWhilePlaying(app) {
  await ensurePlaying(app);
  const playbackBefore = (await app.snapshot())?.player?.playback;
  const streamBefore = (await sinkInputsFor(app.pids())).entries.some((e) => !e.corked);
  await app.call("winop", { target: "player", op: "appBlankPlayer" }, 20_000);
  await sleep(1500);
  const page = await pageMedia(app);
  const url = await app.evalPlayer("location.href").catch(() => null);
  const streamAfter = (await sinkInputsFor(app.pids())).entries.some((e) => !e.corked);
  const disc = await app.waitFor((s) => s?.player?.connection === "disconnected", { timeoutMs: 5000 });
  // Restore the official page (the profile was never touched).
  const st = await app.state();
  await app.call("winop", { target: "player", op: "loadURL", args: ["https://music.youtube.com/"] }, 30_000);
  const rec = await app.waitFor((s, s2) => s2.lastBridgeReady && s2.seq > st.seq && s2.lastBridgeReady.at > st.lastBridgeReady?.at && s?.player?.connection === "ready", { timeoutMs: 45_000 });
  return {
    playbackBefore,
    streamBefore,
    pageLeft: url === "about:blank" && !page.present,
    streamAfter,
    coreDisconnected: disc.ok,
    recovered: rec.ok,
    recoveredWaitMs: rec.ok ? rec.waitedMs : null,
  };
}
