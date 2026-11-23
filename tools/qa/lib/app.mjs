// Launches the real desktop app (production main bundle via tools/qa/qa-entry.mjs) inside a
// transient systemd user scope and drives it through the control socket.
import { spawn } from "node:child_process";
import { createWriteStream, existsSync, mkdirSync, readFileSync } from "node:fs";
import { connect } from "node:net";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import {
  allPids,
  appProcessSet,
  cgroupOf,
  cgroupPids,
  onCleanup,
  procCmdline,
  procStat,
  run,
  sleep,
} from "./sys.mjs";

export const REPO = resolve(import.meta.dirname, "../../..");
export const ELECTRON = resolve(REPO, "apps/desktop/node_modules/electron/dist/electron");
export const TARGET_DIR = process.env.CARGO_TARGET_DIR || resolve(REPO, "target");
export const DAEMON = resolve(TARGET_DIR, "release/ytm-daemon");
export const QA_ENTRY = resolve(REPO, "tools/qa/qa-entry.mjs");
export const PROD_MAIN = resolve(REPO, "apps/desktop/dist/main.mjs");

/** Processes on this machine that belong to any YTMcp instance (dev or installed). */
export function findAppProcesses() {
  const out = [];
  for (const pid of allPids()) {
    if (pid === process.pid) continue;
    const argv = procCmdline(pid);
    const joined = argv.join(" ");
    if (
      argv[0]?.endsWith("/ytm-daemon") ||
      argv[0] === "/opt/ytmcp/ytmcp-player" ||
      (argv[0]?.includes("electron") && /ytmcp|qa-entry\.mjs|apps\/desktop\/dist\/main\.mjs/.test(joined)) ||
      /--user-data-dir=[^ ]*ytmcp/.test(joined) ||
      /ytmcp\/electron-profile/.test(joined)
    ) {
      out.push({ pid, exe: (argv[0] ?? "").split("/").pop(), cgroup: cgroupOf(pid), realProfile: usesRealProfile(pid) });
    }
  }
  return out;
}

const REAL_PROFILE_DIR = join(process.env.HOME ?? "", ".local/share/ytmcp");

function userDataDirArg(pid) {
  return procCmdline(pid).find((a) => a.startsWith("--user-data-dir="))?.slice(16) ?? null;
}

/**
 * True when a process belongs to an instance using the user's real profile. Chromium
 * overwrites its environ with the process title, so the decision uses the
 * --user-data-dir of the process, its ancestors or its children; ytm-daemon (not
 * Chromium) is judged by its environment.
 */
export function usesRealProfile(pid) {
  const near = [pid];
  let cur = pid;
  for (let i = 0; i < 4; i += 1) {
    cur = procStat(cur)?.ppid ?? 0;
    if (cur <= 1) break;
    near.push(cur);
  }
  for (const p of allPids()) if (procStat(p)?.ppid === pid) near.push(p);
  for (const p of near) {
    const dir = userDataDirArg(p);
    if (dir) return dir.startsWith(REAL_PROFILE_DIR);
  }
  let env = "";
  try {
    env = readFileSync(`/proc/${pid}/environ`, "utf8");
  } catch {
    return true;
  }
  const vars = Object.fromEntries(env.split("\0").filter((kv) => kv.includes("=")).map((kv) => [kv.slice(0, kv.indexOf("=")), kv.slice(kv.indexOf("=") + 1)]));
  if (!Object.keys(vars).length) return true; // unknown: treat as real to stay safe
  if (vars.YTMCP_MODE === "mock") return false;
  const data = vars.XDG_DATA_HOME;
  return !data || data === join(process.env.HOME ?? "", ".local/share");
}

