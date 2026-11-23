// A throwaway nested KWin (virtual framebuffer) at a chosen output scale, for display-scale
// checks that must not touch the real session: it gets its own D-Bus session bus and its own
// XDG config/cache/data dirs (KWin writes kwinoutputconfig.json and kwinrc on start), renders
// off-screen, and is torn down with its whole process group. KWin's --scale only enlarges the
// virtual framebuffer, so the output scale is set with kscreen-doctor inside that private
// session, after checking it sees nothing but the virtual output.
import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { onCleanup, sleep } from "./sys.mjs";

/** Starts the nested compositor; resolves to { socket, bus, scale, stop() }. */
export async function startNestedKwin({ dir, scale = 1, width = 1600, height = 1000, label = "scale" }) {
  // width/height are logical; the framebuffer is that times the scale.
  const fbWidth = Math.round(width * scale);
  const fbHeight = Math.round(height * scale);
  rmSync(dir, { recursive: true, force: true });
  for (const d of ["config", "cache", "data"]) mkdirSync(join(dir, d), { recursive: true, mode: 0o700 });
  const runtime = process.env.XDG_RUNTIME_DIR || "/tmp";
  const socket = `ytmqa-wl-${label}-${process.pid}`;
  const busFile = join(dir, "bus.txt");
  const env = {
    ...process.env,
    XDG_CONFIG_HOME: join(dir, "config"),
    XDG_CACHE_HOME: join(dir, "cache"),
    XDG_DATA_HOME: join(dir, "data"),
  };
  delete env.WAYLAND_DISPLAY;
  delete env.DISPLAY;
  const inner = `printf %s "$DBUS_SESSION_BUS_ADDRESS" > "$BUS_FILE"; exec kwin_wayland --virtual --socket "$SOCK" --width ${fbWidth} --height ${fbHeight} --scale 1 --no-lockscreen --no-global-shortcuts --no-kactivities`;
  const child = spawn("dbus-run-session", ["--", "sh", "-c", inner], {
    env: { ...env, BUS_FILE: busFile, SOCK: socket },
    stdio: ["ignore", "ignore", "pipe"],
    detached: true,
  });
  let stderr = "";
  child.stderr.on("data", (c) => {
    if (stderr.length < 20_000) stderr += c;
  });
  let exited = null;
  child.on("exit", (code, signal) => {
    exited = { code, signal };
  });
  const stop = async () => {
    if (!exited) {
      try {
        process.kill(-child.pid, "SIGTERM");
      } catch {
        // gone
      }
      const t0 = Date.now();
      while (!exited && Date.now() - t0 < 5000) await sleep(100);
      if (!exited) {
        try {
          process.kill(-child.pid, "SIGKILL");
        } catch {
          // gone
        }
      }
    }
    const t1 = Date.now();
    while (existsSync(join(runtime, socket)) && Date.now() - t1 < 3000) await sleep(100);
    const socketGone = !existsSync(join(runtime, socket));
    rmSync(dir, { recursive: true, force: true });
    unregister();
    return { stopped: Boolean(exited), socketGone };
  };
  const unregister = onCleanup(() => {
    try {
      process.kill(-child.pid, "SIGKILL");
    } catch {
      // gone
    }
  });
  const t0 = Date.now();
  while (Date.now() - t0 < 15_000 && !exited) {
    if (existsSync(join(runtime, socket)) && existsSync(busFile) && readFileSync(busFile, "utf8")) break;
    await sleep(150);
  }
  if (exited || !existsSync(join(runtime, socket))) {
    await stop();
    throw new Error(`nested kwin did not start: ${JSON.stringify(exited)} ${stderr.slice(-400)}`);
  }
  const bus = readFileSync(busFile, "utf8");
  const nestedEnv = { ...process.env, WAYLAND_DISPLAY: socket, DBUS_SESSION_BUS_ADDRESS: bus, QT_QPA_PLATFORM: "wayland" };
  const doctor = (args) => execFileSync("kscreen-doctor", args, { env: nestedEnv, timeout: 15_000 }).toString().replace(/\x1b\[[0-9;]*m/g, "");
  let outputs = [];
  try {
    outputs = [...doctor(["-o"]).matchAll(/^Output: \d+ (\S+)/gm)].map((m) => m[1]);
    if (!outputs.length || outputs.some((o) => !o.startsWith("Virtual-"))) throw new Error(`unexpected outputs ${outputs.join(",")}`);
    if (scale !== 1) doctor([`output.${outputs[0]}.scale.${scale}`]);
  } catch (err) {
    await stop();
    throw new Error(`nested kwin output setup refused: ${String(err?.message ?? err).slice(0, 300)}`);
  }
  return { socket, bus, scale, width, height, outputs, stop, stderr: () => stderr };
}
