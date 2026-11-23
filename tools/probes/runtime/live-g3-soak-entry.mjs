import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { app } from "electron";

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

async function sampleProcessTreeMetrics(rootPid) {
  const psRes = await execText("ps", ["-e", "-o", "pid=,ppid=,%cpu=,rss="]);
  if (!psRes.ok) {
    return { ok: false, procCount: 0, totalCpuPct: 0, totalRssKb: 0, totalPssKb: 0 };
  }

  const childrenByParent = new Map();
  const procStats = new Map();
  for (const line of psRes.stdout.split(/\r?\n/)) {
    const parts = line.trim().split(/\s+/);
    if (parts.length < 4) continue;
    const pid = Number.parseInt(parts[0], 10);
    const ppid = Number.parseInt(parts[1], 10);
    const cpu = Number.parseFloat(parts[2]);
    const rss = Number.parseInt(parts[3], 10);
    if (!Number.isFinite(pid) || !Number.isFinite(ppid)) continue;
    procStats.set(pid, {
      cpu: Number.isFinite(cpu) ? cpu : 0,
      rss: Number.isFinite(rss) ? rss : 0,
    });
    const list = childrenByParent.get(ppid);
    if (list) {
      list.push(pid);
    } else {
      childrenByParent.set(ppid, [pid]);
    }
  }

  const treePids = new Set();
  const stack = [rootPid];
  while (stack.length > 0) {
    const cur = stack.pop();
    if (treePids.has(cur)) continue;
    treePids.add(cur);
    const kids = childrenByParent.get(cur) ?? [];
    for (const k of kids) {
      stack.push(k);
    }
  }

  let totalCpuPct = 0;
  let totalRssKb = 0;
  let totalPssKb = 0;
  for (const pid of treePids) {
    const st = procStats.get(pid);
    if (st) {
      totalCpuPct += st.cpu;
      totalRssKb += st.rss;
    }
    const smapsPath = `/proc/${pid}/smaps_rollup`;
    if (existsSync(smapsPath)) {
      try {
        const txt = readFileSync(smapsPath, "utf8");
        const pssMatch = /^Pss:\s+(\d+)\s+kB/m.exec(txt);
        if (pssMatch) {
          totalPssKb += Number.parseInt(pssMatch[1], 10);
        }
      } catch {
        // Process may have exited between ps and smaps_rollup read
      }
    }
  }

  return {
    ok: true,
    treePids: Array.from(treePids).sort((a, b) => a - b),
    procCount: treePids.size,
    totalCpuPct: Math.round(totalCpuPct * 10) / 10,
    totalRssKb,
    totalPssKb,
  };
}

async function checkPipeWireActive(treePidSet) {
  const res = await execText("pactl", ["list", "sink-inputs"]);
  if (!res.ok) {
    return { ok: false, activeSinkInput: false, matchingStreams: 0 };
  }
  const blocks = res.stdout
    .split(/(?=^Sink Input #\d+)/m)
    .filter((b) => b.trim().startsWith("Sink Input #"));

  let active = false;
  let matchingStreams = 0;
  for (const block of blocks) {
    const appNameMatch = block.match(/^\s*application\.name\s*=\s*"([^"]*)"/m);
    const pidMatch = block.match(/^\s*application\.process\.id\s*=\s*"([^"]*)"/m);
    const corkedMatch = block.match(/^\s*Corked:\s*(\S+)/m);
    const appName = appNameMatch ? appNameMatch[1] : "";
    const pid = pidMatch ? Number.parseInt(pidMatch[1], 10) : -1;
    const isOurs = appName === "YTMcp" || (pid > 0 && treePidSet.has(pid));
    if (isOurs) {
      matchingStreams += 1;
      if ((corkedMatch?.[1] ?? "").toLowerCase() === "no") {
        active = true;
      }
    }
  }
  return { ok: true, activeSinkInput: active, matchingStreams };
}

