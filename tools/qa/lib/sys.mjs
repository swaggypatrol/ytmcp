// Host-side measurement helpers for the live QA harness: subprocesses, /proc sampling,
// PipeWire/Pulse, D-Bus (MPRIS, KWin) and per-cgroup network cuts. Nothing here reads
// cookies, profile files or page credentials.
import { execFile, execFileSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync, readlinkSync, writeFileSync, mkdtempSync, rmSync } from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Runs a command, never throws; returns {ok, code, stdout, stderr, ms}. */
export function run(cmd, args = [], { timeoutMs = 15_000, env, input } = {}) {
  const t0 = Date.now();
  return new Promise((resolve) => {
    const child = execFile(
      cmd,
      args,
      { encoding: "utf8", timeout: timeoutMs, env: env ?? process.env, maxBuffer: 64 * 1024 * 1024 },
      (err, stdout, stderr) => {
        resolve({
          ok: !err,
          code: err ? (typeof err.code === "number" ? err.code : 1) : 0,
          stdout: stdout ?? "",
          stderr: stderr ?? (err ? String(err) : ""),
          ms: Date.now() - t0,
        });
      }
    );
    if (input !== undefined) {
      child.stdin.end(input);
    }
  });
}

export function runSync(cmd, args = [], timeoutMs = 10_000) {
  try {
    return { ok: true, stdout: execFileSync(cmd, args, { encoding: "utf8", timeout: timeoutMs, stdio: ["ignore", "pipe", "pipe"] }) };
  } catch (err) {
    return { ok: false, stdout: err?.stdout ? String(err.stdout) : "", stderr: String(err?.stderr ?? err) };
  }
}

export async function firstLine(cmd, args) {
  const r = await run(cmd, args);
  const text = (r.stdout || r.stderr).trim().split("\n")[0] ?? "";
  return r.ok ? text : `MISSING_OR_FAILED(${cmd}): ${text}`.slice(0, 200);
}

// ---------------------------------------------------------------- /proc

const CLK_TCK = 100;

function readText(path) {
  try {
    return readFileSync(path, "utf8");
  } catch {
    return null;
  }
}

/**
 * argv of a process. Chromium children rewrite their title into one space-joined
 * string; that form is split at " --" so the executable path keeps its spaces.
 */
export function procCmdline(pid) {
  const raw = readText(`/proc/${pid}/cmdline`);
  if (!raw) return [];
  const parts = raw.split("\0").filter(Boolean);
  if (parts.length === 1 && parts[0].includes(" --")) {
    const [exe, ...rest] = parts[0].split(" --");
    const tail = rest.map((r) => `--${r}`);
    const exeParts = exe.split(" ");
    // "electron /path/entry.mjs" (browser process): keep the entry as argv[1]
    if (exeParts.length > 1 && /\.(m?js|cjs)$/.test(exeParts.at(-1))) return [exeParts.slice(0, -1).join(" "), exeParts.at(-1), ...tail];
    return [exe, ...tail];
  }
  if (parts.length === 1 && /^\S+ \S+\.(m?js|cjs)$/.test(parts[0])) return parts[0].split(" ");
  return parts;
}

export function procStat(pid) {
  const raw = readText(`/proc/${pid}/stat`);
  if (!raw) return null;
  const close = raw.lastIndexOf(")");
  const comm = raw.slice(raw.indexOf("(") + 1, close);
  const f = raw.slice(close + 2).split(" ");
  // f[0] = state (field 3); utime = field 14 -> f[11]; stime = field 15 -> f[12]; ppid -> f[1]
  return { pid, comm, state: f[0], ppid: Number(f[1]), utime: Number(f[11]), stime: Number(f[12]) };
}

export function procStatus(pid) {
  const raw = readText(`/proc/${pid}/status`);
  if (!raw) return null;
  const out = {};
  for (const line of raw.split("\n")) {
    const m = /^([A-Za-z_]+):\s*(.*)$/.exec(line);
    if (m) out[m[1]] = m[2].trim();
  }
  return out;
}

export function procPssKb(pid) {
  const raw = readText(`/proc/${pid}/smaps_rollup`);
  if (!raw) return null;
  const get = (k) => Number(new RegExp(`^${k}:\\s+(\\d+) kB`, "m").exec(raw)?.[1] ?? NaN);
  return { pssKb: get("Pss"), privateKb: get("Private_Clean") + get("Private_Dirty"), rssKb: get("Rss") };
}

