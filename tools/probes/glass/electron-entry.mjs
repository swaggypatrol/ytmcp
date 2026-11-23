// Electron main process for the liquid-glass probe. See run-probe.mjs for usage.
//   behavior          a synthetic page that pins down the Chromium behaviour the material relies on
//   shots/look/perf   the real application in mock mode (real-ui.mjs)
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { app, BrowserWindow, screen } from "electron";
import { runLook, runPerf, runShots } from "./real-ui.mjs";

const mode = process.env.GLASS_PROBE_MODE ?? "shots";
const modeArgs = JSON.parse(process.env.GLASS_PROBE_ARGS ?? "[]");
const outDir = process.env.GLASS_PROBE_OUT ?? join(import.meta.dirname, "out");
const here = import.meta.dirname;
mkdirSync(outDir, { recursive: true });

app.enableSandbox();
app.on("window-all-closed", () => {});

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function openWindow(width, height) {
  return new BrowserWindow({
    width,
    height,
    useContentSize: true,
    show: true,
    autoHideMenuBar: true,
    backgroundColor: "#101114",
    webPreferences: { sandbox: true, contextIsolation: true, nodeIntegration: false, webSecurity: true },
  });
}

// Wraps a capturePage result as raw pixels addressed in CSS px.
async function grab(win, rect) {
  const image = await win.webContents.capturePage(rect);
  const bitmap = image.toBitmap();
  // getSize() reports device pixels here; derive the scale from the captured CSS width.
  const pw = image.getSize().width;
  const ph = bitmap.length / 4 / pw;
  const cssWidth = rect?.width ?? win.getContentSize()[0];
  const scale = pw / cssWidth;
  const ox = rect?.x ?? 0;
  const oy = rect?.y ?? 0;
  return {
    image,
    scale,
    pw,
    ph,
    // BGRA in memory on little-endian Linux.
    at(xCss, yCss) {
      const px = Math.min(pw - 1, Math.max(0, Math.floor((xCss - ox) * scale)));
      const py = Math.min(ph - 1, Math.max(0, Math.floor((yCss - oy) * scale)));
      const i = (py * pw + px) * 4;
      return [bitmap[i + 2], bitmap[i + 1], bitmap[i]];
    },
    devicePixel(px, py) {
      const i = (py * pw + px) * 4;
      return [bitmap[i + 2], bitmap[i + 1], bitmap[i]];
    },
  };
}

function savePng(image, name, maxWidth) {
  const size = image.getSize();
  const out = maxWidth && size.width > maxWidth ? image.resize({ width: maxWidth, quality: "best" }) : image;
  writeFileSync(join(outDir, name), out.toPNG());
}

async function environmentReport(win) {
  const display = screen.getPrimaryDisplay();
  const gpu = await app.getGPUInfo("basic").catch(() => null);
  return {
    electron: process.versions.electron,
    chromium: process.versions.chrome,
    ozonePlatform: app.commandLine.getSwitchValue("ozone-platform") || "(default)",
    xdgSessionType: process.env.XDG_SESSION_TYPE ?? null,
    waylandDisplay: process.env.WAYLAND_DISPLAY ?? null,
    display: {
      size: display.size,
      scaleFactor: display.scaleFactor,
      reportedHz: display.displayFrequency,
      colorSpace: display.colorSpace,
      colorDepth: display.colorDepth,
    },
    gpuFeatureStatus: app.getGPUFeatureStatus(),
    gpuDevice: gpu?.gpuDevice?.find((d) => d.active) ?? gpu?.gpuDevice?.[0] ?? null,
    auxAttributes: gpu?.auxAttributes
      ? { glRenderer: gpu.auxAttributes.glRenderer, displayType: gpu.auxAttributes.displayType, skiaBackendType: gpu.auxAttributes.skiaBackendType }
      : null,
    rendererDpr: await win.webContents.executeJavaScript("window.devicePixelRatio"),
  };
}

// ---------------------------------------------------------------------------------------------
// behaviour matrix
// ---------------------------------------------------------------------------------------------

