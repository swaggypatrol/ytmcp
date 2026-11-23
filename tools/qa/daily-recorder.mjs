#!/usr/bin/env node
// Optional daily-use recorder for the installed app.
// Samples every 30 s: process-tree CPU (from /proc deltas) and PSS, MPRIS status/track/
// position, the app's PipeWire stream, and app restarts/crashes from the journal.
// Writes JSONL under ~/.local/state/ytm-glass/qa/. Records no titles, URLs or cookies.
//
//   node tools/qa/daily-recorder.mjs record [--interval 30] [--out FILE]
//   node tools/qa/daily-recorder.mjs summarize [--in FILE]      (prints the recording summary)
//
// As a transient user unit (stops by itself after 9 h):
//   systemd-run --user --unit=ytm-glass-daily-recorder -p RuntimeMaxSec=9h \
//     node "$PWD/tools/qa/daily-recorder.mjs" record
import { appendFileSync, existsSync, mkdirSync, readFileSync, readdirSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { findAppProcesses } from "./lib/app.mjs";
import {
  appProcessSet,
  mprisNames,
  mprisProps,
  percentile,
  procCmdline,
  resourceDelta,
  resourceSample,
  round,
  run,
  sha256Short,
  sinkInputsFor,
  sleep,
} from "./lib/sys.mjs";

const STATE_DIR = join(process.env.XDG_STATE_HOME || join(homedir(), ".local/state"), "ytm-glass/qa");
const { positionals, values: opt } = parseArgs({
  allowPositionals: true,
  options: { interval: { type: "string", default: "30" }, out: { type: "string" }, in: { type: "string" } },
});
const cmd = positionals[0] ?? "record";

/** Root browser process of the running (real-profile) app, if any. */
function findRoot() {
  const procs = findAppProcesses().filter((p) => p.realProfile);
  for (const p of procs) {
    const argv = procCmdline(p.pid);
    if (!argv.some((a) => a.startsWith("--type=")) && !argv[0]?.endsWith("ytm-daemon")) return p.pid;
  }
  return null;
}

async function journalSince(sinceSec) {
  const r = await run("journalctl", ["--user", `--since=@${sinceSec}`, "-o", "json", "--no-pager"], { timeoutMs: 20_000 });
  const crashes = [];
  let appLines = 0;
  for (const line of r.stdout.split("\n")) {
    if (!line) continue;
    let e;
    try {
      e = JSON.parse(line);
    } catch {
      continue;
    }
    const unit = String(e._SYSTEMD_USER_UNIT ?? e._SYSTEMD_UNIT ?? "");
    const ident = String(e.SYSLOG_IDENTIFIER ?? "");
    const msg = typeof e.MESSAGE === "string" ? e.MESSAGE : "";
    if (!/ytmglass|ytm-glass|ytm-daemon/i.test(`${unit} ${ident}`) && !/ytm-daemon|ytm-glass/.test(msg)) continue;
    appLines += 1;
    const m = /render-process-gone:([a-z-]+)|"event":"(daemon_unexpected_exit|daemon_spawn_error|startup_failed|quit_cleanup_failed)"|Segmentation fault|core dumped/.exec(msg);
    if (m) crashes.push({ at: Number(e.__REALTIME_TIMESTAMP) / 1000, kind: m[1] ?? m[2] ?? "signal" });
  }
  const cd = await run("coredumpctl", ["list", "--no-legend", "--no-pager", `--since=@${sinceSec}`], { timeoutMs: 10_000 });
  for (const l of cd.stdout.split("\n")) if (/ytm-glass|ytm-daemon|electron/.test(l)) crashes.push({ kind: "coredump", line: l.trim().split(/\s+/).slice(-1)[0] });
  return { appLines, crashes };
}

async function record() {
  mkdirSync(STATE_DIR, { recursive: true, mode: 0o700 });
  const out = opt.out ?? join(STATE_DIR, `daily-${new Date().toISOString().slice(0, 10)}-${process.pid}.jsonl`);
  const intervalMs = Number(opt.interval) * 1000;
  appendFileSync(out, `${JSON.stringify({ type: "start", at: new Date().toISOString(), intervalSec: Number(opt.interval), host: (await run("uname", ["-r"])).stdout.trim() })}\n`);
  console.log(`recording to ${out}`);
  let prev = null;
  let prevRoot = null;
  let lastJournal = Math.floor(Date.now() / 1000);
  for (;;) {
    const t = Date.now();
    const root = findRoot();
    const pids = root ? appProcessSet({ rootPid: root }) : [];
    const res = pids.length ? resourceSample(pids) : null;
    const delta = prev && res && prevRoot === root ? resourceDelta(prev, res) : null;
    const names = await mprisNames(pids);
    const mp = names.ytmGlass.length ? await mprisProps() : null;
    const audio = pids.length ? await sinkInputsFor(pids) : { entries: [] };
    const j = await journalSince(lastJournal);
    lastJournal = Math.floor(t / 1000);
    const rec = {
      type: "sample",
      at: new Date(t).toISOString(),
      running: Boolean(root),
      restarted: Boolean(prevRoot && root && prevRoot !== root),
      processCount: pids.length,
      cpuPctOfOneCore: delta?.cpuPctOfOneCore ?? null,
      wakeupsPerSec: delta?.wakeupsPerSec ?? null,
      pssMb: res ? round(res.pssKb / 1024, 1) : null,
      perRoleCpu: delta ? Object.fromEntries(Object.entries(delta.perRole).map(([k, v]) => [k, v.cpuPct])) : null,
      mpris: mp ? { status: mp.status, track: sha256Short(mp.trackId), positionSec: mp.positionUs != null ? round(mp.positionUs / 1e6, 1) : null, lengthSec: mp.lengthUs != null ? round(mp.lengthUs / 1e6, 1) : null } : null,
      mprisServices: names.ytmGlass.length,
      chromiumMprisFromApp: names.chromiumFromApp.length,
      stream: audio.entries.map((e) => ({ corked: e.corked, mute: e.mute })),
      journal: j,
    };
    appendFileSync(out, `${JSON.stringify(rec)}\n`);
    prev = res;
    prevRoot = root;
    const wait = intervalMs - (Date.now() - t);
    if (wait > 0) await sleep(wait);
  }
}

function latestFile() {
  if (!existsSync(STATE_DIR)) return null;
  const f = readdirSync(STATE_DIR).filter((n) => n.startsWith("daily-") && n.endsWith(".jsonl")).sort();
  return f.length ? join(STATE_DIR, f.at(-1)) : null;
}

/** Builds the T21 report section from one JSONL file; the verdict comes only from measured time. */
export function summarize(lines) {
  const samples = lines.filter((l) => l.type === "sample");
  if (!samples.length) return { text: "No samples.", stats: null };
  const start = new Date(samples[0].at).getTime();
  const end = new Date(samples.at(-1).at).getTime();
  const gaps = [];
  let running = 0;
  const byStatus = {};
  for (let i = 1; i < samples.length; i++) {
    const a = samples[i - 1];
    const b = samples[i];
    const dt = (new Date(b.at) - new Date(a.at)) / 1000;
    if (dt > 120) gaps.push({ at: a.at, sec: Math.round(dt) });
    if (a.running && b.running && dt <= 120) {
      running += dt;
      const st = a.mpris?.status ?? "unknown";
      byStatus[st] = (byStatus[st] ?? 0) + dt;
    }
  }
  const playing = samples.filter((s) => s.mpris?.status === "Playing");
  const playingNoStream = playing.filter((s) => !s.stream.some((x) => !x.corked)).length;
  const stuck = [];
  for (let i = 1; i < samples.length; i++) {
    const a = samples[i - 1];
    const b = samples[i];
    if (a.mpris?.status === "Playing" && b.mpris?.status === "Playing" && a.mpris.track === b.mpris.track && a.mpris.positionSec != null && b.mpris.positionSec != null && b.mpris.positionSec - a.mpris.positionSec < 5) stuck.push(b.at);
  }
  const cpu = (st) => samples.filter((s) => s.mpris?.status === st && s.cpuPctOfOneCore != null).map((s) => s.cpuPctOfOneCore);
  const pss = samples.filter((s) => s.pssMb != null).map((s) => ({ t: (new Date(s.at) - start) / 3.6e6, v: s.pssMb }));
  let slope = null;
  if (pss.length > 10) {
    const n = pss.length;
    const mt = pss.reduce((a, p) => a + p.t, 0) / n;
    const mv = pss.reduce((a, p) => a + p.v, 0) / n;
    const num = pss.reduce((a, p) => a + (p.t - mt) * (p.v - mv), 0);
    const den = pss.reduce((a, p) => a + (p.t - mt) ** 2, 0);
    slope = den ? round(num / den, 1) : null;
  }
  const tracks = new Set(samples.map((s) => s.mpris?.track).filter(Boolean)).size;
  const restarts = samples.filter((s) => s.restarted).length;
  const crashes = samples.flatMap((s) => s.journal?.crashes ?? []);
  const dupMpris = samples.filter((s) => s.mprisServices > 1 || s.chromiumMprisFromApp > 0).length;
  const runningHours = round(running / 3600, 2);
  const stats = {
    wallStart: samples[0].at,
    wallEnd: samples.at(-1).at,
    wallHours: round((end - start) / 3.6e6, 2),
    appRunningHours: runningHours,
    secondsByMprisStatus: Object.fromEntries(Object.entries(byStatus).map(([k, v]) => [k, Math.round(v)])),
    samples: samples.length,
    samplingGaps: gaps,
    distinctTracks: tracks,
    restarts,
    crashes,
    playingSamplesWithoutStream: playingNoStream,
    playingSamplesPositionStuck: stuck.length,
    duplicateMprisSamples: dupMpris,
    cpuPlaying: { p50: percentile(cpu("Playing"), 50), p95: percentile(cpu("Playing"), 95) },
    cpuPaused: { p50: percentile(cpu("Paused"), 50), p95: percentile(cpu("Paused"), 95) },
    pssMb: pss.length ? { first: pss[0].v, last: pss.at(-1).v, max: Math.max(...pss.map((p) => p.v)), slopeMbPerHour: slope } : null,
  };
  const done = runningHours >= 8;
  const clean = restarts === 0 && crashes.length === 0 && dupMpris === 0 && playingNoStream === 0 && stuck.length === 0;
  const verdict = !done ? "INCOMPLETE (the app ran for less than 8 hours)" : clean ? "PASS_TARGET (no crash, restart, duplicate MPRIS or silent playback in 8 hours; memory curve below)" : "FAIL (see the anomaly counts below)";
  const text = [
    "## T21 eight-hour daily-use record",
    "",
    `- Verdict: ${verdict}`,
    `- Recorded: ${stats.wallStart} → ${stats.wallEnd} (wall clock ${stats.wallHours} h, app running ${stats.appRunningHours} h, ${stats.samples} samples)`,
    `- Seconds by MPRIS status: ${JSON.stringify(stats.secondsByMprisStatus)}`,
    `- Sampling gaps (>120 s, likely suspend or recorder stopped): ${gaps.length ? gaps.map((g) => `${g.at} ${g.sec}s`).join("; ") : "none"}`,
    `- Tracks: ${tracks}; app restarts: ${restarts}; crashes/errors: ${crashes.length}${crashes.length ? ` ${JSON.stringify(crashes)}` : ""}`,
    `- Playing samples without an active audio stream: ${playingNoStream}; Playing samples with a stalled position: ${stuck.length}; duplicate MPRIS samples: ${dupMpris}`,
    `- Process tree CPU (percent of one core) Playing p50/p95: ${stats.cpuPlaying.p50}/${stats.cpuPlaying.p95}; Paused p50/p95: ${stats.cpuPaused.p50}/${stats.cpuPaused.p95}`,
    `- Process tree PSS (MB): first ${stats.pssMb?.first}, last ${stats.pssMb?.last}, max ${stats.pssMb?.max}, linear slope ${stats.pssMb?.slopeMbPerHour} MB/h`,
    "",
    "The recorder does not mark deliberate actions (pause, screen lock, network loss); add them to the report by time so they are not misattributed.",
  ].join("\n");
  return { text, stats };
}

if (cmd === "record") {
  await record();
} else if (cmd === "summarize") {
  const file = opt.in ?? latestFile();
  if (!file) {
    console.error(`no recording found in ${STATE_DIR}`);
    process.exit(2);
  }
  const lines = readFileSync(file, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
  const { text, stats } = summarize(lines);
  console.log(text);
  console.log(`\n<!-- source: ${file.replace(homedir(), "~")} -->`);
  if (process.env.YTM_GLASS_QA_JSON === "1") console.log(JSON.stringify(stats, null, 2));
} else {
  console.error("usage: daily-recorder.mjs record|summarize");
  process.exit(2);
}