/** Voluntary+nonvoluntary context switches per thread (tid -> count). */
export function procCtxSwitches(pid) {
  let tasks;
  try {
    tasks = readdirSync(`/proc/${pid}/task`);
  } catch {
    return null;
  }
  const out = {};
  for (const tid of tasks) {
    const raw = readText(`/proc/${pid}/task/${tid}/status`);
    if (!raw) continue;
    out[tid] = Number(/^voluntary_ctxt_switches:\s+(\d+)/m.exec(raw)?.[1] ?? 0) + Number(/^nonvoluntary_ctxt_switches:\s+(\d+)/m.exec(raw)?.[1] ?? 0);
  }
  return out;
}

/** Context switches between two per-thread maps; threads that appeared count from zero. */
function ctxDelta(a, b) {
  let d = 0;
  for (const [tid, v] of Object.entries(b ?? {})) d += v - (a?.[tid] ?? 0);
  return d;
}

export function procNs(pid, ns) {
  try {
    return readlinkSync(`/proc/${pid}/ns/${ns}`);
  } catch {
    return null;
  }
}

export function allPids() {
  return readdirSync("/proc").filter((n) => /^\d+$/.test(n)).map(Number);
}

/** All descendants of rootPid (inclusive) by ppid links. */
export function descendants(rootPid) {
  const byParent = new Map();
  for (const pid of allPids()) {
    const st = procStat(pid);
    if (!st) continue;
    if (!byParent.has(st.ppid)) byParent.set(st.ppid, []);
    byParent.get(st.ppid).push(pid);
  }
  const out = [];
  const stack = [rootPid];
  while (stack.length) {
    const p = stack.pop();
    if (!existsSync(`/proc/${p}`)) continue;
    out.push(p);
    for (const c of byParent.get(p) ?? []) stack.push(c);
  }
  return out;
}

export function cgroupOf(pid) {
  const raw = readText(`/proc/${pid}/cgroup`);
  return raw ? raw.trim().split("\n").find((l) => l.startsWith("0::"))?.slice(3) ?? null : null;
}

export function cgroupPids(cgPath) {
  const raw = readText(`/sys/fs/cgroup${cgPath}/cgroup.procs`);
  return raw ? raw.trim().split("\n").filter(Boolean).map(Number) : [];
}

export function cgroupCpuUsec(cgPath) {
  const raw = readText(`/sys/fs/cgroup${cgPath}/cpu.stat`);
  return raw ? Number(/^usage_usec (\d+)/m.exec(raw)?.[1] ?? NaN) : null;
}

export function cgroupMemCurrent(cgPath) {
  const raw = readText(`/sys/fs/cgroup${cgPath}/memory.current`);
  return raw ? Number(raw.trim()) : null;
}

/** Classifies an Electron/daemon process by argv. */
export function processRole(argv) {
  const type = argv.find((a) => a.startsWith("--type="))?.slice(7) ?? null;
  const exe = argv[0] ?? "";
  if (exe.endsWith("ytm-daemon")) return "core:ytm-daemon";
  if (exe.includes("crashpad")) return "crashpad";
  if (!type) return "browser(main)";
  if (type === "utility") {
    const sub = argv.find((a) => a.startsWith("--utility-sub-type="))?.slice(19) ?? "?";
    return `utility:${sub}`;
  }
  return type;
}

/** Snapshot of the app's process set: pids from the given roots/cgroups. */
export function appProcessSet({ rootPid = null, cgroups = [] } = {}) {
  const pids = new Set();
  if (rootPid) for (const p of descendants(rootPid)) pids.add(p);
  for (const cg of cgroups) for (const p of cgroupPids(cg)) pids.add(p);
  return [...pids].sort((a, b) => a - b);
}

export function describeProcesses(pids) {
  return pids
    .map((pid) => {
      const st = procStat(pid);
      if (!st) return null;
      const argv = procCmdline(pid);
      const status = procStatus(pid) ?? {};
      return {
        pid,
        ppid: st.ppid,
        comm: st.comm,
        role: processRole(argv),
        // argv is reduced to flags: no user paths beyond the executable basename.
        exe: (argv[0] ?? "").split("/").pop(),
        flags: argv.filter((a) => a.startsWith("--")).map((a) => a.replace(/=.*/, "")),
        noSandboxFlag: argv.includes("--no-sandbox"),
        seccomp: status.Seccomp ?? null,
        noNewPrivs: status.NoNewPrivs ?? null,
        userNs: procNs(pid, "user"),
        pidNs: procNs(pid, "pid"),
        netNs: procNs(pid, "net"),
        cgroup: cgroupOf(pid),
      };
    })
    .filter(Boolean);
}