async function runBehavior() {
  const win = openWindow(1504, 900);
  await win.loadFile(join(here, "behavior.html"));
  await sleep(600);
  const info = await win.webContents.executeJavaScript("({ box: __behavior.box, cases: __behavior.cases, supports: __behavior.supports })");
  const origins = {};
  for (const id of info.cases) origins[id] = await win.webContents.executeJavaScript(`__behavior.rectOf(${JSON.stringify(id)})`);
  const B = info.box;
  const cy = B.y + B.h / 2;

  const mean = (a) => a.reduce((s, v) => s + v, 0) / a.length;
  const round = (v, d = 2) => Number(v.toFixed(d));

  // Mean shift (in CSS px) of the ramp pattern inside the box, away from its edges.
  function rampShift(shot, id, x0 = B.x + 20, x1 = B.x + B.w - 60, y = cy) {
    const o = origins[id];
    const dx = [];
    const dy = [];
    for (let x = x0; x <= x1; x += 4) {
      const [r, g] = shot.at(o.x + x + 0.25, o.y + y + 0.25);
      dx.push(r - x);
      dy.push(g - y);
    }
    return { dx: round(mean(dx)), dy: round(mean(dy)) };
  }

  // 10%–90% rise width of the black/white step, in CSS px, scanned in device pixels.
  function edgeWidth(shot, id, y = cy) {
    const o = origins[id];
    const py = Math.floor((o.y + y) * shot.scale);
    const pxStart = Math.floor((o.x + B.x + 10) * shot.scale);
    const pxEnd = Math.floor((o.x + B.x + B.w - 10) * shot.scale);
    let first = null;
    let last = null;
    for (let px = pxStart; px <= pxEnd; px++) {
      const v = shot.devicePixel(px, py)[0];
      if (first === null && v > 25) first = px;
      if (last === null && v > 230) last = px;
    }
    return first === null || last === null ? null : round((last - first) / shot.scale);
  }

  function swatches(shot, id, y) {
    const o = origins[id];
    return [60, 100, 140, 180].map((x) => shot.at(o.x + x, o.y + y));
  }

  const shot = await grab(win);
  const results = {
    reference: rampShift(shot, "reference"),
    floodConst: rampShift(shot, "flood-const"),
    imageConst: rampShift(shot, "image-const"),
    imageHalf: {
      leftHalf: rampShift(shot, "image-half", B.x + 10, B.x + 60),
      rightHalf: rampShift(shot, "image-half", B.x + 90, B.x + 130),
    },
    mapSrgb: rampShift(shot, "map-srgb"),
    mapLinear: rampShift(shot, "map-linear"),
    xy: rampShift(shot, "xy", B.x + 20, B.x + B.w - 60, B.y + 20),
    lowresRamp: [20, 50, 80, 110, 140].map((dx) => {
      const o = origins["lowres-ramp"];
      return { x: dx, shift: shot.at(o.x + B.x + dx + 0.25, o.y + cy)[0] - (B.x + dx) };
    }),
    lowresRampFine: Array.from({ length: 21 }, (_, k) => {
      const o = origins["lowres-ramp"];
      const x = B.x + 60 + k;
      return shot.at(o.x + x + 0.25, o.y + cy)[0] - x;
    }),
    radius: {
      center: rampShift(shot, "radius"),
      cornerPixelShiftX: shot.at(origins.radius.x + B.x + 4, origins.radius.y + B.y + 4)[0] - (B.x + 4),
    },
    defaultRegion: rampShift(shot, "default-region"),
    // Samples whose source lies 10 px beyond the right edge of the box (pattern continues there).
    outOfBounds: (() => {
      const o = origins["image-const"];
      const x = B.x + B.w - 10;
      return { at: x, expectedIfOutsideContentVisible: x + 20, rgb: shot.at(o.x + x + 0.25, o.y + cy) };
    })(),
    outOfBoundsStrip: Array.from({ length: 13 }, (_, k) => {
      const o = origins["image-const"];
      const x = B.x + B.w - 26 + k * 2;
      return { x, r: shot.at(o.x + x + 0.25, o.y + cy)[0] };
    }),
    passSrgb: { inside: swatches(shot, "pass-srgb", cy), outside: swatches(shot, "pass-srgb", 12) },
    passLinear: { inside: swatches(shot, "pass-linear", cy), outside: swatches(shot, "pass-linear", 12) },
    saturateZero: { inside: swatches(shot, "ramp-url-saturate", cy), outside: swatches(shot, "ramp-url-saturate", 12) },
    tintOver: { inside: swatches(shot, "tint-over", cy), outside: swatches(shot, "tint-over", 12) },
    edgeWidthCssPx: {
      none: edgeWidth(shot, "edge-none"),
      blurOnly: edgeWidth(shot, "edge-blur"),
      urlThenBlur: edgeWidth(shot, "edge-url-then-blur"),
      blurThenUrl: edgeWidth(shot, "edge-blur-then-url"),
      nestedChild: edgeWidth(shot, "nested-child"),
      nestedChildOutsideChildRow: edgeWidth(shot, "nested-child", B.y + 4),
      siblingPlate: edgeWidth(shot, "sibling-plate"),
    },
    rampUrlThenBlur: rampShift(shot, "ramp-url-then-blur"),
    // Displacement (CSS px) at depth d from each edge of the box for the bitmap-free fields.
    procedural: Object.fromEntries(["proc-convolve", "proc-convolve-unit", "proc-convolve-dpr", "proc-light"].map((id) => {
      const o = origins[id];
      const depths = [1, 4, 8, 12, 16, 24];
      const px = (x, y) => shot.at(o.x + x + 0.25, o.y + y + 0.25);
      return [id, {
        depths,
        left: depths.map((d) => px(B.x + d, cy)[0] - (B.x + d)),
        right: depths.map((d) => px(B.x + B.w - 1 - d, cy)[0] - (B.x + B.w - 1 - d)),
        top: depths.map((d) => px(B.x + B.w / 2, B.y + d)[1] - (B.y + d)),
        bottom: depths.map((d) => px(B.x + B.w / 2, B.y + B.h - 1 - d)[1] - (B.y + B.h - 1 - d)),
        centre: [px(B.x + B.w / 2, cy)[0] - (B.x + B.w / 2), px(B.x + B.w / 2, cy)[1] - cy],
      }];
    })),
    // Ramp value (R = x) seen 1 px and 12 px inside the left/right box edges after a 10 px blur.
    // A kernel that reads transparent black beyond the box darkens both edges.
    blurEdges: Object.fromEntries(["svgblur-box", "svgblur-wide", "svgblur-duplicate", "cssblur"].map((id) => {
      const o = origins[id];
      const read = (x) => shot.at(o.x + x + 0.25, o.y + cy)[0];
      return [id, { leftEdge: read(B.x + 1), leftInner: read(B.x + 12), center: read(B.x + B.w / 2), rightInner: read(B.x + B.w - 12), rightEdge: read(B.x + B.w - 1), expected: [B.x + 1, B.x + 12, B.x + B.w / 2, B.x + B.w - 12, B.x + B.w - 1] }];
    })),
    dynamicBefore: rampShift(shot, "dynamic"),
    blobBefore: rampShift(shot, "blob-url"),
  };
  savePng(shot.image, `behavior-dsf${Math.round(shot.scale * 100) / 100}.png`);

  await win.webContents.executeJavaScript("__behavior.setDynamicScale(80); __behavior.useBlobUrl()");
  await sleep(500);
  const shot2 = await grab(win);
  results.dynamicAfterScale80 = rampShift(shot2, "dynamic");
  results.blobAfter = rampShift(shot2, "blob-url");

  const report = { environment: await environmentReport(win), captureScale: shot.scale, supports: info.supports, results };
  const name = `behavior-dsf${Math.round(shot.scale * 100) / 100}.json`;
  writeFileSync(join(outDir, name), JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report, null, 1));
  win.destroy();
}

const shotsEvidenceDir = process.env.GLASS_PROBE_SHOTS_DIR;
const modes = {
  behavior: runBehavior,
  shots: (args) => runShots(shotsEvidenceDir ?? outDir, { quick: args.includes("quick") }),
  look: (args) => runLook(shotsEvidenceDir ? join(shotsEvidenceDir, "look") : outDir, args[0] ?? "dark"),
  perf: (args) => runPerf(process.env.GLASS_PROBE_PERF_FILE ?? join(outDir, "perf.json"), Number(args.find((a) => /^\d+$/.test(a)) ?? 60), args.includes("collapse") ? "collapse" : null),
};

// The real-UI modes import the desktop shell, which must register its scheme and profile path
// before Electron is ready; they therefore start immediately and wait for readiness themselves.
const run = modes[mode];
const started = mode === "behavior" ? app.whenReady().then(() => run(modeArgs)) : run?.(modeArgs);
Promise.resolve(started ?? Promise.reject(new Error(`unknown mode ${mode}`))).then(
  () => app.exit(0),
  (err) => {
    console.error(err);
    app.exit(1);
  }
);