/** Waits until no other YTMcp process exists; refuses at once if one uses the real profile. */
export async function waitNoOtherInstances({ timeoutMs = 180_000 } = {}) {
  const t0 = Date.now();
  // Mock Electron-only probes of other lanes do not conflict; a mock ytm-daemon would (MPRIS name).
  const conflicting = () => findAppProcesses().filter((p) => p.realProfile || p.exe === "ytm-daemon");
  let others = conflicting();
  while (others.length) {
    if (others.some((p) => p.realProfile)) {
      throw new Error(`refusing to launch: a real-profile YTMcp instance is running: ${JSON.stringify(others.filter((p) => p.realProfile))}`);
    }
    if (Date.now() - t0 > timeoutMs) {
      throw new Error(`other (mock) YTMcp instances kept running for ${timeoutMs} ms: ${JSON.stringify(others)}`);
    }
    await sleep(2000);
    others = conflicting();
  }
  return Date.now() - t0;
}

class Control {
  constructor(sock) {
    this.sock = sock;
    this.nextId = 1;
    this.pending = new Map();
    let buf = "";
    sock.setEncoding("utf8");
    sock.on("data", (chunk) => {
      buf += chunk;
      let nl;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const msg = JSON.parse(buf.slice(0, nl));
        buf = buf.slice(nl + 1);
        const p = this.pending.get(msg.id);
        if (p) {
          this.pending.delete(msg.id);
          clearTimeout(p.timer);
          if (msg.ok) p.resolve(msg.result);
          else p.reject(new Error(msg.error));
        }
      }
    });
    sock.on("close", () => {
      for (const p of this.pending.values()) {
        clearTimeout(p.timer);
        p.reject(new Error("control socket closed"));
      }
      this.pending.clear();
      this.closed = true;
    });
    sock.on("error", () => {});
  }

  call(op, args = {}, timeoutMs = 30_000) {
    if (this.closed) return Promise.reject(new Error("control socket closed"));
    const id = this.nextId++;
    return new Promise((resolvePromise, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`control op ${op} timed out`));
      }, timeoutMs);
      this.pending.set(id, { resolve: resolvePromise, reject, timer });
      this.sock.write(`${JSON.stringify({ id, op, args })}\n`);
    });
  }

  close() {
    this.sock.destroy();
  }
}

/**
 * A running app instance. `launch()` refuses to start while any other YTMcp
 * process exists (one real-profile instance at a time).
 */
export class AppInstance {
  constructor(opts) {
    this.opts = opts;
    this.logLines = [];
    this.exit = null;
  }

