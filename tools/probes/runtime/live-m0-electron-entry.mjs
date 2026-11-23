import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { app } from "electron";

// Disable Chromium's built-in MPRIS/MediaSession before app.ready() so only ytm_glass registers on D-Bus.
if (!app.isReady() && app.commandLine && typeof app.commandLine.appendSwitch === "function") {
  const existing =
    typeof app.commandLine.getSwitchValue === "function"
      ? app.commandLine.getSwitchValue("disable-features")
      : "";
  const merged = Array.from(
    new Set(
      existing
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean)
        .concat(["HardwareMediaKeyHandling", "MediaSessionService"])
    )
  ).join(",");
  app.commandLine.appendSwitch("disable-features", merged);
}

const execFileAsync = promisify(execFile);

function sleep(ms) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, ms));
}

async function execText(cmd, args, timeoutMs = 8000) {
  try {
    const { stdout, stderr } = await execFileAsync(cmd, args, {
      encoding: "utf8",
      timeout: timeoutMs,
    });
    return { ok: true, stdout: stdout ?? "", stderr: stderr ?? "" };
  } catch (err) {
    return {
      ok: false,
      stdout: err?.stdout ? String(err.stdout) : "",
      stderr: err?.stderr ? String(err.stderr) : String(err),
      code: typeof err?.code === "number" ? err.code : 1,
    };
  }
}

async function captureMprisBusCheck() {
  const busRes = await execText("busctl", ["--user", "list"]);
  const mprisNames = busRes.stdout
    .split(/\r?\n/)
    .map((line) => line.trim().split(/\s+/)[0] ?? "")
    .filter((name) => name.includes("mpris"));
  const ownPids = new Set([process.pid]);
  try {
    for (const metric of app.getAppMetrics()) {
      if (typeof metric.pid === "number") {
        ownPids.add(metric.pid);
      }
    }
  } catch {
    // Ignore if metrics unavailable
  }

  const chromiumInstances = mprisNames.filter((name) => name.includes("chromium"));
  const appChromiumInstances = chromiumInstances.filter((name) => {
    const match = /\.chromium\.instance(\d+)$/.exec(name);
    return match ? ownPids.has(Number.parseInt(match[1], 10)) : false;
  });
  const externalChromiumInstances = chromiumInstances.filter(
    (name) => !appChromiumInstances.includes(name)
  );
  const ytmInstances = mprisNames.filter((name) => name.includes("ytm_glass"));

  const playerctlList = await execText("playerctl", ["-l"]);
  const playerctlStatus = await execText("playerctl", ["-p", "ytm_glass", "status"]);
  const playerctlMeta = await execText("playerctl", [
    "-p",
    "ytm_glass",
    "metadata",
    "--format",
    "{{status}}|{{mpris:trackid}}|{{xesam:title}}|{{xesam:artist}}|{{mpris:length}}",
  ]);

  return {
    timestamp: new Date().toISOString(),
    appPid: process.pid,
    mprisBusNames: mprisNames,
    ytmGlassBusCount: ytmInstances.length,
    chromiumMprisCount: chromiumInstances.length,
    appChromiumMprisCount: appChromiumInstances.length,
    appChromiumMprisNames: appChromiumInstances,
    externalChromiumMprisNames: externalChromiumInstances,
    singleExpectedService: ytmInstances.length === 1 && appChromiumInstances.length === 0,
    playerctlList: playerctlList.stdout
      .split(/\r?\n/)
      .map((s) => s.trim())
      .filter(Boolean),
    playerctlStatus: playerctlStatus.ok ? playerctlStatus.stdout.trim() : `ERR:${playerctlStatus.stderr.trim()}`,
    playerctlMetadataSummary: playerctlMeta.ok ? playerctlMeta.stdout.trim() : null,
  };
}

async function captureAudioSinkInputs() {
  const res = await execText("pactl", ["list", "sink-inputs"]);
  if (!res.ok) {
    return { ok: false, error: res.stderr.trim(), entries: [] };
  }
  const blocks = res.stdout.split(/(?=^Sink Input #\d+)/m).filter((b) => b.trim().startsWith("Sink Input #"));
  const entries = blocks.map((block) => {
    const idMatch = block.match(/^Sink Input #(\d+)/m);
    const corkedMatch = block.match(/^\s*Corked:\s*(\S+)/m);
    const appNameMatch = block.match(/^\s*application\.name\s*=\s*"([^"]*)"/m);
    const pidMatch = block.match(/^\s*application\.process\.id\s*=\s*"([^"]*)"/m);
    const binaryMatch = block.match(/^\s*application\.process\.binary\s*=\s*"([^"]*)"/m);
    const formatMatch = block.match(/^\s*Sample Specification:\s*(.+)$/m);
    return {
      sinkInputId: idMatch ? idMatch[1] : null,
      corked: corkedMatch ? corkedMatch[1] : null,
      applicationName: appNameMatch ? appNameMatch[1] : null,
      processId: pidMatch ? pidMatch[1] : null,
      processBinary: binaryMatch ? binaryMatch[1] : null,
      sampleSpec: formatMatch ? formatMatch[1].trim() : null,
    };
  });
  return { ok: true, entries };
}

