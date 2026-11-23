#!/usr/bin/env node
// Optional R10 evidence through the real MPRIS and StatusNotifierItem session-bus APIs.
// YTMCP_ALLOW_LIVE=1 node tools/qa/v1-desktop.mjs
import { spawn } from "node:child_process";
import { accessSync, constants, realpathSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { AppInstance, ELECTRON, REPO, waitAppReady } from "./lib/app.mjs";
import { check, gateDoc, writeEvidence } from "./lib/evidence.mjs";
import { ensureNullSink, fileSha256, mprisProps, NULL_SINK, run, sinkInputsFor, sleep, unloadNullSink } from "./lib/sys.mjs";
import { quitViaMpris } from "./gates/g2.mjs";
import { pageMedia, uiSearch } from "./gates/common.mjs";

if (process.env.YTMCP_ALLOW_LIVE !== "1") {
  console.error("Refusing live desktop acceptance without YTMCP_ALLOW_LIVE=1");
  process.exit(2);
}

const ROOT = "org.mpris.MediaPlayer2";
const PLAYER = `${ROOT}.Player`;
const SERVICE = `${ROOT}.ytmcp`;
const MPRIS_PATH = "/org/mpris/MediaPlayer2";
const RESOURCES = "/opt/ytmcp/resources";
const MAIN = `${RESOURCES}/app/dist/main.mjs`;
const DAEMON = `${RESOURCES}/bin/ytm-daemon`;
const SEEK_TOLERANCE_US = 2_000_000;
const WATCHER = "org.kde.StatusNotifierWatcher";

async function dbus(args, json = false) {
  const res = await run("busctl", ["--user", "--timeout=5", ...(json ? ["-j"] : []), ...args], { timeoutMs: 6000 });
  if (!json) return res.ok;
  return res.ok ? JSON.parse(res.stdout).data : null;
}

const property = (name, iface = PLAYER) => dbus(["get-property", SERVICE, MPRIS_PATH, iface, name], true);
const mprisCall = (method, args = [], iface = PLAYER) => dbus(["call", SERVICE, MPRIS_PATH, iface, method, ...args]);

async function activeNullStream(app) {
  return (await sinkInputsFor(app.pids())).entries.some((input) => input.sink === NULL_SINK && !input.corked);
}

function coverFacts(raw) {
  const facts = { present: typeof raw === "string" && raw.length > 0, type: null, containedFile: null };
  if (!facts.present) return facts;
  try {
    const url = new URL(raw);
    facts.type = ["file:", "https:"].includes(url.protocol) ? url.protocol.slice(0, -1) : "other";
    if (facts.type === "file") {
      const home = process.env.XDG_CACHE_HOME || join(homedir(), ".cache");
      const cache = join(home, "ytmcp", "covers");
      const file = fileURLToPath(url);
      facts.containedFile = false;
      if (file.startsWith(cache + sep)) {
        const realFile = realpathSync(file);
        if (realFile.startsWith(realpathSync(cache) + sep) && statSync(realFile).isFile()) {
          accessSync(realFile, constants.R_OK);
          facts.containedFile = true;
        }
      }
    }
  } catch {
    facts.containedFile = false;
  }
  return facts;
}

async function metadataCheck(app) {
  const result = await app.waitFor(async () => {
    const cover = coverFacts((await property("Metadata"))?.["mpris:artUrl"]?.data);
    return cover.containedFile === true;
  }, { timeoutMs: 8000 });
  const [md, identity, desktopEntry, page] = await Promise.all([
    property("Metadata"), property("Identity", ROOT), property("DesktopEntry", ROOT), pageMedia(app),
  ]);
  const trackId = md?.["mpris:trackid"]?.data;
  const lengthUs = md?.["mpris:length"]?.data ?? null;
  const validTrackPath = typeof trackId === "string" && /^\/(?:[A-Za-z0-9_]+\/)*[A-Za-z0-9_]+$/.test(trackId) && !trackId.endsWith("/NoTrack");
  const cover = coverFacts(md?.["mpris:artUrl"]?.data);
  const measured = {
    validTrackPath,
    hasTitle: Boolean(md?.["xesam:title"]?.data),
    lengthUs,
    pageLengthUs: page.d == null ? null : Math.round(page.d * 1e6),
    identityMatches: identity === "YTMcp" && desktopEntry === "local.ytmcp.Desktop",
    cover,
    coverCachedWithinWait: result.ok,
  };
  return [
    check("R10.metadata", "MPRIS exports the packaged identity, valid track object path and accurate microsecond duration", {
      measured,
      pass: validTrackPath && measured.hasTitle && measured.identityMatches &&
        Number.isSafeInteger(lengthUs) && lengthUs > 0 && measured.pageLengthUs !== null &&
        Math.abs(lengthUs - measured.pageLengthUs) <= SEEK_TOLERANCE_US,
    }),
    check("R10.cover", "MPRIS publishes a readable file inside the cover cache", {
      measured: cover, pass: cover.containedFile === true,
    }),
  ];
}

async function playPauseCheck(app, method, paused) {
  const before = await pageMedia(app);
  const accepted = await mprisCall(method);
  const expected = paused ? "Paused" : "Playing";
  const change = await app.waitFor(async (s) => {
    const page = await pageMedia(app);
    return page.present && page.videoId === before.videoId && page.paused === paused &&
      s?.player?.playback === expected.toLowerCase() && await property("PlaybackStatus") === expected;
  }, { timeoutMs: 12_000 });
  const sample1 = await pageMedia(app);
  await sleep(1000);
  const sample2 = await pageMedia(app);
  const actual = sample2.t - sample1.t;
  const streamOnNull = await activeNullStream(app);
  return check(`R10.${method}`, `MPRIS ${method} reaches the same core and the official player`, {
    measured: { accepted, observedState: change.ok, pagePausedBefore: before.paused, positionChangeSeconds: actual, streamOnNull },
    pass: accepted && change.ok && before.paused === !paused && sample1.videoId === sample2.videoId &&
      (paused ? Math.abs(actual) < 0.3 : actual > 0.5 && streamOnNull),
  });
}

function seekSignalMonitor() {
  const child = spawn("gdbus", ["monitor", "--session", "--dest", SERVICE, "--object-path", MPRIS_PATH], {
    env: { ...process.env, LC_ALL: "C" }, stdio: ["ignore", "pipe", "ignore"],
  });
  let tail = "";
  let monitoring = false;
  const positions = [];
  child.stdout.setEncoding("utf8").on("data", (chunk) => {
    const lines = (tail + chunk).split("\n");
    tail = lines.pop().slice(-512);
    for (const line of lines) {
      if (line.startsWith(`The name ${SERVICE} is owned by `)) monitoring = true;
      const match = /org\.mpris\.MediaPlayer2\.Player\.Seeked\s+\((?:int64\s+)?(-?\d+),/.exec(line);
      if (match) positions.push(Number(match[1]));
    }
  });
  child.once("error", () => { monitoring = false; });
  child.once("exit", () => { monitoring = false; });
  return { child, positions, ready: () => monitoring };
}

async function seekCheck(app, monitor, method, forcedTargetUs = null) {
  const before = await mprisProps();
  const pageBefore = await pageMedia(app);
  const offsetUs = 7_000_000;
  const targetUs = forcedTargetUs ?? before.positionUs + offsetUs;
  if (!before.trackId || !Number.isSafeInteger(before.positionUs) || pageBefore.d == null || targetUs / 1e6 + 5 >= pageBefore.d) {
    return check(`R10.${method}`, `MPRIS ${method} applies a bounded target on the current track`, {
      pass: false, notRun: "no current track with sufficient duration for a distinct seek",
    });
  }
  const startIndex = monitor.positions.length;
  const args = method === "Seek" ? ["x", String(offsetUs)] : ["ox", before.trackId, String(targetUs)];
  const accepted = await mprisCall(method, args);
  const observed = await app.waitFor(async () => {
    const [next, page] = await Promise.all([mprisProps(), pageMedia(app)]);
    return next.trackId === before.trackId && page.videoId === pageBefore.videoId &&
      Math.abs(next.positionUs - targetUs) <= SEEK_TOLERANCE_US &&
      Math.abs(page.t * 1e6 - targetUs) <= SEEK_TOLERANCE_US
      ? { positionUs: next.positionUs, pagePositionUs: Math.round(page.t * 1e6) } : false;
  }, { timeoutMs: 12_000 });
  const signal = await app.waitFor(() => {
    const positions = monitor.positions.slice(startIndex);
    return positions.find((pos) => Math.abs(pos - targetUs) <= SEEK_TOLERANCE_US) ?? false;
  }, { timeoutMs: 3000 });
  return check(`R10.${method}`, `MPRIS ${method} updates the same track and emits a Seeked signal in microseconds`, {
    measured: { accepted, targetUs, observed: observed.value, monitorReady: monitor.ready(), seekedPositionUs: signal.value },
    pass: accepted && observed.ok && signal.ok,
    blocked: monitor.ready() ? null : "D-Bus Seeked monitor did not subscribe",
  });
}

async function skipChecks(app) {
  const checks = [];
  const before = await pageMedia(app);
  const canPrevious = await property("CanGoPrevious");
  const previous = canPrevious && before.t >= 5 ? await mprisCall("Previous") : false;
  const restart = previous && (await app.waitFor(async () => {
    const page = await pageMedia(app);
    return page.videoId === before.videoId && page.t < 3;
  }, { timeoutMs: 10_000 })).ok;
  checks.push(check("R10.Previous", "MPRIS Previous restarts the current track from beyond three seconds", {
    measured: { canPrevious, startedSeconds: before.t, accepted: previous, restarted: restart },
    pass: restart, notRun: canPrevious && before.t >= 5 ? null : "no previous capability or unsuitable start position",
  }));
  const [canNext, props] = await Promise.all([property("CanGoNext"), mprisProps()]);
  const next = canNext ? await mprisCall("Next") : false;
  const advance = next && (await app.waitFor(async () => {
    const [page, current] = await Promise.all([pageMedia(app), mprisProps()]);
    return page.videoId && page.videoId !== before.videoId && page.paused === false &&
      current.trackId && current.trackId !== props.trackId;
  }, { timeoutMs: 14_000 })).ok;
  checks.push(check("R10.Next", "MPRIS Next changes the official page track and MPRIS track object path", {
    measured: { canNext, accepted: next, advanced: advance },
    pass: advance, notRun: canNext ? null : "this public radio queue has no next capability",
  }));
  return checks;
}

const mainWindow = async (app) => (await app.call("windows")).find((win) => win.role === "main");
const visibleAgain = (app, id) => app.waitFor(async () => {
  const win = await mainWindow(app);
  return win?.id === id && win.visible === true;
}, { timeoutMs: 6000 });

async function desktopChecks(app) {
  const win = await mainWindow(app);
  await app.call("winop", { target: "main", op: "hide" });
  const hidden = (await mainWindow(app))?.visible === false;
  const raised = await mprisCall("Raise", [], ROOT);
  const raisedWindow = await visibleAgain(app, win?.id);
  const checks = [check("R10.Raise", "MPRIS Raise shows the same hidden main window", {
    measured: { hidden, accepted: raised, sameWindowVisible: raisedWindow.ok },
    pass: hidden && raised && raisedWindow.ok,
  })];
  const snap = await app.snapshot();
  const items = await dbus(["get-property", WATCHER, "/StatusNotifierWatcher", WATCHER, "RegisteredStatusNotifierItems"], true);
  let item = null;
  for (const registration of items ?? []) {
    const slash = registration.indexOf("/");
    const name = slash < 0 ? registration : registration.slice(0, slash);
    const path = slash < 0 ? "/StatusNotifierItem" : registration.slice(slash);
    const pid = await dbus(["call", "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetConnectionUnixProcessID", "s", name], true);
    if (pid?.[0] === app.mainPid) {
      item = { name, path };
      break;
    }
  }
  const closeToTray = snap?.settings?.closeToTray === true;
  const trayAvailable = snap?.hostHealth?.trayAvailable === true;
  let closedToTray = false;
  let activated = false;
  let trayReshown = false;
  if (closeToTray && trayAvailable && item) {
    await app.call("winop", { target: "main", op: "close" });
    const closed = await mainWindow(app);
    closedToTray = closed?.id === win?.id && closed?.visible === false;
    if (closedToTray) {
      activated = await dbus(["call", item.name, item.path, "org.kde.StatusNotifierItem", "Activate", "ii", "0", "0"]);
      trayReshown = (await visibleAgain(app, win?.id)).ok;
    }
  }
  checks.push(check("R10.trayActivation", "close to tray hides the main window; its registered KDE tray item shows it again", {
    measured: { closeToTray, trayAvailable, ownedTrayItem: Boolean(item), closedToTray, activated, trayReshown },
    pass: closedToTray && activated && trayReshown,
    blocked: !closeToTray || !trayAvailable ? "existing close-to-tray setting or KDE tray host unavailable" : null,
    note: "Exercises StatusNotifierItem.Activate. Tray menu commands are outside this check.",
  }));
  return checks;
}

const output = join(REPO, "artifacts", "live", "v1-desktop.json");
const checks = [];
const runtimeSha256 = fileSha256(ELECTRON);
const meta = { runtimeSha256, mainSha256: fileSha256(MAIN), daemonSha256: fileSha256(DAEMON), target: "installed resources with the matching QA Electron runtime" };
let app;
let sink;
let monitor;
let step = "preflight";
try {
  if (!runtimeSha256 || runtimeSha256 !== fileSha256("/opt/ytmcp/ytmcp-player") || !meta.mainSha256 || !meta.daemonSha256) {
    checks.push(check("R10.target", "installed resources exist and the QA runtime exactly matches the package", {
      pass: false, blocked: "missing installed resources or runtime hash mismatch",
    }));
  } else {
    sink = await ensureNullSink();
    step = "authenticated launch";
    const workDir = join(process.env.XDG_RUNTIME_DIR ?? "/tmp", "ytmcp-qa", `v1-desktop-${Date.now()}`);
    app = await AppInstance.launch({ label: "v1-desktop", workDir, daemonBin: DAEMON, extraEnv: {
      YTMCP_QA_MAIN: MAIN,
      YTMCP_UI_DIST_DIR: `${RESOURCES}/ui-dist`,
      YTMCP_BRIDGE_DIST_DIR: `${RESOURCES}/bridge-dist`,
      YTMCP_IDENTITY_PATH: `${RESOURCES}/config/app-identity.json`,
    } });
    const ready = await waitAppReady(app, { timeoutMs: 90_000 });
    if (!ready.ok) {
      checks.push(check("R10.ready", "app-owned player session authenticated with a healthy bridge", {
        pass: false, blocked: "app player did not become ready",
      }));
    } else {
      step = "public-track playback";
      await uiSearch(app, "Debussy Clair de Lune");
      const play = await app.clickCommand('[data-testid="view-search"] [data-testid="virtual-track-row"] button[data-row-index="0"]', { op: "playTarget", timeoutMs: 25_000 });
      const seeded = play.finalStatus === "observed" && (await app.waitFor(() => activeNullStream(app), { timeoutMs: 6000 })).ok;
      checks.push(check("R10.seed", "play a public search result with audio on the temporary null sink", {
        measured: { observed: play.finalStatus === "observed", activeNullStream: seeded }, pass: seeded,
      }));
      if (seeded) {
        step = "MPRIS methods";
        checks.push(...await metadataCheck(app));
        checks.push(await playPauseCheck(app, "Pause", true), await playPauseCheck(app, "Play", false));
        monitor = seekSignalMonitor();
        await app.waitFor(() => monitor.ready(), { timeoutMs: 3000 });
        checks.push(await seekCheck(app, monitor, "SetPosition", 15_000_000));
        checks.push(await seekCheck(app, monitor, "Seek"));
        checks.push(...await skipChecks(app));
      }
      step = "desktop integration";
      checks.push(...await desktopChecks(app));
    }
  }
} catch {
  checks.push(check("R10.harness", "the target desktop probe completed its active step", { measured: { step }, pass: false }));
} finally {
  monitor?.child.kill();
  if (app) {
    try {
      const quit = await quitViaMpris(app);
      checks.push(check("R10.quit", "MPRIS Quit exits and leaves no app processes", {
        measured: { exited: quit.exited, leftovers: quit.leftoverPids.length },
        pass: quit.mprisQuitCall && quit.exited && quit.leftoverPids.length === 0,
      }));
    } catch {
      checks.push(check("R10.quit", "MPRIS Quit exits and leaves no app processes", { pass: false }));
    } finally {
      if (!app.exit || app.pids().length) await app.kill();
    }
  }
  if (sink?.created) await unloadNullSink(sink.moduleId);
}
const evidence = gateDoc({ gate: "R10", name: "Target machine KDE / MPRIS desktop integration", run: "V1", meta, checks });
console.log(`R10 ${evidence.result} -> ${writeEvidence(output, evidence)}`);
if (evidence.result !== "PASS_TARGET") process.exitCode = 1;