  static async launch({
    label,
    workDir,
    live = true,
    extraArgs = [],
    extraEnv = {},
    daemonBin = DAEMON,
    entry = QA_ENTRY,
    xdg = null,
    scope = true,
    // A startup that blocks in a modal error dialog cannot answer the control ping.
    requirePing = true,
  }) {
    // One instance at a time: other lanes' mock instances would also claim the MPRIS name.
    await waitNoOtherInstances();
    if (live && process.env.YTMCP_ALLOW_LIVE !== "1") {
      throw new Error("refusing live launch without YTMCP_ALLOW_LIVE=1");
    }
    mkdirSync(workDir, { recursive: true, mode: 0o700 });
    // AF_UNIX paths are limited to 108 bytes; the runtime dir is short and private (0700).
    const socket = join(process.env.XDG_RUNTIME_DIR || "/tmp", `ytmqa-${label}-${process.pid}.sock`);
    const logPath = join(workDir, `app-${label}.log`);
    const unit = `ytmcp-qa-${label}-${Date.now()}`;
    const env = {
      ...process.env,
      PULSE_SINK: "ytmcp_null",
      YTMCP_QA_SOCKET: socket,
      YTMCP_QA_MAIN: PROD_MAIN,
      YTMCP_DAEMON_BIN: daemonBin,
      ...extraEnv,
    };
    delete env.YTMCP_SHOW_PLAYER;
    delete env.ELECTRON_RUN_AS_NODE;
    if (xdg) Object.assign(env, xdg);
    const argv = [...extraArgs, entry];
    const cmd = scope ? "systemd-run" : ELECTRON;
    const args = scope
      ? ["--user", "--scope", "--quiet", `--unit=${unit}`, "--description=YTMcp QA run", ELECTRON, ...argv]
      : argv;
    const child = spawn(cmd, args, { cwd: REPO, env, stdio: ["ignore", "pipe", "pipe"] });
    const inst = new AppInstance({ label, workDir, socket, logPath, unit, live, scope });
    inst.child = child;
    inst.startedAtMs = Date.now();
    inst.startedMono = performance.now();
    const log = createWriteStream(logPath, { flags: "w", mode: 0o600 });
    const onData = (stream) => (chunk) => {
      log.write(chunk);
      for (const line of chunk.toString("utf8").split("\n")) {
        if (line.trim()) inst.logLines.push({ t: Date.now(), stream, line });
      }
    };
    child.stdout.on("data", onData("stdout"));
    child.stderr.on("data", onData("stderr"));
    child.on("exit", (code, signal) => {
      inst.exit = { code, signal, atMs: Date.now() };
      log.end();
    });
    // If the harness dies, never leave an instance (and its audio) behind.
    inst.unregisterCleanup = onCleanup(() => {
      for (const u of inst.units()) {
        try {
          execFileSync("systemctl", ["--user", "stop", u], { stdio: "ignore", timeout: 10_000 });
        } catch {
          // already gone
        }
      }
    });
    try {
      inst.ctl = await inst.connectControl();
      const ping = await inst.ctl.call("ping", {}, requirePing ? 30_000 : 4_000);
      inst.mainPid = ping.pid;
    } catch (err) {
      if (requirePing) {
        await inst.kill();
        throw err;
      }
      // `systemd-run --scope` execs Electron, so the spawned pid is the browser process.
      inst.mainPid = inst.child.pid ?? null;
    }
    return inst;
  }

