// Electron entry: a small window that records what the wheel delivers on this machine.
//
//   node tools/probes/input/run-wheel-log.mjs [seconds]
//
// Scroll over the window with the mouse (and a touchpad, if there is one). It closes by itself
// and writes every wheel event to tools/probes/input/out/wheel.json: deltaY, deltaMode,
// wheelDeltaY, the modifier keys and the time. Nothing else is read and nothing leaves the machine.
import { app, BrowserWindow, ipcMain } from "electron";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

const out = process.env.WHEEL_LOG_OUT;
const seconds = Number(process.env.WHEEL_LOG_SECONDS ?? 20);

const page = `<!doctype html><meta charset="utf-8"><title>Wheel log</title>
<style>
  body { margin: 0; height: 100vh; display: grid; place-items: center; background: #1c1a1f; color: #eee; font: 15px/1.5 system-ui, sans-serif; }
  main { text-align: center; }
  b { font-size: 28px; font-variant-numeric: tabular-nums; }
  pre { color: #aaa; font-size: 12px; min-height: 6em; }
</style>
<main>
  <p>Scroll the wheel here, one notch at a time, then a few fast turns each way.</p>
  <p><b id="n">0</b> events · closes in <b id="t">${seconds}</b> s</p>
  <pre id="last"></pre>
</main>
<script>
  const { ipcRenderer } = require("electron");
  const events = [];
  addEventListener("wheel", (e) => {
    e.preventDefault();
    const row = { t: Math.round(e.timeStamp * 10) / 10, deltaY: e.deltaY, deltaX: e.deltaX, deltaMode: e.deltaMode,
      wheelDeltaY: e.wheelDeltaY, wheelDeltaX: e.wheelDeltaX, ctrl: e.ctrlKey, shift: e.shiftKey };
    events.push(row);
    n.textContent = events.length;
    last.textContent = events.slice(-5).map((r) => JSON.stringify(r)).join("\\n");
  }, { passive: false });
  let left = ${seconds};
  setInterval(() => { t.textContent = --left; if (left <= 0) ipcRenderer.send("done", { dpr: devicePixelRatio, events }); }, 1000);
</script>`;

app.whenReady().then(() => {
  const win = new BrowserWindow({ width: 560, height: 360, webPreferences: { nodeIntegration: true, contextIsolation: false } });
  ipcMain.once("done", (_event, result) => {
    mkdirSync(dirname(out), { recursive: true });
    writeFileSync(out, JSON.stringify({ electron: process.versions.electron, chrome: process.versions.chrome,
      sessionType: process.env.XDG_SESSION_TYPE ?? null, ...result }, null, 2));
    app.quit();
  });
  win.loadURL(`data:text/html;charset=utf-8,${encodeURIComponent(page)}`);
});
app.on("window-all-closed", () => app.quit());