async function run() {
  if (process.env.YTMCP_ALLOW_LIVE !== "1") {
    throw new Error("Refusing to run live G3 soak without YTMCP_ALLOW_LIVE=1");
  }

  const soakSecondsRaw = Number.parseInt(process.env.YTMCP_SOAK_SECONDS ?? "1800", 10);
  const soakSeconds = Number.isFinite(soakSecondsRaw) && soakSecondsRaw > 0 ? soakSecondsRaw : 1800;
  const sampleIntervalSec = soakSeconds <= 60 ? 5 : 30;
  const runScheduledChecks = soakSeconds >= 120;

  const bootstrapModuleUrl = pathToFileURL(resolve(process.env.YTMCP_M0_MAIN_MODULE)).href;
  const entryModuleUrl = pathToFileURL(resolve(process.env.YTMCP_M0_ENTRY_MODULE)).href;
  const reportPath = resolve(process.env.YTMCP_SOAK_REPORT_PATH);
  const samplesPath = resolve(process.env.YTMCP_SOAK_SAMPLES_PATH);
  const commitSha = process.env.YTMCP_COMMIT_SHA ?? "unknown";

  mkdirSync(dirname(reportPath), { recursive: true });
  mkdirSync(dirname(samplesPath), { recursive: true });
  writeFileSync(samplesPath, "", "utf8");

  const { startDesktopShell } = await import(bootstrapModuleUrl);
  const { createIntegratedDesktopHooks } = await import(entryModuleUrl);

  let seqCounter = 2000;
  let latestSnapshot = null;
  let latestPlayer = null;
  let latestAuth = null;
  let loggedInBridgeSeen = false;
  const commandReceipts = new Map();
  const pendingRequests = new Map();
  const hostErrors = [];
  const anomalies = [];

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
    if (check()) return Promise.resolve();
    return new Promise((resolvePromise, rejectPromise) => {
      const timer = setTimeout(() => {
        const idx = waiters.indexOf(entry);
        if (idx !== -1) waiters.splice(idx, 1);
        rejectPromise(new Error(`Timed out after ${timeoutMs}ms waiting for: ${label}`));
      }, timeoutMs);
      const entry = { check, resolve: resolvePromise, reject: rejectPromise, timer };
      waiters.push(entry);
    });
  };

  const integrated = createIntegratedDesktopHooks({
    daemonBin: process.env.YTMCP_DAEMON_BIN,
    clientOptions: {
      // Errors for host.sessionMaterial (e.g. a failed identity read) never reach onUiEnvelope.
      onSessionMaterialError: (err) => {
        hostErrors.push({ ts: Date.now(), code: err.code, safeMessage: err.safeMessage, source: "sessionMaterial" });
      },
      onUiEnvelope: (env) => {
        if (env.kind === "core.snapshot") {
          latestSnapshot = env.payload;
          latestPlayer = latestSnapshot.player;
          latestAuth = latestSnapshot.authState;
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
            commandReceipts.set(d.receipt.commandId, d.receipt);
          } else if (d.deltaType === "networkChanged" && latestSnapshot) {
            latestSnapshot.networkState = d.network_state;
          } else if (d.deltaType === "queueReplaced" && latestSnapshot) {
            latestSnapshot.queueSummary = d.queue_summary;
          }
        } else if (env.kind === "core.commandReceipt") {
          commandReceipts.set(env.payload.commandId, env.payload);
        } else if (env.kind === "core.catalogResponse" && env.requestId) {
          const p = pendingRequests.get(env.requestId);
          if (p) {
            pendingRequests.delete(env.requestId);
            p.resolve({ ok: true, payload: env.payload });
          }
        } else if (env.kind === "core.error") {
          if (env.requestId && pendingRequests.has(env.requestId)) {
            const p = pendingRequests.get(env.requestId);
            pendingRequests.delete(env.requestId);
            p.resolve({ ok: false, error: env.payload });
          } else {
            hostErrors.push({
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

  const origBridgeEnvelope = integrated.hooks.onBridgeEnvelope;
  integrated.hooks.onBridgeEnvelope = async (envelope) => {
    if (envelope.kind === "bridge.ready" && envelope.payload?.loggedInObserved === true) {
      loggedInBridgeSeen = true;
    }
    await origBridgeEnvelope?.(envelope);
    notifyWaiters();
  };

  const fetchSnapshot = async () => {
    const requestId = `soak-snap-${randomUUID()}`;
    const prevRev = latestSnapshot?.revision ?? -1;
    seqCounter += 1;
    integrated.rustHost.sendEnvelope({
      protocolVersion: 1,
      kind: "ui.subscribeSnapshot",
      coreEpoch: integrated.rustHost.getCoreEpoch(),
      accountEpoch: null,
      bridgeEpoch: null,
      requestId,
      seq: seqCounter,
      payload: {},
    });
    await waitUntil(
      "snapshot response",
      () => latestSnapshot !== null && (latestSnapshot.revision !== prevRev || prevRev >= 0),
      10_000
    );
    await sleep(100);
    return latestSnapshot;
  };

  const sendCatalogQuery = (req, timeoutMs = 20_000) => {
    const requestId = `soak-cat-${randomUUID()}`;
    return new Promise((resolvePromise, rejectPromise) => {
      const timer = setTimeout(() => {
        pendingRequests.delete(requestId);
        rejectPromise(new Error(`Timed out catalog query ${req.kind}`));
      }, timeoutMs);
      pendingRequests.set(requestId, {
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
      integrated.rustHost.sendEnvelope({
        protocolVersion: 1,
        kind: "ui.catalogQuery",
        coreEpoch: integrated.rustHost.getCoreEpoch(),
        accountEpoch: latestSnapshot?.accountEpoch ?? null,
        bridgeEpoch: latestPlayer?.bridgeEpoch ?? null,
        requestId,
        seq: seqCounter,
        payload: req,
      });
    });
  };

  const sendCommand = async (op, args, timeoutMs = 20_000) => {
    const commandId = `soak-${op}-${randomUUID()}`;
    seqCounter += 1;
    integrated.rustHost.sendEnvelope({
      protocolVersion: 1,
      kind: "ui.playerCommand",
      coreEpoch: integrated.rustHost.getCoreEpoch(),
      accountEpoch: latestSnapshot?.accountEpoch ?? null,
      bridgeEpoch: latestPlayer?.bridgeEpoch ?? null,
      requestId: `req-${commandId}`,
      seq: seqCounter,
      payload: { commandId, op, args: args ?? null },
    });
    await waitUntil(
      `command ${op} terminal receipt`,
      () => {
        const rec = commandReceipts.get(commandId);
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
    return commandReceipts.get(commandId);
  };

  let controller = null;
  const startTimeIso = new Date().toISOString();
  const startMs = Date.now();
  const summary = {
    probe: "M0-G3-hidden-soak",
    commitSha,
    targetDurationSec: soakSeconds,
    sampleIntervalSec,
    startTimeIso,
    endTimeIso: null,
    elapsedSec: 0,
    status: "FAIL",
    playerWindowShownInitially: false,
    manualDesktopEventsNotAutomated: ["workspace_switch", "screen_lock"],
    trackTransitions: 0,
    observedVideoIds: [],
    samplesCount: 0,
    pipewireActiveSamples: 0,
    pipewireMissingDuringPlaybackSamples: 0,
    scheduledChecks: {
      uiRestoreCheck: { executed: false, ok: false, detail: null },
      offlineBufferCheck: { executed: false, ok: false, detail: null },
      pauseResumeCheck: { executed: false, ok: false, pausedProcessMetrics: null },
    },
    processMetricsSummary: {
      minRssKb: null,
      maxRssKb: null,
      minPssKb: null,
      maxPssKb: null,
      avgCpuPct: null,
    },
    anomalies,
    hostErrorsCount: 0,
  };

  try {
    controller = await startDesktopShell(integrated.hooks);
    if (!controller) {
      throw new Error("Failed to acquire single-instance lock for G3 soak");
    }
    app.on("will-quit", () => {
      void integrated.shutdown();
    });
    await integrated.attachController(controller);

    const mainWin = controller.windowManager.getMainWindow();
    const playerWin = controller.windowManager.getPlayerWindow();
    summary.playerWindowShownInitially = Boolean(playerWin && playerWin.isVisible());
    if (summary.playerWindowShownInitially) {
      throw new Error("Player window must remain hidden during G3 hidden soak");
    }

    await fetchSnapshot();
    await waitUntil(
      "authenticated session + bridge.ready(loggedInObserved=true)",
      () => latestAuth?.state === "authenticated" && loggedInBridgeSeen,
      35_000
    );

    const searchResp = await sendCatalogQuery({
      kind: "searchSongs",
      query: "Daft Punk",
      limit: 10,
    });
    const track =
      searchResp.ok && searchResp.payload.resultType === "searchSongs"
        ? searchResp.payload.songs.items.find((t) => t.availability === "available") ??
          searchResp.payload.songs.items[0]
        : null;
    if (!track) {
      throw new Error("Failed to acquire public search track for G3 soak");
    }

    const playRec = await sendCommand(
      "playTarget",
      { target: { videoId: track.videoId, playlistId: `RDAMVM${track.videoId}` } },
      35_000
    );
    if (playRec?.status !== "observed") {
      throw new Error(`playTarget failed: ${JSON.stringify(playRec)}`);
    }
    await waitUntil("player playback=playing", () => latestPlayer?.playback === "playing", 20_000);
    await sendCommand("setVolume", { volume: 0.2 }, 15_000);

    // Hide main window to run genuinely hidden + minimized playback
    if (mainWin && !mainWin.isDestroyed()) {
      mainWin.hide();
    }
    await sleep(1500);

    const seenVideoIds = [];
    let lastTrackKey = `${latestPlayer?.currentTrack?.videoId ?? ""}::${latestPlayer?.trackInstanceId ?? ""}`;
    if (latestPlayer?.currentTrack?.videoId) {
      seenVideoIds.push(latestPlayer.currentTrack.videoId);
    }

    const soakLoopStartMs = Date.now();
    const targetEndMs = soakLoopStartMs + soakSeconds * 1000;
    const cpuSamples = [];
    const rssSamples = [];
    const pssSamples = [];

    const recordSample = async (phaseLabel) => {
      const snap = await fetchSnapshot();
      const proc = await sampleProcessTreeMetrics(process.pid);
      const pw = await checkPipeWireActive(new Set(proc.treePids ?? []));
      const elapsedSec = Math.round((Date.now() - soakLoopStartMs) / 1000);
      const curVideoId = snap.player?.currentTrack?.videoId ?? null;
      const curInstId = snap.player?.trackInstanceId ?? null;
      const trackKey = `${curVideoId ?? ""}::${curInstId ?? ""}`;
      if (curVideoId && !seenVideoIds.includes(curVideoId)) {
        seenVideoIds.push(curVideoId);
      }
      if (trackKey !== "::" && trackKey !== lastTrackKey) {
        summary.trackTransitions += 1;
        lastTrackKey = trackKey;
      }

      if (proc.ok) {
        cpuSamples.push(proc.totalCpuPct);
        rssSamples.push(proc.totalRssKb);
        pssSamples.push(proc.totalPssKb);
      }
      summary.samplesCount += 1;
      if (pw.activeSinkInput) {
        summary.pipewireActiveSamples += 1;
      } else if (snap.player?.playback === "playing" && phaseLabel === "playing") {
        summary.pipewireMissingDuringPlaybackSamples += 1;
        anomalies.push({
          elapsedSec,
          kind: "missing_pipewire_sink_input",
          playback: snap.player?.playback ?? null,
        });
      }

      if (phaseLabel === "playing" && snap.player?.playback !== "playing") {
        anomalies.push({
          elapsedSec,
          kind: "unexpected_non_playing_state",
          playback: snap.player?.playback ?? null,
        });
      }

      const sampleObj = {
        ts: new Date().toISOString(),
        elapsedSec,
        phase: phaseLabel,
        playback: snap.player?.playback ?? null,
        positionMs: snap.player?.timeAnchor?.positionMs ?? null,
        durationMs: snap.player?.timeAnchor?.durationMs ?? null,
        videoId: curVideoId,
        trackInstanceId: curInstId,
        queueLoadedCount: snap.queueSummary?.loadedCount ?? 0,
        networkState: snap.networkState ?? null,
        mainVisible: Boolean(mainWin && !mainWin.isDestroyed() && mainWin.isVisible()),
        playerVisible: Boolean(playerWin && !playerWin.isDestroyed() && playerWin.isVisible()),
        pipewireActiveSinkInput: pw.activeSinkInput,
        pipewireMatchingStreams: pw.matchingStreams,
        procCount: proc.procCount,
        totalCpuPct: proc.totalCpuPct,
        totalRssKb: proc.totalRssKb,
        totalPssKb: proc.totalPssKb,
      };
      appendFileSync(samplesPath, `${JSON.stringify(sampleObj)}\n`, "utf8");
      return sampleObj;
    };

    // Initial sample
    await recordSample("playing");

    const restoreAtSec = 300;
    const offlineAtSec = 600;
    const pauseAtSec = 900;

    while (Date.now() < targetEndMs) {
      const remainingMs = targetEndMs - Date.now();
      await sleep(Math.min(sampleIntervalSec * 1000, Math.max(250, remainingMs)));
      const elapsedSec = Math.round((Date.now() - soakLoopStartMs) / 1000);

      if (
        runScheduledChecks &&
        !summary.scheduledChecks.uiRestoreCheck.executed &&
        elapsedSec >= restoreAtSec
      ) {
        summary.scheduledChecks.uiRestoreCheck.executed = true;
        if (mainWin && !mainWin.isDestroyed()) {
          mainWin.show();
        }
        await sleep(500);
        const restored = await fetchSnapshot();
        const ok =
          Boolean(restored.player?.currentTrack?.videoId) &&
          restored.player?.playback === "playing" &&
          (restored.queueSummary?.loadedCount ?? 0) > 0;
        summary.scheduledChecks.uiRestoreCheck.ok = ok;
        summary.scheduledChecks.uiRestoreCheck.detail = {
          playback: restored.player?.playback ?? null,
          positionMs: restored.player?.timeAnchor?.positionMs ?? null,
          queueLoadedCount: restored.queueSummary?.loadedCount ?? 0,
        };
        if (mainWin && !mainWin.isDestroyed()) {
          mainWin.hide();
        }
      }

      if (
        runScheduledChecks &&
        !summary.scheduledChecks.offlineBufferCheck.executed &&
        elapsedSec >= offlineAtSec
      ) {
        summary.scheduledChecks.offlineBufferCheck.executed = true;
        const beforeSnap = await fetchSnapshot();
        const beforePos = beforeSnap.player?.timeAnchor?.positionMs ?? 0;
        integrated.rustHost.sendNetworkChanged("offline");
        await sleep(10_000);
        const midSnap = await fetchSnapshot();
        const midPos = midSnap.player?.timeAnchor?.positionMs ?? 0;
        const stayedPlaying =
          midSnap.player?.playback === "playing" &&
          midSnap.networkState === "offline" &&
          (midPos > beforePos || midSnap.player?.currentTrack?.videoId !== beforeSnap.player?.currentTrack?.videoId);
        integrated.rustHost.sendNetworkChanged("online");
        await sleep(1000);
        summary.scheduledChecks.offlineBufferCheck.ok = stayedPlaying;
        summary.scheduledChecks.offlineBufferCheck.detail = {
          beforePosMs: beforePos,
          after10sOfflinePosMs: midPos,
          observedPlaybackWhileOffline: midSnap.player?.playback ?? null,
          networkState: midSnap.networkState ?? null,
        };
        if (!stayedPlaying) {
          anomalies.push({ elapsedSec, kind: "offline_buffer_check_failed" });
        }
      }

      if (
        runScheduledChecks &&
        !summary.scheduledChecks.pauseResumeCheck.executed &&
        elapsedSec >= pauseAtSec
      ) {
        summary.scheduledChecks.pauseResumeCheck.executed = true;
        const pRec = await sendCommand("pause", null, 15_000);
        await sleep(10_000);
        const pausedSample = await recordSample("paused_hidden");
        const rRec = await sendCommand("play", null, 15_000);
        await sleep(1500);
        const resumedSample = await recordSample("playing");
        summary.scheduledChecks.pauseResumeCheck.ok =
          pRec?.status === "observed" &&
          rRec?.status === "observed" &&
          pausedSample.playback === "paused" &&
          resumedSample.playback === "playing" &&
          resumedSample.pipewireActiveSinkInput;
        summary.scheduledChecks.pauseResumeCheck.pausedProcessMetrics = {
          procCount: pausedSample.procCount,
          totalCpuPct: pausedSample.totalCpuPct,
          totalRssKb: pausedSample.totalRssKb,
          totalPssKb: pausedSample.totalPssKb,
        };
      } else {
        await recordSample("playing");
      }
    }

    await sendCommand("pause", null, 15_000).catch(() => {});

    summary.observedVideoIds = seenVideoIds;
    summary.processMetricsSummary = {
      minRssKb: rssSamples.length ? Math.min(...rssSamples) : null,
      maxRssKb: rssSamples.length ? Math.max(...rssSamples) : null,
      minPssKb: pssSamples.length ? Math.min(...pssSamples) : null,
      maxPssKb: pssSamples.length ? Math.max(...pssSamples) : null,
      avgCpuPct: cpuSamples.length
        ? Math.round((cpuSamples.reduce((a, b) => a + b, 0) / cpuSamples.length) * 10) / 10
        : null,
    };

    const scheduledOk =
      !runScheduledChecks ||
      (summary.scheduledChecks.uiRestoreCheck.ok &&
        summary.scheduledChecks.offlineBufferCheck.ok &&
        summary.scheduledChecks.pauseResumeCheck.ok);

    summary.status =
      !summary.playerWindowShownInitially &&
      summary.pipewireActiveSamples >= 1 &&
      summary.pipewireMissingDuringPlaybackSamples === 0 &&
      anomalies.length === 0 &&
      scheduledOk
        ? "PASS_TARGET"
        : "FAIL";
  } catch (err) {
    summary.fatalError = err instanceof Error ? err.message : String(err);
    summary.status = "FAIL";
  } finally {
    summary.endTimeIso = new Date().toISOString();
    summary.elapsedSec = Math.round((Date.now() - startMs) / 1000);
    summary.hostErrorsCount = hostErrors.length;
    writeFileSync(reportPath, `${JSON.stringify(summary, null, 2)}\n`, "utf8");
    try {
      await integrated.shutdown();
    } catch {
      // Ignore shutdown errors
    }
    try {
      controller?.shutdown();
    } catch {
      // Ignore shutdown errors
    }
  }

  app.exit(summary.status === "PASS_TARGET" ? 0 : 1);
}

void run().catch((err) => {
  console.error("Live G3 soak failed:", err);
  app.exit(1);
});