  async connectControl(timeoutMs = 20_000) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      if (this.exit) throw new Error(`app exited before control socket: ${JSON.stringify(this.exit)}`);
      if (existsSync(this.opts.socket)) {
        try {
          const sock = await new Promise((res, rej) => {
            const s = connect(this.opts.socket);
            s.once("connect", () => res(s));
            s.once("error", rej);
          });
          return new Control(sock);
        } catch {
          // retry
        }
      }
      await sleep(100);
    }
    throw new Error("control socket did not appear");
  }

  call(op, args, timeoutMs) {
    return this.ctl.call(op, args, timeoutMs);
  }

  /** Runs JS in the main UI renderer (main world) and returns its JSON result. */
  evalMain(js, timeoutMs) {
    return this.call("eval", { target: "main", js }, timeoutMs);
  }

  evalPlayer(js, { userGesture = false, timeoutMs, worldId = null } = {}) {
    return this.call("eval", { target: "player", js, userGesture, worldId }, timeoutMs);
  }

  state() {
    return this.call("state");
  }

  async snapshot() {
    return (await this.state()).snapshot;
  }

  events(since = 0, types = null) {
    return this.call("events", { since, types }, 30_000);
  }

  /** systemd units holding app processes: the QA scope plus the scope Chromium moves itself into. */
  units() {
    const out = [`${this.opts.unit}.scope`];
    if (this.mainPid) out.push(`app-local.ytmcp.Desktop-${this.mainPid}.scope`);
    return out;
  }

  /** All cgroups currently holding app processes. */
  cgroups() {
    const set = new Set([`/user.slice/user-${process.getuid()}.slice/user@${process.getuid()}.service/app.slice/${this.opts.unit}.scope`]);
    for (const pid of appProcessSet({ rootPid: this.mainPid })) {
      const cg = cgroupOf(pid);
      if (cg) set.add(cg);
    }
    return [...set];
  }

  pids() {
    return appProcessSet({ rootPid: this.mainPid, cgroups: this.cgroups() });
  }

  /** Waits until `pred(snapshot, state)` is truthy; returns {ok, value, waitedMs}. */
  async waitFor(pred, { timeoutMs = 20_000, intervalMs = 250 } = {}) {
    const t0 = Date.now();
    let last = null;
    while (Date.now() - t0 < timeoutMs) {
      try {
        const st = await this.state();
        last = st;
        const v = await pred(st.snapshot, st);
        if (v) return { ok: true, value: v, waitedMs: Date.now() - t0 };
      } catch {
        // app may be reloading
      }
      await sleep(intervalMs);
    }
    return { ok: false, value: null, waitedMs: Date.now() - t0, last };
  }

  async waitForEvent(pred, { since = 0, timeoutMs = 20_000, types = null } = {}) {
    const t0 = Date.now();
    while (Date.now() - t0 < timeoutMs) {
      const evs = await this.events(since, types).catch(() => []);
      const hit = evs.find(pred);
      if (hit) return hit;
      await sleep(200);
    }
    return null;
  }

  /** Clicks a UI element by CSS selector in the real renderer DOM; returns what was clicked. */
  async click(selector, { textIncludes = null } = {}) {
    const js = `(() => {
      const els = [...document.querySelectorAll(${JSON.stringify(selector)})];
      const el = els.find((e) => ${JSON.stringify(textIncludes)} === null || (e.textContent || "").includes(${JSON.stringify(textIncludes)}));
      if (!el) return { clicked: false, reason: "not found", count: els.length };
      if (el.disabled) return { clicked: false, reason: "disabled", title: el.title || null };
      el.scrollIntoView({ block: "center" });
      el.click();
      return { clicked: true, label: el.getAttribute("aria-label"), testid: el.getAttribute("data-testid") };
    })()`;
    return this.evalMain(js);
  }

  /** Sets a range/text input value the way user input does (input + change events). */
  async setInput(selector, value, { change = true } = {}) {
    const js = `(() => {
      const el = document.querySelector(${JSON.stringify(selector)});
      if (!el) return { set: false, reason: "not found" };
      if (el.disabled) return { set: false, reason: "disabled" };
      el.focus();
      const proto = Object.getPrototypeOf(el);
      Object.getOwnPropertyDescriptor(proto, "value").set.call(el, ${JSON.stringify(String(value))});
      el.dispatchEvent(new Event("input", { bubbles: true }));
      ${change ? 'el.dispatchEvent(new Event("change", { bubbles: true }));' : ""}
      return { set: true, value: el.value };
    })()`;
    return this.evalMain(js);
  }

  async exists(selector) {
    return this.evalMain(`Boolean(document.querySelector(${JSON.stringify(selector)}))`);
  }

  async text(selector) {
    return this.evalMain(`(document.querySelector(${JSON.stringify(selector)})?.textContent ?? null)`);
  }

  /**
   * Clicks a UI control and follows the resulting ui.playerCommand through the host IPC
   * guard to the core receipt. Returns the measured lifecycle.
   */
  async clickCommand(selector, { op = null, timeoutMs = 15_000, expect = null, setValue = null } = {}) {
    const since = (await this.state()).seq;
    const clickedAtMs = Date.now();
    const action = setValue == null ? await this.click(selector) : await this.setInput(selector, setValue);
    if (!(action.clicked || action.set)) return { selector, action, sent: false };
    const sent = await this.waitForEvent((e) => e.type === "ui->host" && e.kind === "ui.playerCommand" && (!op || e.op === op), {
      since,
      timeoutMs: 5000,
    });
    if (!sent) return { selector, action, sent: false, reason: "no ui.playerCommand crossed the IPC guard" };
    const t0 = Date.now();
    let final = null;
    let list = [];
    while (Date.now() - t0 < timeoutMs) {
      list = await this.call("receipts", { commandId: sent.commandId });
      final = list.find((r) => ["observed", "failed", "indeterminate", "cancelled"].includes(r.status));
      if (final) break;
      await sleep(150);
    }
    const toBridge = await this.events(since, ["core->bridge"]).then((evs) => evs.find((e) => e.commandId === sent.commandId) ?? null);
    let expectation = null;
    if (expect) {
      const w = await this.waitFor(expect, { timeoutMs: 8000 });
      expectation = { met: w.ok, waitedMs: w.waitedMs };
    }
    const accepted = list.find((r) => r.status === "accepted") ?? list[0] ?? null;
    return {
      selector,
      action,
      sent: true,
      commandId: sent.commandId,
      op: sent.op,
      args: sent.args,
      ipcAck: sent.ack,
      forwardedToBridge: Boolean(toBridge),
      clickedAtMs,
      acceptedAtMs: accepted?.acceptedAtMs ?? null,
      observedAtMs: final?.observedAtMs ?? null,
      finalStatus: final?.status ?? "timeout",
      error: final?.error ?? null,
      clickToObservedMs: final?.observedAtMs ? final.observedAtMs - clickedAtMs : null,
      statuses: list.map((r) => r.status),
      expectation,
    };
  }

  /** Closes the main window like the window-manager close button (quits when no tray). */
  async quitViaClose({ timeoutMs = 20_000 } = {}) {
    const t0 = Date.now();
    this.knownCgroups = this.cgroups();
    try {
      await this.call("winop", { target: "main", op: "close" }, 5000);
    } catch {
      // socket may close during quit
    }
    return this.waitExit(timeoutMs, t0);
  }

  async quit({ timeoutMs = 20_000 } = {}) {
    const t0 = Date.now();
    this.knownCgroups = this.cgroups();
    try {
      await this.call("quit", {}, 5000);
    } catch {
      // ignore
    }
    return this.waitExit(timeoutMs, t0);
  }

  async waitExit(timeoutMs, t0 = Date.now()) {
    this.knownCgroups ??= this.cgroups();
    while (!this.exit && Date.now() - t0 < timeoutMs) await sleep(100);
    // the scope unit disappears once its last process is gone
    let leftovers = [];
    const deadline = Date.now() + 5000;
    do {
      const strays = this.opts.live ? findAppProcesses().filter((p) => p.realProfile).map((p) => p.pid) : [];
      leftovers = [...new Set([...this.knownCgroups.flatMap((cg) => cgroupPids(cg)), ...strays])];
      if (!leftovers.length) break;
      await sleep(200);
    } while (Date.now() < deadline);
    this.ctl?.close();
    if (!leftovers.length) this.unregisterCleanup?.();
    return {
      exited: Boolean(this.exit),
      exit: this.exit,
      quitMs: this.exit ? this.exit.atMs - t0 : null,
      leftoverPids: leftovers,
      leftover: leftovers.map((pid) => ({ pid, exe: (procCmdline(pid)[0] ?? "").split("/").pop() })),
    };
  }

  /** Last resort for a hung instance: stop the whole transient scope. */
  async kill() {
    this.knownCgroups = this.cgroups();
    for (const u of this.units()) await run("systemctl", ["--user", "stop", u]);
    this.child?.kill("SIGKILL");
    // The browser process may have moved itself into a scope that did not exist yet when
    // units() was read (e.g. it is blocked in a startup dialog); kill it directly as well.
    for (const pid of [this.mainPid, ...this.knownCgroups.flatMap((cg) => cgroupPids(cg))]) {
      if (!pid) continue;
      try {
        process.kill(pid, "SIGKILL");
      } catch {
        // gone
      }
    }
    return this.waitExit(10_000);
  }

  logs() {
    return this.logLines;
  }

  readLogFile() {
    try {
      return readFileSync(this.opts.logPath, "utf8");
    } catch {
      return "";
    }
  }
}

/** Waits for the integrated app to report bridge+auth readiness. */
export async function waitAppReady(app, { timeoutMs = 60_000 } = {}) {
  return app.waitFor(
    (s, st) =>
      s &&
      st.lastBridgeReady &&
      s.player?.connection === "ready" &&
      s.authState?.state === "authenticated" &&
      s,
    { timeoutMs }
  );
}