/** One resource sample of a process set (cumulative counters; diff two samples for rates). */
export function resourceSample(pids, cgroups = []) {
  let ticks = 0;
  let pss = 0;
  let priv = 0;
  const per = [];
  for (const pid of pids) {
    const st = procStat(pid);
    if (!st) continue;
    const mem = procPssKb(pid);
    ticks += st.utime + st.stime;
    pss += mem?.pssKb ?? 0;
    priv += mem?.privateKb ?? 0;
    per.push({ pid, role: processRole(procCmdline(pid)), ticks: st.utime + st.stime, ctx: procCtxSwitches(pid) ?? {}, pssKb: mem?.pssKb ?? null });
  }
  const cg = cgroups.map((c) => ({ cgroup: c, usageUsec: cgroupCpuUsec(c), memCurrent: cgroupMemCurrent(c) }));
  return { atMs: Date.now(), mono: performance.now(), ticks, pssKb: pss, privateKb: priv, per, cg };
}

/** CPU (% of one logical core), wakeups/s and PSS between two samples. */
export function resourceDelta(a, b) {
  const sec = (b.mono - a.mono) / 1000;
  const prev = new Map(a.per.map((p) => [p.pid, p]));
  let ticks = 0;
  let ctx = 0;
  const perRole = {};
  for (const p of b.per) {
    const q = prev.get(p.pid);
    const dt = q ? p.ticks - q.ticks : 0;
    const dc = q ? ctxDelta(q.ctx, p.ctx) : 0;
    ticks += dt;
    ctx += dc;
    const r = (perRole[p.role] ??= { cpuPct: 0, wakeupsPerSec: 0, pssKb: 0, count: 0 });
    r.cpuPct += (dt / CLK_TCK / sec) * 100;
    r.wakeupsPerSec += dc / sec;
    r.pssKb += p.pssKb ?? 0;
    r.count += 1;
  }
  for (const r of Object.values(perRole)) {
    r.cpuPct = round(r.cpuPct, 2);
    r.wakeupsPerSec = round(r.wakeupsPerSec, 1);
  }
  const cg = b.cg.map((c, i) => {
    const before = a.cg[i];
    const du = before && c.usageUsec != null && before.usageUsec != null ? c.usageUsec - before.usageUsec : null;
    return { cgroup: c.cgroup, cpuPct: du == null ? null : round((du / 1e6 / sec) * 100, 2), memCurrent: c.memCurrent };
  });
  return {
    seconds: round(sec, 2),
    cpuPctOfOneCore: round((ticks / CLK_TCK / sec) * 100, 2),
    wakeupsPerSec: round(ctx / sec, 1),
    pssMb: round(b.pssKb / 1024, 1),
    privateMb: round(b.privateKb / 1024, 1),
    processCount: b.per.length,
    perRole,
    cgroups: cg,
  };
}

export const round = (v, d = 1) => (Number.isFinite(v) ? Math.round(v * 10 ** d) / 10 ** d : null);

export function percentile(values, p) {
  const v = values.filter(Number.isFinite).sort((a, b) => a - b);
  if (!v.length) return null;
  const idx = Math.min(v.length - 1, Math.max(0, Math.ceil((p / 100) * v.length) - 1));
  return v[idx];
}

// ---------------------------------------------------------------- audio

export const NULL_SINK = "ytmglass_null";

/** Loads the QA null sink (idempotent). Restores the default sink if loading changed it. */
export async function ensureNullSink() {
  const before = (await run("pactl", ["get-default-sink"])).stdout.trim();
  const sinks = (await run("pactl", ["list", "short", "sinks"])).stdout;
  let moduleId = null;
  let created = false;
  if (!sinks.split("\n").some((l) => l.split("\t")[1] === NULL_SINK)) {
    const r = await run("pactl", [
      "load-module",
      "module-null-sink",
      `sink_name=${NULL_SINK}`,
      "sink_properties=device.description=YTMGlassNull",
    ]);
    if (!r.ok) throw new Error(`null sink load failed: ${r.stderr}`);
    moduleId = r.stdout.trim();
    created = true;
  } else {
    const mods = (await run("pactl", ["list", "short", "modules"])).stdout;
    moduleId = mods.split("\n").find((l) => l.includes(`sink_name=${NULL_SINK}`))?.split("\t")[0] ?? null;
  }
  const after = (await run("pactl", ["get-default-sink"])).stdout.trim();
  if (before && after !== before) await run("pactl", ["set-default-sink", before]);
  return { moduleId, created, defaultSinkBefore: before, defaultSinkAfterLoad: after };
}

