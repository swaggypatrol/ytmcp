#!/usr/bin/env node
// Live M0 acceptance harness (T05). Drives the real desktop app through its real UI and
// writes redacted gate evidence. See docs/building.md.
//
//   YTMCP_ALLOW_LIVE=1 node tools/qa/m0.mjs --run dryrun --soak-min 5
//   YTMCP_ALLOW_LIVE=1 node tools/qa/m0.mjs --run final  --soak-min 30 --lock-screen
//
// Phases (--phases, default all): build, process, A (G0 runtime/G1/G2 part 1),
// B (G2 restart/G3/G4/G5), C (G4 failure isolation), D (G1 100% / 150% scale in a nested KWin).
import { mkdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";
import { AppInstance, DAEMON, REPO, findAppProcesses, waitAppReady } from "./lib/app.mjs";
import { startNestedKwin } from "./lib/nested.mjs";
import { STATUS, check, evidencePath, gateDoc, verdict, writeEvidence } from "./lib/evidence.mjs";
import { describeProcesses, ensureNullSink, sinkInputsFor, sleep, unloadNullSink, NULL_SINK } from "./lib/sys.mjs";
import { navigate, waitCatalog } from "./gates/common.mjs";
import { buildRelease, classifyLogs, collectEnvironment, runtimeChecks, sidecarFailureChecks } from "./gates/g0.mjs";
import {
  frameCheck,
  frameSample,
  gpuChecks,
  imeCheck,
  ipcRejectionChecks,
  nonMusicOriginCheck,
  reopenChecks,
  resizeChecks,
  sandboxChecks,
  sessionChecks,
  waylandChecks,
} from "./gates/g1.mjs";
import { accountFacts, logHygieneCheck, quitChecks, quitViaMpris, reloadChecks, restartChecks } from "./gates/g2.mjs";
import { controlChecks, mediaFacts, pausedHiddenDrop, soak, soakChecks } from "./gates/g3.mjs";
import {
  accountConsistencyCheck,
  failureIsolationChecks,
  libraryPagingCheck,
  offlineBrowseCheck,
  offlineBrowseProbe,
  playlistPagingCheck,
  searchCheck,
} from "./gates/g4.mjs";
import { g5Checks } from "./gates/g5.mjs";

const { values: opt } = parseArgs({
  options: {
    run: { type: "string", default: "dryrun" },
    phases: { type: "string", default: "build,process,A,B,C,D" },
    "soak-min": { type: "string", default: "30" },
    "lock-screen": { type: "boolean", default: false },
    query: { type: "string", default: "Debussy Clair de Lune" },
    "paused-sec": { type: "string", default: "60" },
    // Optional evidence subdirectory under artifacts/m0 for this run.
    out: { type: "string" },
  },
});

const RUN = opt.run.toUpperCase() === "FINAL" ? "FINAL" : "DRYRUN";
const PHASES = new Set(opt.phases.split(","));
if (RUN === "FINAL" && !["build", "process", "A", "B", "C", "D"].every((p) => PHASES.has(p))) {
  throw new Error("final run requires all phases (build,process,A,B,C,D)");
}
if (opt.out !== undefined && (!/^[a-zA-Z0-9][\w.-]*$/.test(opt.out) || opt.out === "FINAL")) {
  throw new Error("--out must name a subdirectory of artifacts/m0");
}
const EVIDENCE_RUN = opt.out ?? (RUN === "FINAL" ? "FINAL" : "dryrun");
const SOAK_MIN = Number(opt["soak-min"]);
if (!Number.isFinite(SOAK_MIN) || SOAK_MIN <= 0) throw new Error("--soak-min must be a positive number");
const REQUIRED_SOAK_MIN = RUN === "FINAL" ? 30 : SOAK_MIN;
if (RUN === "FINAL" && SOAK_MIN < 30) throw new Error("final run needs --soak-min >= 30 (M0_CHECKLIST G3)");
if (process.env.YTMCP_ALLOW_LIVE !== "1") {
  console.error("Refusing live M0 run without YTMCP_ALLOW_LIVE=1");
  process.exit(2);
}

const stamp = new Date().toISOString().replace(/[:.]/g, "-");
const scratch = process.env.YTMCP_QA_WORKDIR ?? join(process.env.XDG_RUNTIME_DIR ?? "/tmp", "ytmcp-qa");
const ctx = {
  run: RUN,
  workDir: join(scratch, `m0-${stamp}`),
  mockXdgRoot: join(scratch, `m0-${stamp}`, "mock-xdg"),
  gates: Object.fromEntries(["g0", "g1", "g2", "g3", "g4", "g5"].map((g) => [g, { checks: [], evidence: {} }])),
  meta: null,
  timeline: [],
};
mkdirSync(ctx.workDir, { recursive: true, mode: 0o700 });

const NAMES = {
  g0: "Environment, versions and minimal build",
  g1: "Native Wayland, GPU, sandbox, two sessions",
  g2: "Normal sign-in by the account owner and persistence across restarts",
  g3: "Real playback, controls and 30 minutes hidden",
  g4: "Basic catalog and account consistency",
  g5: "Single MPRIS service and one control action",
};

function note(msg, extra) {
  const line = { at: new Date().toISOString(), msg, ...(extra ? { extra } : {}) };
  ctx.timeline.push(line);
  console.log(`[m0 ${line.at}] ${msg}`);
}

function add(gate, checks, evidence = {}) {
  ctx.gates[gate].checks.push(...[checks].flat());
  Object.assign(ctx.gates[gate].evidence, evidence);
}

function flush() {
  const written = [];
  for (const [g, data] of Object.entries(ctx.gates)) {
    if (!data.checks.length) continue;
    const doc = gateDoc({ gate: g.toUpperCase(), name: NAMES[g], run: RUN, meta: ctx.meta, checks: data.checks, evidence: data.evidence });
    written.push(writeEvidence(evidencePath(REPO, EVIDENCE_RUN, g), doc));
  }
  const summary = {
    run: RUN,
    countsForM0: RUN === "FINAL",
    generatedAt: new Date().toISOString(),
    commit: ctx.meta?.git?.commit ?? null,
    gates: Object.fromEntries(Object.entries(ctx.gates).map(([g, d]) => [g.toUpperCase(), d.checks.length ? verdict(d.checks) : "NOT_RUN"])),
    g6: "NOT_RUN (installed package; tools/qa/g6-installed.sh)",
    failedChecks: Object.values(ctx.gates).flatMap((d) => d.checks.filter((c) => c.status !== "PASS_TARGET" && !c.support).map((c) => `${c.id}: ${c.status}`)),
    timeline: ctx.timeline,
  };
  writeEvidence(evidencePath(REPO, EVIDENCE_RUN, "summary"), summary);
  return { written, summary };
}

/** Guards a phase: an exception becomes a FAIL check instead of aborting the whole run. */
async function phase(name, gate, fn) {
  note(`phase ${name} start`);
  try {
    await fn();
  } catch (err) {
    note(`phase ${name} error: ${err?.stack ?? err}`);
    add(gate, check(`${gate.toUpperCase()}.phase${name}`, `phase ${name} completed without harness/app error`, { measured: { error: String(err?.message ?? err).slice(0, 400) }, pass: false }));
  } finally {
    flush();
  }
}

async function launch(label, extra = {}) {
  const app = await AppInstance.launch({ label, workDir: ctx.workDir, ...extra });
  const ready = await waitAppReady(app, { timeoutMs: 90_000 });
  const st = await app.state();
  note(`launch ${label}: pid=${app.mainPid} ready=${ready.ok} in ${ready.waitedMs}ms`);
  return { app, ready: { ...ready, state: st } };
}

async function closeApp(app, gate, prefix) {
  const q = await quitViaMpris(app);
  if (!q.exited || q.leftoverPids.length) {
    note(`${prefix}: MPRIS quit left processes; stopping scopes`, q.leftover);
    await app.kill();
  }
  add(gate, quitChecks(prefix, q));
  return q;
}

// ------------------------------------------------------------------------------------------------
const preexisting = findAppProcesses();
if (preexisting.some((p) => p.realProfile)) {
  console.error("A real-profile YTMcp instance is running; refusing to start.", preexisting);
  process.exit(3);
}
const sink = await ensureNullSink();
note("null sink ready", sink);
ctx.meta = { ...(await collectEnvironment()), harness: "tools/qa/m0.mjs", run: RUN, soakMinutes: SOAK_MIN, nullSink: NULL_SINK };
add("g0", check("G0.environment", "environment, versions and lockfile hashes recorded", { measured: { kernel: ctx.meta.kernel, plasma: ctx.meta.plasma, gpu: ctx.meta.gpu, electron: ctx.meta.electronBinary, rustc: ctx.meta.rustc, node: ctx.meta.node, dirtyFiles: ctx.meta.git.dirtyFiles }, pass: Boolean(ctx.meta.kernel && ctx.meta.git.commit) }));

let sharedFacts = { before: null };

try {
  if (PHASES.has("build")) {
    await phase("build", "g0", async () => {
      const b = await buildRelease();
      add("g0", check("G0.build", "release build of ytm-daemon, bridge, UI and desktop shell succeeds on the target", { measured: b, pass: b.steps.every((s) => s.exit === 0) }), { build: b });
    });
  }

  if (PHASES.has("process")) {
    await phase("process", "g0", async () => {
      const r = await sidecarFailureChecks(ctx);
      add("g0", r.checks, { sidecarFailure: r.evidence });
    });
  }

  if (PHASES.has("A")) {
    await phase("A", "g1", async () => {
      const { app, ready } = await launch("A");
      try {
        const logs = classifyLogs(app.logs());
        const rt = runtimeChecks(app, ready, logs);
        add("g0", rt.checks, { runtime: rt.evidence, initialSnapshot: ready.state.snapshot ? { revision: ready.state.snapshot.revision, protocolVersion: ready.state.snapshot.protocolVersion, mockMode: ready.state.snapshot.mockMode, authState: ready.state.snapshot.authState?.state, connection: ready.state.snapshot.player?.connection, networkState: ready.state.snapshot.networkState, hostHealth: ready.state.snapshot.hostHealth, capabilities: ready.state.snapshot.capabilities, queue: { completeness: ready.state.snapshot.queueSummary?.completeness, loaded: ready.state.snapshot.queueSummary?.loadedCount } } : null });
        add("g0", check("G0.ready", "app reached bridge ready + authenticated", { measured: { ok: ready.ok, waitedMs: ready.waitedMs, connection: ready.state.snapshot?.player?.connection, auth: ready.state.snapshot?.authState?.state }, pass: ready.ok }));
        const snap = ready.state.snapshot;
        const w = await waylandChecks(app);
        add("g1", w.checks, { wayland: w.measured });
        const g = await gpuChecks(app, snap);
        add("g1", g.checks, { gpu: g.measured });
        const sb = await sandboxChecks(app);
        add("g1", sb.checks, { sandbox: sb.measured });
        const se = await sessionChecks(app);
        add("g1", se.checks, { sessions: se.measured });
        const ipc = await ipcRejectionChecks(app);
        add("g1", ipc.checks, { ipcRejection: ipc.measured });
        const refresh = ctx.meta.display?.[0]?.refreshHz ?? 60;
        const scale = ctx.meta.display?.[0]?.scale ?? null;
        await app.call("winop", { target: "main", op: "show" });
        await app.click('[data-testid="nav-home"]');
        const fs2 = await frameSample(app, { seconds: 5 });
        add("g1", [
          check("G1.nativeScale", `native KDE scale (${scale}) applied: devicePixelRatio matches`, { measured: { kdeScale: scale, dpr: fs2.dpr }, pass: scale != null && Math.abs(fs2.dpr - scale) < 0.01 }),
          frameCheck("G1.frameNativeScale", fs2, refresh),
        ], { frameNativeScale: { ...fs2, intervals: undefined } });
        const rz = await resizeChecks(app);
        add("g1", rz.checks, { resize: rz.measured });
        const ime = await imeCheck(app);
        add("g1", ime.checks, { ime: ime.measured });
        const nm = await nonMusicOriginCheck(app);
        add("g1", nm.checks, { nonMusicOrigin: nm.measured });
        const ro = await reopenChecks(app, await app.snapshot());
        add("g1", ro.checks, { reopen: ro.measured });
        // G2 part 1: identity before the cold restart, with one logged-in read-only query
        await app.call("winop", { target: "main", op: "show" });
        const libSince = (await app.state()).seq;
        await app.click('[data-testid="nav-library"]');
        await app.waitForEvent((e) => e.catalog?.resultType === "libraryPlaylists", { since: libSince, timeoutMs: 25_000 });
        await app.waitFor((s) => s?.authState?.playerCatalogMatched === true, { timeoutMs: 20_000 });
        sharedFacts.before = await accountFacts(app);
        add("g2", [], { beforeRestart: sharedFacts.before });
      } finally {
        await closeApp(app, "g2", "G2.coldQuitA");
        add("g2", logHygieneCheck("G2.logHygieneA", app.readLogFile()));
      }
    });
  }

  if (PHASES.has("B")) {
    await phase("B", "g3", async () => {
      const { app, ready } = await launch("B");
      try {
        await app.waitFor((s) => s?.authState?.playerCatalogMatched === true, { timeoutMs: 20_000 });
        // One read-only catalog query of the restarted instance (the UI's home feed).
        const sinceHome = (await app.state()).seq;
        await navigate(app, "library");
        await navigate(app, "home");
        await waitCatalog(app, sinceHome, null, 20_000);
        const after = await accountFacts(app);
        add("g2", sharedFacts.before ? restartChecks(sharedFacts.before, after, ready) : check("G2.restart", "cold restart comparison", { measured: null, pass: false, notRun: "phase A did not run" }), { afterRestart: after });
        await app.call("winop", { target: "main", op: "show" });
        const s = await searchCheck(app, opt.query);
        add("g4", s.check, { search: s.measured });
        const c = await controlChecks(app, { query: opt.query });
        add("g3", c.checks, { controls: c.log });
        const mf = await mediaFacts(app);
        add("g3", mf.check, { mediaFacts: mf.measured });
        const sk = await soak(app, { minutes: SOAK_MIN, lockScreen: opt["lock-screen"] && RUN === "FINAL", onOfflineBrowse: offlineBrowseProbe(app), note });
        add("g3", soakChecks(sk, { requiredMinutes: REQUIRED_SOAK_MIN, lockScreen: opt["lock-screen"] && RUN === "FINAL" }), { soak: sk });
        const netCut = sk.events.find((e) => e.name === "netCutResult")?.result ?? null;
        add("g4", offlineBrowseCheck(netCut), { offlineBrowse: netCut?.during ?? null });
        const pd = await pausedHiddenDrop(app, Number(opt["paused-sec"]));
        add("g3", pd.check, { pausedHiddenDrop: pd.measured });
        await app.call("winop", { target: "main", op: "show" });
        const lib = await libraryPagingCheck(app);
        add("g4", lib.check, { libraryPaging: lib.measured });
        const evs = await app.events(0);
        const libEv = evs.filter((e) => e.catalog?.resultType === "libraryPlaylists").at(0)?.catalog;
        const pl = await playlistPagingCheck(app, libEv?.ids ?? [], libEv?.counts ?? []);
        add("g4", pl.check, { playlistPaging: pl.measured });
        add("g4", await accountConsistencyCheck(app));
        const g5 = await g5Checks(app);
        add("g5", g5.checks, { mpris: g5.measured });
        const rl = await reloadChecks(app);
        add("g2", rl.checks, { reload: rl.measured });
        const audio = await sinkInputsFor(app.pids());
        add("g3", check("G3.nullSinkOnly", "app audio went to the QA null sink only", { measured: audio.entries.map((e) => ({ sink: e.sink })), pass: audio.entries.every((e) => e.sink === NULL_SINK) }));
      } finally {
        await closeApp(app, "g2", "G2.coldQuitB");
        add("g2", logHygieneCheck("G2.logHygieneB", app.readLogFile()));
      }
    });
  }

  if (PHASES.has("C")) {
    await phase("C", "g4", async () => {
      const { app } = await launch("C", { daemonBin: resolve(REPO, "tools/qa/daemon-scope.sh"), extraEnv: { YTMCP_QA_REAL_DAEMON: DAEMON } });
      try {
        add("g4", [], { processTreeC: describeProcesses(app.pids()).map((p) => ({ role: p.role, cgroup: p.cgroup?.split("/").pop() })) });
        const f = await failureIsolationChecks(app, { query: opt.query });
        add("g4", f.checks, { failureIsolation: f.measured });
      } finally {
        await closeApp(app, "g4", "G4.quitC");
      }
    });
  }

  if (PHASES.has("D")) {
    await phase("D", "g1", async () => {
      // Chromium on Wayland ignores --force-device-scale-factor, and the real display scale is
      // never changed. Instead the app runs in a throwaway nested KWin (virtual output, private
      // D-Bus and config) whose output scale is set to 1 and 1.5. Mock profile: this checks
      // rendering scale and layout, not account data.
      const results = {};
      for (const scale of [1, 1.5]) {
        const tag = String(scale).replace(".", "_");
        const base = join(ctx.mockXdgRoot, `scale-${tag}`);
        const nk = await startNestedKwin({ dir: join(base, "kwin"), scale, label: `s${tag}` });
        let app = null;
        try {
          app = await AppInstance.launch({
            label: `D${tag}`,
            workDir: join(ctx.workDir, `D${tag}`),
            live: false,
            xdg: {
              XDG_DATA_HOME: join(base, "data"),
              XDG_CONFIG_HOME: join(base, "config"),
              XDG_CACHE_HOME: join(base, "cache"),
              XDG_STATE_HOME: join(base, "state"),
              YTMCP_MODE: "mock",
              WAYLAND_DISPLAY: nk.socket,
              DBUS_SESSION_BUS_ADDRESS: nk.bus,
            },
          });
          await sleep(4000);
          await app.call("winop", { target: "main", op: "show" });
          await app.call("winop", { target: "main", op: "setSize", args: [1280, 800] });
          await sleep(1000);
          await app.click('[data-testid="nav-home"]');
          const fsx = await frameSample(app, { seconds: 5 });
          const layout = await app.evalMain(`({ w: innerWidth, h: innerHeight, overflowX: document.documentElement.scrollWidth > innerWidth + 1, sidebar: Boolean(document.querySelector('[data-testid="nav-home"]')), playerBar: Boolean(document.querySelector('[data-testid="btn-play-pause"]')) })`);
          results[tag] = { outputScale: scale, outputs: nk.outputs, dpr: fsx.dpr, layout, frames: { ...fsx, intervals: undefined } };
        } catch (err) {
          results[tag] = { outputScale: scale, error: String(err?.message ?? err).slice(0, 300) };
        } finally {
          if (app) results[tag].quit = await app.quit();
          results[tag].nestedStopped = await nk.stop();
        }
      }
      const ok = (r, dpr) => Boolean(r && r.dpr === dpr && r.layout && !r.layout.overflowX && r.layout.sidebar && r.layout.playerBar);
      add("g1", [
        check("G1.scale100", "100% output scale (nested KWin, real display untouched): devicePixelRatio 1, layout intact", {
          measured: results["1"],
          pass: ok(results["1"], 1),
        }),
        check("G1.scale150", "150% fractional output scale (nested KWin): devicePixelRatio 1.5, layout intact", {
          measured: results["1_5"],
          pass: ok(results["1_5"], 1.5),
        }),
        check("G1.frameScaleNested", "frame intervals in the nested KWin (virtual 60 Hz output; supporting, not the real display)", {
          measured: { scale100: results["1"]?.frames ?? null, scale150: results["1_5"]?.frames ?? null },
          pass: [results["1"], results["1_5"]].every((r) => r?.frames?.p95Ms != null && r.frames.p95Ms < 1000 / 60 * 1.5),
          support: true,
        }),
      ], { nestedScale: results });
    });
  }
} finally {
  const u = await unloadNullSink(sink.created ? sink.moduleId : null);
  note("null sink unloaded", u);
  const { written, summary } = flush();
  writeFileSync(join(ctx.workDir, "summary.json"), JSON.stringify(summary, null, 2));
  console.log(JSON.stringify(summary.gates, null, 2));
  console.log(written.join("\n"));
  if (RUN === "FINAL" && Object.values(summary.gates).some((status) => status !== STATUS.PASS_TARGET)) {
    process.exitCode = 1;
  }
}
