// G0: environment, versions, release build, startup logs, process tree, initial snapshot,
// and explicit errors for a missing sidecar / protocol mismatch.
import { chmodSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { AppInstance, DAEMON, ELECTRON, PROD_MAIN, REPO, TARGET_DIR } from "../lib/app.mjs";
import { check } from "../lib/evidence.mjs";
import { describeProcesses, fileSha256, firstLine, run, sleep } from "../lib/sys.mjs";

export async function collectEnvironment() {
  const git = async (...a) => (await run("git", ["-C", REPO, ...a])).stdout.trim();
  const kscreen = await run("kscreen-doctor", ["-j"]);
  let display = null;
  try {
    display = JSON.parse(kscreen.stdout).outputs
      .filter((o) => o.enabled)
      .map((o) => {
        const mode = o.modes.find((m) => m.id === o.currentModeId);
        return { name: o.name, scale: o.scale, size: mode?.size ?? null, refreshHz: mode?.refreshRate ?? null, hdr: o.hdr ?? null, vrrPolicy: o.vrrPolicy ?? null };
      });
  } catch {
    display = { error: "kscreen-doctor output unparsable" };
  }
  const pkgs = await run("pacman", ["-Q", "pipewire", "pipewire-pulse", "wireplumber", "kwin", "plasma-workspace", "nvidia-utils", "fcitx5"]);
  return {
    time: new Date().toISOString(),
    timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
    git: { commit: await git("rev-parse", "HEAD"), branch: await git("rev-parse", "--abbrev-ref", "HEAD"), dirtyFiles: (await git("status", "--porcelain")).split("\n").filter(Boolean).length },
    os: (await run("sh", ["-c", '. /etc/os-release && echo "$PRETTY_NAME"'])).stdout.trim(),
    kernel: await firstLine("uname", ["-r"]),
    plasma: await firstLine("plasmashell", ["--version"]),
    kwin: await firstLine("kwin_wayland", ["--version"]),
    session: { XDG_SESSION_TYPE: process.env.XDG_SESSION_TYPE ?? null, WAYLAND_DISPLAY: process.env.WAYLAND_DISPLAY ?? null, XDG_CURRENT_DESKTOP: process.env.XDG_CURRENT_DESKTOP ?? null },
    gpu: await firstLine("nvidia-smi", ["--query-gpu=name,driver_version", "--format=csv,noheader"]),
    display,
    packages: pkgs.stdout.trim().split("\n"),
    pulseServer: (await run("pactl", ["info"])).stdout.split("\n").filter((l) => /Server (Name|Version)/.test(l)).map((l) => l.trim()),
    node: process.version,
    pnpm: await firstLine("pnpm", ["--version"]),
    rustc: await firstLine("rustc", ["--version"]),
    cargo: await firstLine("cargo", ["--version"]),
    electronBinary: await firstLine(ELECTRON, ["--version"]),
    lockfiles: {
      "Cargo.lock": fileSha256(join(REPO, "Cargo.lock")),
      "pnpm-lock.yaml": fileSha256(join(REPO, "pnpm-lock.yaml")),
      "config/app-identity.json": fileSha256(join(REPO, "config/app-identity.json")),
    },
    profile: "existing-app-owned (~/.local/share/ytm-glass)",
    buildMode: "release daemon + esbuild/vite production bundles, unpackaged Electron",
  };
}

export async function buildRelease() {
  // An optional local helper may print a CPU list to pin the build to; without it the build runs unpinned.
  const cpus = (await run(`${process.env.HOME}/.local/bin/workload-cpus`, [])).stdout.trim() || null;
  const steps = [
    ["cargo", ["build", "--release", "-j", "12", "-p", "ytm-daemon"]],
    ["pnpm", ["--filter", "@ytm-glass/bridge", "run", "build"]],
    ["pnpm", ["--filter", "@ytm-glass/ui", "run", "build"]],
    ["pnpm", ["--filter", "@ytm-glass/desktop", "run", "build"]],
  ];
  const results = [];
  for (const [cmd, args] of steps) {
    const [bin, argv] = cpus ? ["taskset", ["-c", cpus, cmd, ...args]] : [cmd, args];
    const r = await run(bin, argv, { timeoutMs: 900_000, env: { ...process.env, CARGO_TARGET_DIR: TARGET_DIR } });
    results.push({ cmd: `${cmd} ${args.join(" ")}`, exit: r.code, ms: r.ms, tail: (r.stderr || r.stdout).trim().split("\n").slice(-2) });
  }
  const artifacts = {
    "ytm-daemon": fileSha256(DAEMON),
    "apps/desktop/dist/main.mjs": fileSha256(PROD_MAIN),
    "apps/desktop/dist/ui-preload.cjs": fileSha256(join(REPO, "apps/desktop/dist/ui-preload.cjs")),
    "apps/desktop/dist/player-preload.cjs": fileSha256(join(REPO, "apps/desktop/dist/player-preload.cjs")),
    "bridge/dist/bridge-probe.mjs": fileSha256(join(REPO, "bridge/dist/bridge-probe.mjs")),
    "ui/dist/index.html": fileSha256(join(REPO, "ui/dist/index.html")),
  };
  return { cpus, steps: results, artifacts };
}

/** Classifies startup log lines into shell / core / player sources. */
export function classifyLogs(lines) {
  const out = { shell: [], core: [], player: [], chromium: [], other: [] };
  for (const { line } of lines) {
    let obj = null;
    if (line.startsWith("{")) {
      try {
        obj = JSON.parse(line);
      } catch {
        obj = null;
      }
    }
    if (obj) {
      const ev = String(obj.event ?? obj.msg ?? "");
      if (ev === "daemon_stderr") out.core.push(String(obj.detail?.line ?? "").replace(/^\S+\s+/, "").slice(0, 160));
      else if (/bridge|player|render-process/.test(ev)) out.player.push(`${obj.component}:${ev}`);
      else out.shell.push(`${obj.component}:${ev}`);
    } else if (/^\[\d+:\d+\//.test(line)) out.chromium.push(line.replace(/^\[[^\]]+\]\s*/, "").slice(0, 160));
    else out.other.push(line.slice(0, 160));
  }
  const uniq = (a) => [...new Set(a)].slice(0, 40);
  return {
    counts: Object.fromEntries(Object.entries(out).map(([k, v]) => [k, v.length])),
    shell: uniq(out.shell),
    core: uniq(out.core),
    player: uniq(out.player),
    chromium: uniq(out.chromium),
    other: uniq(out.other),
  };
}

export function runtimeChecks(app, readyWait, logs) {
  const st = readyWait.last ?? null;
  const evidence = {
    handshake: {
      coreReadyLogged: logs.core.some((l) => l.includes("ytm-daemon ready")),
      snapshotDelivered: (readyWait.state?.snapshotCount ?? 0) > 0,
      coreEpochPresent: Boolean(readyWait.state?.snapshot?.coreEpoch),
      waitedMs: readyWait.waitedMs,
    },
    logSources: logs,
    processTree: describeProcesses(app.pids()),
  };
  const roles = new Set(evidence.processTree.map((p) => p.role));
  return {
    evidence,
    checks: [
      check("G0.handshake", "release ytm-daemon started and handshake completed (core.ready + snapshot to UI)", {
        measured: evidence.handshake,
        pass: evidence.handshake.coreReadyLogged && evidence.handshake.snapshotDelivered && evidence.handshake.coreEpochPresent,
      }),
      check("G0.logs", "startup logs distinguish shell, core and player sources", {
        measured: evidence.logSources.counts,
        pass: evidence.logSources.counts.shell > 0 && evidence.logSources.counts.core > 0 && evidence.logSources.counts.player > 0,
      }),
      check("G0.processTree", "process tree has browser, GPU, renderers and the Rust core", {
        measured: [...roles],
        pass: roles.has("browser(main)") && roles.has("gpu-process") && roles.has("renderer") && roles.has("core:ytm-daemon"),
      }),
    ],
  };
}

/** Real desktop main with a broken sidecar, on a throwaway mock profile. */
async function sidecarFailureRun(ctx, label, daemonBin) {
  const base = join(ctx.mockXdgRoot, label);
  const xdg = {
    XDG_DATA_HOME: join(base, "data"),
    XDG_CONFIG_HOME: join(base, "config"),
    XDG_CACHE_HOME: join(base, "cache"),
    XDG_STATE_HOME: join(base, "state"),
    YTM_GLASS_MODE: "mock",
    YTM_GLASS_WITH_DAEMON: "1",
  };
  let app = null;
  let launchError = null;
  let events = [];
  try {
    app = await AppInstance.launch({ label, workDir: join(ctx.workDir, label), live: false, xdg, daemonBin, requirePing: false });
    const t0 = Date.now();
    while (!app.exit && Date.now() - t0 < 20_000) {
      if (app.mainPid) events = await app.events(0).catch(() => events);
      await sleep(300);
    }
  } catch (err) {
    launchError = String(err?.message ?? err);
  }
  const exitedWithoutIntervention = Boolean(app?.exit);
  const exitInfo = app ? (exitedWithoutIntervention ? await app.waitExit(5000) : await app.kill()) : null;
  const lines = app?.logs() ?? [];
  const errorLines = lines
    .map((l) => l.line)
    .filter((l) => /daemon_binary_missing|startup_failed|mismatch|incompatible|BRIDGE_INCOMPATIBLE/i.test(l))
    .map((l) => l.slice(0, 240));
  const uiErrors = events.filter((e) => e.type === "core->ui" && e.kind === "core.error").map((e) => e.error);
  const snapshots = events.filter((e) => e.type === "core->ui" && e.kind === "core.snapshot").length;
  return {
    label,
    launchError,
    exitCode: exitInfo?.exit?.code ?? null,
    exited: Boolean(exitInfo?.exited),
    leftoverPids: exitInfo?.leftoverPids ?? null,
    errorLines,
    coreErrorsSentToUi: uiErrors,
    coreSnapshotsSentToUi: snapshots,
    exitedWithoutIntervention,
  };
}

export async function sidecarFailureChecks(ctx) {
  const missing = await sidecarFailureRun(ctx, "g0-missing-sidecar", "/nonexistent/ytm-daemon");
  const fake = join(ctx.workDir, "fake-daemon-v2.mjs");
  writeFileSync(
    fake,
    `#!${process.execPath}
// QA fake sidecar that answers host.hello as protocol v2.
let buf = "";
process.stdin.on("data", (c) => {
  buf += c;
  let nl;
  while ((nl = buf.indexOf("\\n")) >= 0) {
    const line = buf.slice(0, nl); buf = buf.slice(nl + 1);
    let env; try { env = JSON.parse(line); } catch { continue; }
    if (env.kind === "host.hello") {
      process.stdout.write(JSON.stringify({ protocolVersion: 2, kind: "core.ready", coreEpoch: "core-fake", accountEpoch: null, bridgeEpoch: null, requestId: env.requestId, seq: 1, payload: { coreEpoch: "core-fake", protocolVersion: 2 } }) + "\\n");
    }
  }
});
`
  );
  chmodSync(fake, 0o755);
  const mismatch = await sidecarFailureRun(ctx, "g0-protocol-mismatch", fake);
  const explicit = (r) => r.errorLines.length > 0 && r.coreSnapshotsSentToUi === 0 && r.exited;
  const cleanFailure = (r) => r.exitedWithoutIntervention && r.exitCode === 1
    && r.leftoverPids?.length === 0 && r.errorLines.some((line) => line.includes('"event":"startup_failed"'));
  return {
    evidence: { missingSidecar: missing, protocolMismatch: mismatch, note: "process tests on a throwaway mock profile through the real desktop main; supporting evidence" },
    checks: [
      check("G0.missingSidecar", "missing ytm-daemon gives an explicit error and no JS fallback (process test)", {
        measured: missing,
        pass: explicit(missing),
      }),
      check("G0.protocolMismatch", "protocol v2 sidecar is rejected explicitly (process test)", {
        measured: mismatch,
        pass: explicit(mismatch),
      }),
      check("G0.startupFailureExits", "sidecar failure is logged and exits with code 1 without intervention or orphan processes", {
        measured: { missing: cleanFailure(missing), mismatch: cleanFailure(mismatch) },
        pass: cleanFailure(missing) && cleanFailure(mismatch),
        support: true,
      }),
    ],
  };
}