export async function unloadNullSink(moduleId) {
  if (!moduleId) return { ok: false, reason: "no module id" };
  const r = await run("pactl", ["unload-module", String(moduleId)]);
  return { ok: r.ok, stderr: r.stderr.trim() || null };
}

/** Sink inputs of the given pids, with the sink they play to and whether they are corked. */
export async function sinkInputsFor(pids) {
  const [inputs, sinks] = await Promise.all([
    run("pactl", ["-f", "json", "list", "sink-inputs"]),
    run("pactl", ["-f", "json", "list", "short", "sinks"]),
  ]);
  if (!inputs.ok || !sinks.ok) return { ok: false, entries: [] };
  let list = [];
  let sinkList = [];
  try {
    list = JSON.parse(inputs.stdout || "[]");
    sinkList = JSON.parse(sinks.stdout || "[]");
  } catch {
    return { ok: false, entries: [] };
  }
  const sinkName = new Map(sinkList.map((s) => [s.index, s.name]));
  const pidSet = new Set(pids.map(String));
  const entries = list
    .filter((si) => pidSet.has(String(si.properties?.["application.process.id"] ?? "")))
    .map((si) => ({
      index: si.index,
      sink: sinkName.get(si.sink) ?? String(si.sink),
      corked: si.corked,
      mute: si.mute,
      sampleSpec: si.sample_specification,
      appName: si.properties?.["application.name"] ?? null,
      processId: Number(si.properties?.["application.process.id"]),
      mediaName: si.properties?.["media.name"] ? "[present]" : null,
    }));
  const otherOnNullSink = list.some((si) => sinkName.get(si.sink) === NULL_SINK &&
    !pidSet.has(String(si.properties?.["application.process.id"] ?? "")) && !si.corked);
  return { ok: true, entries, otherOnNullSink };
}

// ---------------------------------------------------------------- D-Bus: MPRIS / KWin

export async function busNames() {
  const r = await run("busctl", ["--user", "list", "--no-legend"]);
  return r.stdout.split("\n").map((l) => l.trim().split(/\s+/)[0]).filter(Boolean);
}

export async function mprisNames(appPids = []) {
  const names = (await busNames()).filter((n) => n.startsWith("org.mpris.MediaPlayer2."));
  const pidSet = new Set(appPids);
  const chromiumFromApp = names.filter((n) => {
    const m = /\.chromium\.instance(\d+)$/.exec(n);
    return m ? pidSet.has(Number(m[1])) : false;
  });
  return { all: names, ytmGlass: names.filter((n) => n === "org.mpris.MediaPlayer2.ytm_glass"), chromiumFromApp };
}

export async function mprisProps(name = "org.mpris.MediaPlayer2.ytm_glass") {
  const get = async (prop) => {
    const r = await run("busctl", ["--user", "-j", "get-property", name, "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2.Player", prop]);
    if (!r.ok) return null;
    try {
      return JSON.parse(r.stdout).data;
    } catch {
      return null;
    }
  };
  const [status, position, metadata] = await Promise.all([get("PlaybackStatus"), get("Position"), get("Metadata")]);
  const md = metadata ?? {};
  return {
    status,
    positionUs: position,
    trackId: md["mpris:trackid"]?.data ?? null,
    lengthUs: md["mpris:length"]?.data ?? null,
    hasTitle: Boolean(md["xesam:title"]?.data),
  };
}