async function run() {
  if (process.env.YTM_GLASS_ALLOW_LIVE !== "1") {
    throw new Error("Refusing to run live M0 probe without YTM_GLASS_ALLOW_LIVE=1");
  }

  const bootstrapModuleUrl = pathToFileURL(resolve(process.env.YTM_GLASS_M0_MAIN_MODULE)).href;
  const entryModuleUrl = pathToFileURL(resolve(process.env.YTM_GLASS_M0_ENTRY_MODULE)).href;
  const reportPath = resolve(process.env.YTM_GLASS_LIVE_REPORT_PATH);
  const mprisReportPath = resolve(process.env.YTM_GLASS_LIVE_MPRIS_PATH);

  const { startDesktopShell } = await import(bootstrapModuleUrl);
  const { createIntegratedDesktopHooks } = await import(entryModuleUrl);

  const observed = {
    sessionSummary: {
      seen: false,
      hasAuthCookies: false,
      cookieCount: 0,
      hasChannelLabel: false,
      authUserIndex: null,
    },
    bridgeReadyHistory: [],
    bridgePlaybackEvents: [],
    snapshots: [],
    deltas: [],
    commandReceipts: new Map(),
    pendingRequests: new Map(),
    hostErrors: [],
  };

  let seqCounter = 1000;
  let latestSnapshot = null;
  let latestPlayer = null;
  let latestAuth = null;

  const waiters = [];
  const notifyWaiters = () => {
    for (let i = waiters.length - 1; i >= 0; i -= 1) {
      if (waiters[i].check()) {
        const w = waiters.splice(i, 1)[0];
        clearTimeout(w.timer);
        w.resolve();
      }
    }
  };

  const waitUntil = (label, check, timeoutMs = 25_000) => {
    if (check()) {
      return Promise.resolve();
    }
    return new Promise((resolvePromise, rejectPromise) => {
      const timer = setTimeout(() => {
        const idx = waiters.indexOf(entry);
        if (idx !== -1) waiters.splice(idx, 1);
        rejectPromise(new Error(`Timed out after ${timeoutMs}ms waiting for: ${label}`));
      }, timeoutMs);
      const entry = {
        check,
        resolve: resolvePromise,
        reject: rejectPromise,
        timer,
      };
      waiters.push(entry);
    });
  };

  let controller = null;
  const integrated = createIntegratedDesktopHooks({
    daemonBin: process.env.YTM_GLASS_DAEMON_BIN,
    clientOptions: {
      // Errors for host.sessionMaterial (e.g. a failed identity read) never reach onUiEnvelope.
      onSessionMaterialError: (err) => {
        observed.hostErrors.push({ ts: Date.now(), code: err.code, safeMessage: err.safeMessage, source: "sessionMaterial" });
      },
      onUiEnvelope: (env) => {
        if (env.kind === "core.snapshot") {
          const snap = env.payload;
          latestSnapshot = snap;
          latestPlayer = snap.player;
          latestAuth = snap.authState;
          observed.snapshots.push({
            ts: Date.now(),
            revision: snap.revision,
            authState: snap.authState?.state ?? null,
            playerCatalogMatched: snap.authState?.playerCatalogMatched ?? null,
            playback: snap.player?.playback ?? null,
            positionMs: snap.player?.timeAnchor?.positionMs ?? null,
          });
        } else if (env.kind === "core.delta") {
          const d = env.payload;
          if (d.deltaType === "playerChanged") {
            latestPlayer = d.player;
            if (latestSnapshot) {
              latestSnapshot.player = d.player;
              latestSnapshot.pending = d.pending;
            }
          } else if (d.deltaType === "authChanged") {
            latestAuth = d.auth_state;
            if (latestSnapshot) {
              latestSnapshot.authState = d.auth_state;
            }
          } else if (d.deltaType === "commandUpdated" && d.receipt) {
            observed.commandReceipts.set(d.receipt.commandId, {
              ...d.receipt,
              receivedAtMs: Date.now(),
            });
          } else if (d.deltaType === "networkChanged" && latestSnapshot) {
            latestSnapshot.networkState = d.network_state;
          }
          observed.deltas.push({ ts: Date.now(), deltaType: d.deltaType, revision: d.revision });
        } else if (env.kind === "core.commandReceipt") {
          const receipt = env.payload;
          observed.commandReceipts.set(receipt.commandId, {
            ...receipt,
            receivedAtMs: Date.now(),
          });
        } else if (env.kind === "core.catalogResponse" && env.requestId) {
          const p = observed.pendingRequests.get(env.requestId);
          if (p) {
            observed.pendingRequests.delete(env.requestId);
            p.resolve({ ok: true, payload: env.payload });
          }
        } else if (env.kind === "core.error") {
          if (env.requestId && observed.pendingRequests.has(env.requestId)) {
            const p = observed.pendingRequests.get(env.requestId);
            observed.pendingRequests.delete(env.requestId);
            p.resolve({ ok: false, error: env.payload });
          } else {
            observed.hostErrors.push({
              ts: Date.now(),
              code: env.payload?.code ?? null,
              safeMessage: env.payload?.safeMessage ?? null,
            });
          }
        }
        notifyWaiters();
      },
    },
  });

  // Wrap hooks cleanly to inspect redacted metadata without touching credentials
  const origSessionMaterial = integrated.hooks.onSessionMaterial;
  integrated.hooks.onSessionMaterial = (payload) => {
    const cookieKeys = Object.keys(payload.cookiePairs ?? {});
    observed.sessionSummary = {
      seen: true,
      hasAuthCookies: cookieKeys.length > 0,
      cookieCount: cookieKeys.length,
      hasChannelLabel: typeof payload.channelLabel === "string" && payload.channelLabel.trim() !== "",
      authUserIndex: payload.authUserIndex ?? null,
      hasAccountEpoch: typeof payload.accountEpoch === "string" && payload.accountEpoch.length > 0,
    };
    origSessionMaterial?.(payload);
    notifyWaiters();
  };

  const origBridgeEnvelope = integrated.hooks.onBridgeEnvelope;
  integrated.hooks.onBridgeEnvelope = async (envelope) => {
    if (envelope.kind === "bridge.ready") {
      observed.bridgeReadyHistory.push({
        ts: Date.now(),
        bridgeEpoch: envelope.payload?.bridgeEpoch ?? null,
        bridgeVersion: envelope.payload?.bridgeVersion ?? null,
        loggedInObserved: envelope.payload?.loggedInObserved ?? false,
        hasAccountHint:
          typeof envelope.payload?.accountHint === "string" &&
          envelope.payload.accountHint.trim() !== "",
      });
    } else if (envelope.kind === "bridge.playbackObserved") {
      observed.bridgePlaybackEvents.push({
        ts: Date.now(),
        playback: envelope.payload?.playback ?? null,
        positionMs: envelope.payload?.positionMs ?? null,
        codec: envelope.payload?.codec ?? null,
        audioMode: envelope.payload?.audioMode ?? null,
        qualityLabel: envelope.payload?.qualityLabel ?? null,
        drmProtected: envelope.payload?.drmProtected ?? null,
        videoId: envelope.payload?.track?.videoId ?? null,
      });
    }
    await origBridgeEnvelope?.(envelope);
    notifyWaiters();
  };

  const sendCatalogQuery = (req, timeoutMs = 20_000) => {
    const requestId = `live-cat-${randomUUID()}`;
    return new Promise((resolvePromise, rejectPromise) => {
      const timer = setTimeout(() => {
        observed.pendingRequests.delete(requestId);
        rejectPromise(new Error(`Timed out catalog query ${req.kind}`));
      }, timeoutMs);
      observed.pendingRequests.set(requestId, {
        resolve: (val) => {
          clearTimeout(timer);
          resolvePromise(val);
        },
        reject: (err) => {
          clearTimeout(timer);
          rejectPromise(err);
        },
      });
      seqCounter += 1;
      const sent = integrated.rustHost.sendEnvelope({
        protocolVersion: 1,
        kind: "ui.catalogQuery",
        coreEpoch: integrated.rustHost.getCoreEpoch(),
        accountEpoch: latestSnapshot?.accountEpoch ?? null,
        bridgeEpoch: latestPlayer?.bridgeEpoch ?? null,
        requestId,
        seq: seqCounter,
        payload: req,
      });
      if (!sent) {
        clearTimeout(timer);
        observed.pendingRequests.delete(requestId);
        rejectPromise(new Error(`Failed to send catalogQuery ${req.kind}`));
      }
    });
  };

  const fetchSnapshot = async () => {
    const prevCount = observed.snapshots.length;
    seqCounter += 1;
    integrated.rustHost.sendEnvelope({
      protocolVersion: 1,
      kind: "ui.subscribeSnapshot",
      coreEpoch: integrated.rustHost.getCoreEpoch(),
      accountEpoch: null,
      bridgeEpoch: null,
      requestId: `snap-${randomUUID()}`,
      seq: seqCounter,
      payload: {},
    });
    await waitUntil("core.snapshot", () => observed.snapshots.length > prevCount, 10_000);
    return latestSnapshot;
  };

  const runPlayerCommand = async (op, args, label, checkObserved, timeoutMs = 20_000) => {
    const commandId = `cmd-${op}-${randomUUID()}`;
    const issuedAtMs = Date.now();
    seqCounter += 1;
    const sent = integrated.rustHost.sendEnvelope({
      protocolVersion: 1,
      kind: "ui.playerCommand",
      coreEpoch: integrated.rustHost.getCoreEpoch(),
      accountEpoch: latestSnapshot?.accountEpoch ?? null,
      bridgeEpoch: latestPlayer?.bridgeEpoch ?? null,
      requestId: `req-${commandId}`,
      seq: seqCounter,
      payload: {
        commandId,
        op,
        args: args ?? null,
      },
    });
    if (!sent) {
      throw new Error(`Failed to send ui.playerCommand ${op}`);
    }
    let settledOnObserved = false;
    try {
      await waitUntil(
        `${label} terminal receipt`,
        () => {
          const rec = observed.commandReceipts.get(commandId);
          return Boolean(
            rec &&
              (rec.status === "observed" ||
                rec.status === "failed" ||
                rec.status === "indeterminate" ||
                rec.status === "cancelled")
          );
        },
        timeoutMs
      );
      const rec = observed.commandReceipts.get(commandId);
      if (rec?.status === "observed") {
        await waitUntil(`${label} observed predicate`, () => checkObserved(rec), 3_000).catch(
          () => {}
        );
        settledOnObserved = checkObserved(rec);
      }
    } catch {
      // Timeout waiting for terminal receipt; fall through and return diagnostic receipt state.
    }
    const receipt = observed.commandReceipts.get(commandId) ?? {
      status: "probe_timeout",
      acceptedAtMs: issuedAtMs,
      observedAtMs: null,
      deduplicated: false,
      error: { code: "TIMEOUT", safeMessage: "Timed out waiting for terminal command receipt" },
    };
    const snap = await fetchSnapshot();
    return {
      label,
      op,
      commandId,
      issuedAtMs,
      predicateMatched: settledOnObserved,
      receipt: {
        status: receipt.status,
        acceptedAtMs: receipt.acceptedAtMs,
        observedAtMs: receipt.observedAtMs ?? null,
        deduplicated: receipt.deduplicated,
        error: receipt.error ?? null,
      },
      observedPlayer: {
        playback: snap.player?.playback ?? null,
        positionMs: snap.player?.timeAnchor?.positionMs ?? null,
        durationMs: snap.player?.timeAnchor?.durationMs ?? null,
        volume: snap.player?.volume ?? null,
        muted: snap.player?.muted ?? null,
        videoId: snap.player?.currentTrack?.videoId ?? null,
        trackInstanceId: snap.player?.trackInstanceId ?? null,
        queueLoadedCount: snap.queueSummary?.loadedCount ?? 0,
        codec: snap.player?.codec ?? "UNKNOWN",
        audioMode: snap.player?.audioMode ?? "UNKNOWN",
        qualityLabel: snap.player?.qualityLabel ?? "UNKNOWN",
        drmProtected: snap.player?.drmProtected ?? "UNKNOWN",
      },
    };
  };

  const results = {
    probe: "live-M0-G2-G4-G3-G5",
    timestamp: new Date().toISOString(),
    playerWindowShown: false,
    g2: {},
    g4: {},
    g3Controls: {},
    g5: {},
    hostErrors: [],
  };

  try {
    controller = await startDesktopShell(integrated.hooks);
    if (!controller) {
      throw new Error("Failed to acquire single-instance lock for live M0 probe");
    }

    app.on("will-quit", () => {
      void integrated.shutdown();
    });

    await integrated.attachController(controller);

    const playerWin = controller.windowManager.getPlayerWindow();
    results.playerWindowShown = Boolean(playerWin && playerWin.isVisible());

    // Subscribe to daemon snapshots
    await fetchSnapshot();

    // --- G2: Cold restart persistence & renderer reload ---
    await waitUntil(
      "session cookies + bridge.ready(loggedInObserved=true) + authenticated state",
      () =>
        observed.sessionSummary.hasAuthCookies &&
        observed.bridgeReadyHistory.some((b) => b.loggedInObserved === true) &&
        latestAuth?.state === "authenticated",
      35_000
    );

    const snapBeforeReload = await fetchSnapshot();
    const initialBridge =
      observed.bridgeReadyHistory.find((b) => b.loggedInObserved === true) ??
      observed.bridgeReadyHistory[observed.bridgeReadyHistory.length - 1];

    // Reload player renderer to verify new bridgeEpoch handshake and loggedInObserved=true
    if (!playerWin || playerWin.isDestroyed()) {
      throw new Error("Player window missing for reload check");
    }
    playerWin.webContents.reload();
    await waitUntil(
      "bridge.ready(loggedInObserved=true) after player renderer reload",
      () =>
        observed.bridgeReadyHistory.some(
          (b) => b.bridgeEpoch !== initialBridge.bridgeEpoch && b.loggedInObserved === true
        ),
      35_000
    );
    const reloadedBridge =
      observed.bridgeReadyHistory.find(
        (b) => b.bridgeEpoch !== initialBridge.bridgeEpoch && b.loggedInObserved === true
      ) ?? observed.bridgeReadyHistory[observed.bridgeReadyHistory.length - 1];
    const snapAfterReload = await fetchSnapshot();

    results.g2 = {
      status:
        observed.sessionSummary.hasAuthCookies &&
        initialBridge.loggedInObserved &&
        reloadedBridge.loggedInObserved &&
        reloadedBridge.bridgeEpoch !== initialBridge.bridgeEpoch &&
        snapAfterReload.authState.state === "authenticated"
          ? "PASS_TARGET"
          : "FAIL",
      safeStorageStatus: controller.safeStorageStatus,
      sessionSummary: observed.sessionSummary,
      initialBridge: {
        bridgeEpoch: initialBridge.bridgeEpoch,
        bridgeVersion: initialBridge.bridgeVersion,
        loggedInObserved: initialBridge.loggedInObserved,
        hasAccountHint: initialBridge.hasAccountHint,
      },
      reloadedBridge: {
        bridgeEpoch: reloadedBridge.bridgeEpoch,
        bridgeVersion: reloadedBridge.bridgeVersion,
        loggedInObserved: reloadedBridge.loggedInObserved,
        epochChanged: reloadedBridge.bridgeEpoch !== initialBridge.bridgeEpoch,
      },
      authState: {
        state: snapBeforeReload.authState.state,
        hasAccountEpoch: Boolean(snapBeforeReload.authState.accountEpoch),
        accountScopePrefix: snapBeforeReload.authState.accountScope
          ? `${snapBeforeReload.authState.accountScope.slice(0, 8)}...`
          : null,
        playerCatalogMatched: snapBeforeReload.authState.playerCatalogMatched,
      },
    };

    // --- G4: Catalog (READ-ONLY) ---
    const searchResp = await sendCatalogQuery({
      kind: "searchSongs",
      query: "Daft Punk",
      limit: 10,
    });
    const playlistsResp = await sendCatalogQuery({
      kind: "libraryPlaylists",
      limit: 25,
    });
    const postSearchSnap = await fetchSnapshot();

    let playlistPaginationCheck = {
      executed: false,
      page1Count: 0,
      hasNextCursor: false,
      page2Executed: false,
      page2Count: 0,
      duplicateVideoIdCount: 0,
      freshness: null,
      providerVersion: null,
    };

    const searchData = searchResp.ok && searchResp.payload.resultType === "searchSongs" ? searchResp.payload : null;
    const libraryData =
      playlistsResp.ok && playlistsResp.payload.resultType === "libraryPlaylists"
        ? playlistsResp.payload
        : null;

    // Kept strictly in memory for multi-track queue fallback if public radio doesn't populate Next.
    let fallbackPlaylistTarget = null;
    if (libraryData && Array.isArray(libraryData.items) && libraryData.items.length > 0) {
      // Pick first playlist with trackCount > 25 if present, otherwise first playlist
      const targetPlaylist =
        libraryData.items.find((p) => typeof p.trackCount === "number" && p.trackCount > 25) ??
        libraryData.items[0];
      const page1Resp = await sendCatalogQuery({
        kind: "playlistTracks",
        playlistId: targetPlaylist.playlistId,
        limit: 25,
      });
      if (page1Resp.ok && page1Resp.payload.resultType === "playlistTracks") {
        const p1 = page1Resp.payload.tracks;
        const avail = p1.items.filter((t) => t.availability === "available");
        if (avail.length >= 2) {
          fallbackPlaylistTarget = {
            videoId: avail[0].videoId,
            playlistId: targetPlaylist.playlistId,
          };
        }
        const page1Ids = new Set(p1.items.map((t) => t.videoId));
        playlistPaginationCheck.executed = true;
        playlistPaginationCheck.page1Count = p1.items.length;
        playlistPaginationCheck.hasNextCursor = Boolean(p1.nextCursor);
        playlistPaginationCheck.freshness = p1.freshness;
        playlistPaginationCheck.providerVersion = p1.providerVersion;

        if (p1.nextCursor) {
          const page2Resp = await sendCatalogQuery({
            kind: "playlistTracks",
            playlistId: targetPlaylist.playlistId,
            cursor: p1.nextCursor,
            limit: 25,
          });
          if (page2Resp.ok && page2Resp.payload.resultType === "playlistTracks") {
            const p2 = page2Resp.payload.tracks;
            let dups = 0;
            for (const item of p2.items) {
              if (page1Ids.has(item.videoId)) dups += 1;
            }
            playlistPaginationCheck.page2Executed = true;
            playlistPaginationCheck.page2Count = p2.items.length;
            playlistPaginationCheck.duplicateVideoIdCount = dups;
          }
        }
      }
    }

    // Offline cache stale verification
    integrated.rustHost.sendNetworkChanged("offline");
    await sleep(300);
    const offlinePlaylistsResp = await sendCatalogQuery({
      kind: "libraryPlaylists",
      limit: 25,
    });
    integrated.rustHost.sendNetworkChanged("online");
    await sleep(300);

    const offlineLibraryFreshness =
      offlinePlaylistsResp.ok && offlinePlaylistsResp.payload.resultType === "libraryPlaylists"
        ? offlinePlaylistsResp.payload.freshness
        : offlinePlaylistsResp.error?.code ?? null;

    results.g4 = {
      status:
        searchData &&
        searchData.songs.items.length > 0 &&
        libraryData &&
        offlineLibraryFreshness === "stale" &&
        postSearchSnap.authState.playerCatalogMatched
          ? "PASS_TARGET"
          : "FAIL",
      searchSongs: searchData
        ? {
            ok: true,
            itemCount: searchData.songs.items.length,
            freshness: searchData.songs.freshness,
            providerVersion: searchData.songs.providerVersion,
            samplePublicTrackVideoId: searchData.songs.items[0]?.videoId ?? null,
          }
        : { ok: false, error: searchResp.error ?? searchResp.payload ?? null },
      libraryPlaylists: libraryData
        ? {
            ok: true,
            itemCount: libraryData.items.length,
            freshness: libraryData.freshness,
            providerVersion: libraryData.providerVersion,
            hasNextCursor: Boolean(libraryData.nextCursor),
          }
        : { ok: false, error: playlistsResp.error ?? playlistsResp.payload ?? null },
      playlistPagination: playlistPaginationCheck,
      offlineCachedLibraryPlaylists: {
        freshness: offlineLibraryFreshness,
        itemCount:
          offlinePlaylistsResp.ok && offlinePlaylistsResp.payload.resultType === "libraryPlaylists"
            ? offlinePlaylistsResp.payload.items.length
            : 0,
      },
      playerCatalogMatched: postSearchSnap.authState.playerCatalogMatched,
    };

    // --- G3 Controls (HIDDEN player) + G5 MPRIS ---
    const playableTrack =
      searchData?.songs.items.find((t) => t.availability === "available") ?? searchData?.songs.items[0];
    if (!playableTrack) {
      throw new Error("No public playable track found from searchSongs");
    }

    const radioListId = `RDAMVM${playableTrack.videoId}`;
    const playTargetRes = await runPlayerCommand(
      "playTarget",
      { target: { videoId: playableTrack.videoId, playlistId: radioListId } },
      "playTarget(public search track + RDAMVM radio queue)",
      (rec) =>
        rec.status === "observed" &&
        latestPlayer?.playback === "playing" &&
        latestPlayer?.currentTrack?.videoId === playableTrack.videoId,
      35_000
    );

    // Let it play briefly so audio stream + YTM Up Next queue + MPRIS metadata stabilize
    await sleep(3000);
    const audioWhilePlaying = await captureAudioSinkInputs();
    const mprisWhilePlaying = await captureMprisBusCheck();
    mkdirSync(dirname(mprisReportPath), { recursive: true });
    writeFileSync(mprisReportPath, `${JSON.stringify(mprisWhilePlaying, null, 2)}\n`, "utf8");

    // G5 Action 1: playerctl PlayPause (Playing -> Paused)
    const beforeMprisPause = (await fetchSnapshot()).player.playback;
    const mprisPauseCmd = await execText("playerctl", ["-p", "ytm_glass", "play-pause"]);
    await waitUntil(
      "MPRIS PlayPause -> paused",
      () => latestPlayer?.playback === "paused",
      10_000
    );
    const afterMprisPause = (await fetchSnapshot()).player.playback;

    // G5 Action 2: playerctl PlayPause (Paused -> Playing)
    const mprisResumeCmd = await execText("playerctl", ["-p", "ytm_glass", "play-pause"]);
    await waitUntil(
      "MPRIS PlayPause -> playing",
      () => latestPlayer?.playback === "playing",
      10_000
    );
    const afterMprisResume = (await fetchSnapshot()).player.playback;

    results.g5 = {
      status:
        mprisWhilePlaying.singleExpectedService &&
        beforeMprisPause === "playing" &&
        afterMprisPause === "paused" &&
        afterMprisResume === "playing"
          ? "PASS_TARGET"
          : "FAIL",
      busCheck: mprisWhilePlaying,
      playPauseAction1: {
        cmdOk: mprisPauseCmd.ok,
        before: beforeMprisPause,
        after: afterMprisPause,
      },
      playPauseAction2: {
        cmdOk: mprisResumeCmd.ok,
        before: afterMprisPause,
        after: afterMprisResume,
      },
    };

    // G3 Core Controls via ui.playerCommand
    const seekTargetMs = 45_000;
    const seekRes = await runPlayerCommand(
      "seek",
      { positionMs: seekTargetMs },
      "seek(45000ms)",
      (rec) =>
        rec.status === "observed" &&
        typeof latestPlayer?.timeAnchor?.positionMs === "number" &&
        latestPlayer.timeAnchor.positionMs >= 35_000
    );

    const vol1Res = await runPlayerCommand(
      "setVolume",
      { volume: 0.45 },
      "setVolume(0.45)",
      (rec) =>
        rec.status === "observed" &&
        typeof latestPlayer?.volume === "number" &&
        Math.abs(latestPlayer.volume - 0.45) < 0.08
    );

    const vol2Res = await runPlayerCommand(
      "setVolume",
      { volume: 0.75 },
      "setVolume(0.75)",
      (rec) =>
        rec.status === "observed" &&
        typeof latestPlayer?.volume === "number" &&
        Math.abs(latestPlayer.volume - 0.75) < 0.08
    );

    const pauseRes = await runPlayerCommand(
      "pause",
      null,
      "pause",
      (rec) => rec.status === "observed" && latestPlayer?.playback === "paused"
    );

    const playRes = await runPlayerCommand(
      "play",
      null,
      "play",
      (rec) => rec.status === "observed" && latestPlayer?.playback === "playing"
    );

    const inspectPlayerNextState = async () => {
      try {
        return await playerWin.webContents.mainFrame.executeJavaScript(
          `(() => {
            const urlObj = new URL(location.href);
            const nextBtn = document.querySelector("ytmusic-player-bar .next-button");
            const prevBtn = document.querySelector("ytmusic-player-bar .previous-button");
            const api = document.querySelector("#movie_player");
            const queueLen = document.querySelectorAll("ytmusic-player-queue-item").length;
            let playlistLen = null;
            try {
              playlistLen = typeof api?.getPlaylist === "function" ? (api.getPlaylist()?.length ?? null) : null;
            } catch {}
            return {
              pathname: urlObj.pathname,
              hasVideoParam: urlObj.searchParams.has("v"),
              hasListParam: urlObj.searchParams.has("list"),
              queueItemCount: queueLen,
              playerApiPlaylistLen: playlistLen,
              nextBtnFound: Boolean(nextBtn),
              nextBtnDisabled: nextBtn
                ? Boolean(nextBtn.disabled || nextBtn.getAttribute("aria-disabled") === "true" || nextBtn.hasAttribute("disabled"))
                : null,
              prevBtnFound: Boolean(prevBtn),
              prevBtnDisabled: prevBtn
                ? Boolean(prevBtn.disabled || prevBtn.getAttribute("aria-disabled") === "true" || prevBtn.hasAttribute("disabled"))
                : null,
              hasApiNext: typeof api?.nextVideo === "function",
              hasApiPrev: typeof api?.previousVideo === "function",
            };
          })();`,
          true
        );
      } catch (err) {
        return { error: err instanceof Error ? err.message : String(err) };
      }
    };

    // Wait up to 5s for multi-track queue / enabled next button
    for (let i = 0; i < 10; i += 1) {
      const diag = await inspectPlayerNextState();
      if (diag && !diag.error && (diag.nextBtnDisabled === false || diag.queueItemCount >= 2)) {
        break;
      }
      await sleep(500);
    }

    let preNextDomDiag = await inspectPlayerNextState();
    let fallbackTargetUsed = false;
    if (
      preNextDomDiag &&
      !preNextDomDiag.error &&
      preNextDomDiag.queueItemCount < 2 &&
      preNextDomDiag.nextBtnDisabled === true &&
      fallbackPlaylistTarget
    ) {
      fallbackTargetUsed = true;
      await runPlayerCommand(
        "playTarget",
        { target: fallbackPlaylistTarget },
        "playTarget(fallback multi-track playlist for queue)",
        (rec) => rec.status === "observed" && latestPlayer?.playback === "playing",
        35_000
      );
      await sleep(2500);
      preNextDomDiag = await inspectPlayerNextState();
    }

    const beforeNextSnap = await fetchSnapshot();
    const beforeNextVideoId = beforeNextSnap.player?.currentTrack?.videoId ?? null;
    const beforeNextTrackInstanceId = beforeNextSnap.player?.trackInstanceId ?? null;
    const nextRes = await runPlayerCommand(
      "next",
      null,
      "next",
      (rec) =>
        rec.status === "observed" &&
        (latestPlayer?.currentTrack?.videoId !== beforeNextVideoId ||
          latestPlayer?.trackInstanceId !== beforeNextTrackInstanceId),
      15_000
    );

    await sleep(300);
    const beforePrevSnap = await fetchSnapshot();
    const beforePrevVideoId = beforePrevSnap.player?.currentTrack?.videoId ?? null;
    const beforePrevTrackInstanceId = beforePrevSnap.player?.trackInstanceId ?? null;
    const prevRes = await runPlayerCommand(
      "previous",
      null,
      "previous",
      (rec) =>
        rec.status === "observed" &&
        (latestPlayer?.currentTrack?.videoId !== beforePrevVideoId ||
          latestPlayer?.trackInstanceId !== beforePrevTrackInstanceId ||
          (latestPlayer?.timeAnchor?.positionMs ?? 999999) < 10_000),
      15_000
    );

    // Pause cleanly at end
    await runPlayerCommand(
      "pause",
      null,
      "final-pause",
      (rec) => rec.status === "observed" && latestPlayer?.playback === "paused",
      15_000
    );

    // Redact private track identifiers if fallback private playlist was needed for queue testing
    const stepsRaw = [playTargetRes, seekRes, vol1Res, vol2Res, pauseRes, playRes, nextRes, prevRes];
    const steps = fallbackTargetUsed
      ? stepsRaw.map((s) =>
          s.op === "next" || s.op === "previous"
            ? {
                ...s,
                observedPlayer: {
                  ...s.observedPlayer,
                  videoId: s.observedPlayer.videoId ? "<redacted-playlist-track>" : null,
                },
              }
            : s
        )
      : stepsRaw;
    const allStepsObserved = steps.every((s) => s.receipt.status === "observed" && s.predicateMatched);

    const qualityStep = [playRes, seekRes, nextRes, playTargetRes].find(
      (s) => s.observedPlayer.qualityLabel && s.observedPlayer.qualityLabel !== "UNKNOWN"
    ) ?? playRes;

    results.g3Controls = {
      status: allStepsObserved ? "PASS_TARGET" : "FAIL",
      preNextDomDiagnostics: { ...preNextDomDiag, fallbackTargetUsed },
      audioSinkInputsDuringPlayback: audioWhilePlaying,
      codecAndQualityObservation: {
        codec: qualityStep.observedPlayer.codec ?? "UNKNOWN",
        audioMode: qualityStep.observedPlayer.audioMode ?? "UNKNOWN",
        qualityLabel: qualityStep.observedPlayer.qualityLabel ?? "UNKNOWN",
        drmProtected:
          qualityStep.observedPlayer.drmProtected === null ||
          qualityStep.observedPlayer.drmProtected === undefined
            ? "UNKNOWN"
            : qualityStep.observedPlayer.drmProtected,
      },
      steps,
    };
  } catch (err) {
    results.fatalError = err instanceof Error ? err.message : String(err);
  } finally {
    results.hostErrors = observed.hostErrors;
    mkdirSync(dirname(reportPath), { recursive: true });
    writeFileSync(reportPath, `${JSON.stringify(results, null, 2)}\n`, "utf8");

    try {
      await integrated.shutdown();
    } catch {
      // Ignore shutdown cleanup errors
    }
    try {
      controller?.shutdown();
    } catch {
      // Ignore shutdown cleanup errors
    }
  }

  const allOk =
    !results.fatalError &&
    results.g2.status === "PASS_TARGET" &&
    results.g4.status === "PASS_TARGET" &&
    results.g3Controls.status === "PASS_TARGET" &&
    results.g5.status === "PASS_TARGET";

  app.exit(allOk ? 0 : 1);
}

void run().catch((err) => {
  console.error("Live M0 probe crashed:", err);
  app.exit(1);
});