/** Runs a KWin script and returns the lines it print()s (read back from the journal). */
export async function kwinScript(body, timeoutMs = 6000) {
  const marker = `YTMQA${Date.now()}${Math.floor(Math.random() * 1e6)}`;
  const dir = mkdtempSync(join(tmpdir(), "ytmqa-kwin-"));
  const file = join(dir, "script.js");
  const src = `function out(o){ print("${marker} " + JSON.stringify(o)); }\ntry { ${body} } catch (e) { out({error: String(e)}); }\nout({done:true});\n`;
  writeFileSync(file, src);
  const since = `@${Math.floor(Date.now() / 1000) - 2}`;
  const pluginName = `ytmqa_${marker}`;
  try {
    const load = await run("qdbus6", ["org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting.loadScript", file, pluginName]);
    const id = load.stdout.trim();
    if (!load.ok || !/^\d+$/.test(id)) return { ok: false, error: `loadScript failed: ${load.stderr || load.stdout}` };
    // KWin 6 exposes the loaded script at /Scripting/Script<N>.
    const started = await run("qdbus6", ["org.kde.KWin", `/Scripting/Script${id}`, "org.kde.kwin.Script.run"]);
    if (!started.ok) await run("qdbus6", ["org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting.start"]);
    const deadline = Date.now() + timeoutMs;
    let lines = [];
    while (Date.now() < deadline) {
      await sleep(250);
      const j = await run("journalctl", ["--user", "--since", since, "-o", "cat", "--no-pager"], { timeoutMs: 5000 });
      lines = j.stdout.split("\n").filter((l) => l.includes(marker));
      if (lines.some((l) => l.includes('"done":true'))) break;
    }
    await run("qdbus6", ["org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting.unloadScript", pluginName]);
    const objs = lines
      .map((l) => {
        try {
          return JSON.parse(l.slice(l.indexOf(marker) + marker.length + 1));
        } catch {
          return null;
        }
      })
      .filter(Boolean);
    return { ok: objs.some((o) => o.done), results: objs.filter((o) => !o.done) };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** KWin facts about windows whose pid is in `pids`. */
export async function kwinWindowsFor(pids) {
  const list = JSON.stringify(pids);
  const r = await kwinScript(`
    const pids = ${list};
    const wins = workspace.windowList();
    for (const w of wins) {
      if (pids.indexOf(w.pid) < 0) continue;
      out({
        cls: String(w),
        pid: w.pid,
        caption: w.caption,
        resourceClass: w.resourceClass,
        resourceName: w.resourceName,
        desktopFileName: w.desktopFileName,
        internalId: String(w.internalId),
        minimized: w.minimized,
        active: w.active,
        hidden: w.hidden,
        normalWindow: w.normalWindow,
        x11: (typeof w.windowId === "number" && w.windowId > 0),
        frame: { w: w.frameGeometry.width, h: w.frameGeometry.height },
        output: w.output ? w.output.name : null
      });
    }`);
  return r;
}

/** Closes (like the title-bar close button) every window of `pids`; returns what was closed. */
export async function kwinCloseWindowsFor(pids) {
  const list = JSON.stringify(pids);
  return kwinScript(`
    const pids = ${list};
    for (const w of workspace.windowList()) {
      if (pids.indexOf(w.pid) < 0) continue;
      out({ closed: String(w.internalId), caption: w.caption, cls: String(w) });
      w.closeWindow();
    }`);
}

/** Activates the app window of `pid` whose caption does not contain `excludeCaption`. */
export async function kwinActivate(pid, excludeCaption = "YouTube") {
  const ex = JSON.stringify(excludeCaption);
  return kwinScript(`
    const wins = workspace.windowList();
    for (const w of wins) {
      if (w.pid !== ${Number(pid)} || !w.normalWindow) continue;
      if (${ex} && String(w.caption).indexOf(${ex}) >= 0) continue;
      workspace.activeWindow = w;
      out({activated: String(w.internalId), active: w.active});
      break;
    }`);
}

export async function kwinActiveWindow() {
  return kwinScript(`const w = workspace.activeWindow; out(w ? {pid: w.pid, caption: w.caption, resourceClass: w.resourceClass} : {pid: null});`);
}

const VDM = ["org.kde.KWin", "/VirtualDesktopManager", "org.kde.KWin.VirtualDesktopManager"];

export async function virtualDesktops() {
  const r = await run("busctl", ["--user", "-j", "get-property", ...VDM, "desktops"]);
  const cur = await run("busctl", ["--user", "-j", "get-property", ...VDM, "current"]);
  let desktops = [];
  try {
    desktops = JSON.parse(r.stdout).data.map(([position, id, name]) => ({ position, id, name }));
  } catch {
    // keep empty
  }
  let current = null;
  try {
    current = JSON.parse(cur.stdout).data;
  } catch {
    // keep null
  }
  return { desktops, current };
}

export async function createVirtualDesktop(name) {
  const before = await virtualDesktops();
  await run("busctl", ["--user", "call", ...VDM, "createDesktop", "us", String(before.desktops.length), name]);
  const after = await virtualDesktops();
  return after.desktops.find((d) => d.name === name && !before.desktops.some((b) => b.id === d.id)) ?? null;
}

export async function setCurrentDesktop(id) {
  return run("busctl", ["--user", "set-property", ...VDM, "current", "s", id]);
}

export async function removeVirtualDesktop(id) {
  return run("busctl", ["--user", "call", ...VDM, "removeDesktop", "s", id]);
}

// ---------------------------------------------------------------- per-cgroup network cut

/**
 * Drops all egress of the given cgroups with a temporary nftables table. A root
 * systemd timer deletes the table after `fallbackSec` even if this process dies.
 */
export async function cutNetwork(cgroups, { fallbackSec = 180 } = {}) {
  const table = `ytmglass_qa_${Date.now()}`;
  const rules = cgroups
    .map((cg) => {
      const rel = cg.replace(/^\//, "");
      const level = rel.split("/").length;
      return `    socket cgroupv2 level ${level} "${rel}" counter drop`;
    })
    .join("\n");
  const ruleset = `table inet ${table} {\n  chain out {\n    type filter hook output priority -10; policy accept;\n${rules}\n  }\n}\n`;
  const unit = `${table.replace(/_/g, "-")}-revert`;
  const timer = await run("sudo", [
    "-n",
    "systemd-run",
    `--on-active=${fallbackSec}`,
    `--unit=${unit}`,
    "--description=YTM Glass QA: remove temporary nftables table",
    "/usr/bin/nft",
    "delete",
    "table",
    "inet",
    table,
  ]);
  if (!timer.ok) return { ok: false, error: `fallback timer not armed: ${timer.stderr.trim()}` };
  const add = await run("sudo", ["-n", "nft", "-f", "-"], { input: ruleset });
  if (!add.ok) {
    await run("sudo", ["-n", "systemctl", "stop", `${unit}.timer`]);
    return { ok: false, error: `nft load failed: ${add.stderr.trim()}` };
  }
  const handle = { table, unit, cgroups, startedAtMs: Date.now() };
  activeCuts.add(handle);
  return { ok: true, ...handle, ruleset };
}

export async function restoreNetwork(handle) {
  const counters = await run("sudo", ["-n", "nft", "list", "table", "inet", handle.table]);
  const del = await run("sudo", ["-n", "nft", "delete", "table", "inet", handle.table]);
  await run("sudo", ["-n", "systemctl", "stop", `${handle.unit}.timer`]);
  activeCuts.delete(handle);
  const dropped = [...counters.stdout.matchAll(/counter packets (\d+) bytes (\d+)/g)].map((m) => ({ packets: Number(m[1]), bytes: Number(m[2]) }));
  const still = await run("sudo", ["-n", "nft", "list", "table", "inet", handle.table]);
  return { deleted: del.ok, tableGoneAfter: !still.ok, droppedCounters: dropped, durationMs: Date.now() - handle.startedAtMs };
}

const activeCuts = new Set();
const cleanupHooks = new Set();

/** Registers a synchronous cleanup (desktops, sinks) that also runs on SIGINT/SIGTERM/exit. */
export function onCleanup(fn) {
  cleanupHooks.add(fn);
  return () => cleanupHooks.delete(fn);
}

function emergencyCleanup() {
  for (const h of activeCuts) {
    try {
      execFileSync("sudo", ["-n", "nft", "delete", "table", "inet", h.table], { stdio: "ignore", timeout: 5000 });
      execFileSync("sudo", ["-n", "systemctl", "stop", `${h.unit}.timer`], { stdio: "ignore", timeout: 5000 });
    } catch {
      // the root timer still removes it
    }
  }
  activeCuts.clear();
  for (const fn of cleanupHooks) {
    try {
      fn();
    } catch {
      // best effort
    }
  }
  cleanupHooks.clear();
}

process.on("exit", emergencyCleanup);
for (const sig of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(sig, () => {
    emergencyCleanup();
    process.exit(130);
  });
}

// ---------------------------------------------------------------- misc

export function sha256Short(value) {
  if (value == null) return null;
  return `h:${createHashHex(String(value)).slice(0, 12)}`;
}

function createHashHex(s) {
  return createHash("sha256").update(s).digest("hex");
}

export function fileSha256(path) {
  try {
    return createHashHex(readFileSync(path));
  } catch {
    return null;
  }
}
