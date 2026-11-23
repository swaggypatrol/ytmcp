// Drives the real application (apps/desktop/dist + ui/dist, YTM_GLASS_MODE=mock) for the
// liquid-glass evidence: screenshots, contrast measured on those screenshots, frame pacing.
// Loaded by electron-entry.mjs; see run-probe.mjs for usage.
import { mkdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { app, screen } from "electron";

const repoRoot = resolve(import.meta.dirname, "../../..");
const WINDOW = { width: 1280, height: 800 };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------------------------------------------------------------------------------------------
// page-side helpers (run in the UI's main world through executeJavaScript)
// ---------------------------------------------------------------------------------------------

const PAGE_HELPERS = `(() => {
  const $ = (id) => document.querySelector('[data-testid="' + id + '"]');
  const main = () => $("main-scroll-region");
  const list = () => $("virtual-track-list");
  const style = document.createElement("style");
  // Glyphs on glass and on the hero become transparent so a capture shows what sits behind them.
  style.textContent =
    ':root[data-probe-hide-text] :is(.lg, .page-hero), :root[data-probe-hide-text] :is(.lg, .page-hero) * { color: transparent !important; }' +
    '#probe-stress { position: fixed; inset: 0; z-index: 5; pointer-events: none; }';
  document.head.append(style);

  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = 1;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  function rgb(css) {
    ctx.clearRect(0, 0, 1, 1);
    ctx.fillStyle = css;
    ctx.fillRect(0, 0, 1, 1);
    const d = ctx.getImageData(0, 0, 1, 1).data;
    return d[3] === 255 ? [d[0], d[1], d[2]] : null;
  }
  function clip(r, box) {
    const x0 = Math.max(r.left, box.left), y0 = Math.max(r.top, box.top);
    const x1 = Math.min(r.right, box.right), y1 = Math.min(r.bottom, box.bottom);
    return x1 - x0 >= 1 && y1 - y0 >= 1 ? [x0, y0, x1 - x0, y1 - y0] : null;
  }
  const viewport = () => ({ left: 0, top: 0, right: innerWidth, bottom: innerHeight });

  // Every glyph run and icon inside the given hosts, with its colour and the WCAG ratio it owes:
  // 4.5 for body text, 3 for large text (24 px, or 18.66 px bold) and for icons.
  function labels(hostSelector) {
    const out = [];
    for (const host of document.querySelectorAll(hostSelector)) {
      if (!host.checkVisibility({ opacityProperty: true, visibilityProperty: true })) continue;
      const hostBox = clip(host.getBoundingClientRect(), viewport());
      if (!hostBox) continue;
      const hostRect = { left: hostBox[0], top: hostBox[1], right: hostBox[0] + hostBox[2], bottom: hostBox[1] + hostBox[3] };
      const name = host.dataset.testid || host.className.split(" ")[0];
      // The part of a box that is really on screen: inside the host and every clipping ancestor.
      const visiblePart = (rect, el) => {
        let box = clip(rect, hostRect);
        for (let node = el; box && node && node !== host; node = node.parentElement) {
          const cs = getComputedStyle(node);
          if (cs.overflowX !== "visible" || cs.overflowY !== "visible") {
            box = clip({ left: box[0], top: box[1], right: box[0] + box[2], bottom: box[1] + box[3] }, node.getBoundingClientRect());
          }
        }
        return box;
      };
      const push = (el, rect, text, min, kind) => {
        if (el.closest(":disabled, [aria-disabled='true']")) return;
        const visible = visiblePart(rect, el);
        if (!visible) return;
        // Skip what lies under something else (a dialog's scrim dims the surfaces behind it).
        const top = document.elementFromPoint(visible[0] + visible[2] / 2, visible[1] + visible[3] / 2);
        const color = rgb(getComputedStyle(el).color);
        if (top && host.contains(top) && color) out.push({ host: name, kind, text, min, color, rect: visible });
      };
      const walker = document.createTreeWalker(host, NodeFilter.SHOW_TEXT);
      for (let node = walker.nextNode(); node; node = walker.nextNode()) {
        const text = node.textContent.trim();
        const el = node.parentElement;
        if (!text || !el || el.closest("svg, option, select, style, script")) continue;
        if (!el.checkVisibility({ opacityProperty: true, visibilityProperty: true })) continue;
        const cs = getComputedStyle(el);
        const size = parseFloat(cs.fontSize);
        const large = size >= 24 || (size >= 18.66 && Number(cs.fontWeight) >= 700);
        const range = document.createRange();
        range.selectNodeContents(node);
        for (const rect of range.getClientRects()) push(el, rect, text.slice(0, 24), large ? 3 : 4.5, large ? "large-text" : "text");
      }
      for (const icon of host.querySelectorAll("svg.icon, button > svg[stroke='currentColor']")) {
        if (icon.checkVisibility({ opacityProperty: true, visibilityProperty: true })) push(icon, icon.getBoundingClientRect(), "icon", 3, "icon");
      }
    }
    return out;
  }

  // Scroll driven from the main thread, one position per frame, eased round trips.
  function measureScroll(el, seconds, rangePx, legMs) {
    return new Promise((done) => {
      const max = Math.min(rangePx, el.scrollHeight - el.clientHeight);
      const base = Math.min(el.scrollTop, el.scrollHeight - el.clientHeight - max);
      const intervals = [];
      let start = 0, prev = 0;
      function frame(now) {
        if (!start) { start = prev = now; requestAnimationFrame(frame); return; }
        intervals.push(now - prev);
        prev = now;
        const t = now - start;
        el.scrollTop = base + max * (0.5 - 0.5 * Math.cos((Math.PI * t) / legMs));
        if (t < seconds * 1000) requestAnimationFrame(frame);
        else done({ intervals, elapsedMs: t, rangePx: max });
      }
      requestAnimationFrame(frame);
    });
  }
  function measureIdle(seconds) {
    return new Promise((done) => {
      const intervals = [];
      let start = 0, prev = 0;
      function frame(now) {
        if (!start) { start = prev = now; requestAnimationFrame(frame); return; }
        intervals.push(now - prev);
        prev = now;
        if (now - start < seconds * 1000) requestAnimationFrame(frame); else done(intervals);
      }
      requestAnimationFrame(frame);
    });
  }

  window.__glassProbe = {
    has: (id) => $(id) !== null,
    click: (id) => { const el = $(id); if (!el) throw new Error("no element " + id); el.click(); return true; },
    clickFirst: (selector) => { const el = document.querySelector(selector); if (!el) throw new Error("no element " + selector); el.click(); return true; },
    select: (id, value) => { const el = $(id); el.value = value; el.dispatchEvent(new Event("change", { bubbles: true })); return true; },
    check: (id, on) => { const el = $(id); if (el.checked !== on) el.click(); return el.checked; },
    root: () => ({ ...document.documentElement.dataset }),
    centre: (id) => { const r = $(id).getBoundingClientRect(); return { x: Math.round(r.left + r.width / 2), y: Math.round(r.top + r.height / 2) }; },
    styleOf: (id, prop) => getComputedStyle($(id))[prop],
    inlineVar: (id, name) => $(id).style.getPropertyValue(name),
    surfaces: () => [...document.querySelectorAll(".lg")].map((el) => {
      const cs = getComputedStyle(el);
      return { testid: el.dataset.testid ?? null, lens: "lgLens" in el.dataset, backdrop: cs.backdropFilter, background: cs.backgroundColor, rect: (({ x, y, width, height }) => [x, y, width, height])(el.getBoundingClientRect()) };
    }),
    filters: () => document.querySelectorAll("filter[id^=lg-lens-]").length,
    labels,
    hideText: (on) => { document.documentElement.toggleAttribute("data-probe-hide-text", on); return on; },
    // An adversarial layer between the page and the glass (null removes it).
    stress: (background) => {
      document.getElementById("probe-stress")?.remove();
      if (background) { const el = document.createElement("div"); el.id = "probe-stress"; el.style.background = background; main().append(el); }
      return true;
    },
    // The mock feed pins two playlists; repeat the cards so the shelf is long enough to scroll.
    lengthenShelf: (times) => {
      const shelf = $("cover-shelf");
      const cards = [...shelf.children];
      for (let i = 1; i < times; i++) for (const card of cards) { const copy = card.cloneNode(true); copy.removeAttribute("data-testid"); shelf.append(copy); }
      return shelf.children.length;
    },
    scrollShelf: (x) => { $("cover-shelf").scrollLeft = x; return $("cover-shelf").scrollLeft; },
    scrollMain: (y) => { main().scrollTop = y; return main().scrollTop; },
    scrollList: (y) => { list().scrollTop = y; return list().scrollTop; },
    geometry: () => {
      const r = (el) => el ? (({ x, y, width, height }) => ({ x, y, width, height }))(el.getBoundingClientRect()) : null;
      return {
        mainScroll: { top: main().scrollTop, max: main().scrollHeight - main().clientHeight },
        list: r(list()), listRows: list()?.scrollHeight ?? 0,
        bar: r($("player-bar")), sidebar: r($("app-sidebar")), dialog: r($("accessible-dialog")),
        dpr: devicePixelRatio,
      };
    },
    measureIdle,
    measureMain: (seconds, rangePx, legMs) => measureScroll(main(), seconds, rangePx, legMs),
    measureList: (seconds, rangePx, legMs) => measureScroll(list(), seconds, rangePx, legMs),
  };
  return true;
})()`;

// ---------------------------------------------------------------------------------------------
// app control
// ---------------------------------------------------------------------------------------------

/** Starts the desktop shell exactly as `just dev-mock` does and returns its main window. */
export async function launchApp() {
  if (process.env.YTM_GLASS_MODE !== "mock") throw new Error("real-ui probe only runs with YTM_GLASS_MODE=mock");
  const created = new Promise((r) => app.once("browser-window-created", (_event, win) => r(win)));
  await import(pathToFileURL(join(repoRoot, "apps/desktop/dist/main.mjs")).href);
  const win = await created;
  if (win.webContents.isLoading()) await new Promise((r) => win.webContents.once("did-finish-load", r));
  win.setContentSize(WINDOW.width, WINDOW.height);
  await js(win, PAGE_HELPERS);
  await js(win, "document.fonts.ready.then(() => true)");
  await until(win, '__glassProbe.has("view-home") && __glassProbe.has("cover-shelf")');
  await sleep(600);
  return win;
}

const js = (win, code) => win.webContents.executeJavaScript(code);

async function until(win, expression, timeoutMs = 8000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await js(win, `Boolean(${expression})`)) return;
    await sleep(60);
  }
  throw new Error(`timed out waiting for ${expression}`);
}

async function navigate(win, view) {
  await js(win, `__glassProbe.click("nav-${view}")`);
  // One shots run timed out here once (not reproduced); a second click is harmless.
  await until(win, `__glassProbe.has("view-${view}")`, 4000).catch(() => js(win, `__glassProbe.click("nav-${view}")`));
  await until(win, `__glassProbe.has("view-${view}")`);
  await sleep(250);
}

/** Changes appearance through the real settings page (theme, reduce transparency, reduce motion). */
async function setAppearance(win, { theme, reduceTransparency, reduceMotion }) {
  await navigate(win, "settings");
  await until(win, '__glassProbe.has("setting-theme-select")');
  if (theme) await js(win, `__glassProbe.select("setting-theme-select", ${JSON.stringify(theme)})`);
  if (reduceTransparency !== undefined) await js(win, `__glassProbe.check("setting-reduce-transparency", ${reduceTransparency})`);
  if (reduceMotion !== undefined) await js(win, `__glassProbe.check("setting-reduce-motion", ${reduceMotion})`);
  await sleep(350);
  return js(win, "__glassProbe.root()");
}

/** Opens the first mock playlist with the 10,000-row fixture and puts the list under the bar. */
async function openLongList(win, listScrollTop = 2000) {
  await navigate(win, "library");
  await until(win, '__glassProbe.has("playlist-card-mock-pl-favorites")');
  await js(win, '__glassProbe.click("playlist-card-mock-pl-favorites")');
  await until(win, '__glassProbe.has("view-playlist") && __glassProbe.has("virtual-track-list")');
  await sleep(300);
  // The store keeps the fixture once loaded, and then the button is gone.
  if (await js(win, '__glassProbe.has("btn-load-10k-fixture")')) await js(win, '__glassProbe.click("btn-load-10k-fixture")');
  await until(win, "__glassProbe.geometry().listRows > 500000");
  await sleep(300);
  const g = await js(win, "__glassProbe.geometry()");
  // Scroll the page until the list card's lower edge sits at the bottom gap, i.e. the card
  // crosses the whole dock; clamp to what the page allows.
  const wanted = g.mainScroll.top + (g.list.y + g.list.height) - (g.bar.y + g.bar.height);
  await js(win, `__glassProbe.scrollMain(${Math.max(0, Math.round(wanted))})`);
  await js(win, `__glassProbe.scrollList(${listScrollTop})`);
  await sleep(350);
  return js(win, "__glassProbe.geometry()");
}

async function emulateMedia(win, features) {
  if (!win.webContents.debugger.isAttached()) win.webContents.debugger.attach("1.3");
  await win.webContents.debugger.sendCommand("Emulation.setEmulatedMedia", { features });
  await sleep(400);
}

// ---------------------------------------------------------------------------------------------
// capture + contrast
// ---------------------------------------------------------------------------------------------

async function grab(win, rect) {
  const image = await win.webContents.capturePage(rect);
  const bitmap = image.toBitmap();
  const pw = image.getSize().width;
  const ph = bitmap.length / 4 / pw;
  const scale = pw / (rect?.width ?? win.getContentSize()[0]);
  return {
    image, scale, pw, ph,
    // BGRA in memory on little-endian Linux.
    devicePixel(px, py) {
      const i = (py * pw + px) * 4;
      return [bitmap[i + 2], bitmap[i + 1], bitmap[i]];
    },
  };
}

function savePng(dir, image, name, maxWidth) {
  const out = maxWidth && image.getSize().width > maxWidth ? image.resize({ width: maxWidth, quality: "best" }) : image;
  writeFileSync(join(dir, name), out.toPNG());
}

const toLinear = (v) => { const c = v / 255; return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4; };
const luminance = (c) => 0.2126 * toLinear(c[0]) + 0.7152 * toLinear(c[1]) + 0.0722 * toLinear(c[2]);
const ratio = (a, b) => (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);

const GLASS_HOSTS = ".lg";
const HERO_HOSTS = ".page-hero";

/**
 * Contrast of every glyph run in the hosts against the hardest device pixel behind it: glyphs
 * are made transparent, the window is captured, and each label's box is scanned.
 */
async function measureContrast(win, hostSelector) {
  const found = await js(win, `__glassProbe.labels(${JSON.stringify(hostSelector)})`);
  await js(win, "__glassProbe.hideText(true)");
  await sleep(120);
  const shot = await grab(win);
  await js(win, "__glassProbe.hideText(false)");
  return found.map((label) => {
    const [x, y, w, h] = label.rect;
    const ink = luminance(label.color);
    const x0 = Math.max(0, Math.floor(x * shot.scale)), x1 = Math.min(shot.pw - 1, Math.ceil((x + w) * shot.scale) - 1);
    const y0 = Math.max(0, Math.floor(y * shot.scale)), y1 = Math.min(shot.ph - 1, Math.ceil((y + h) * shot.scale) - 1);
    let hardest = Infinity;
    let behind = null;
    for (let py = y0; py <= y1; py++) for (let px = x0; px <= x1; px++) {
      const pixel = shot.devicePixel(px, py);
      const r = ratio(ink, luminance(pixel));
      if (r < hardest) { hardest = r; behind = pixel; }
    }
    return { host: label.host, kind: label.kind, text: label.text, min: label.min, color: label.color, rect: label.rect.map(Math.round), ratio: Number(hardest.toFixed(2)), behind };
  });
}

/** Folds measurements into per host/kind groups: count, failures, and the worst label. */
function summarize(scene, measurements, into) {
  for (const m of measurements) {
    const key = `${m.host}/${m.kind}`;
    const group = (into[key] ??= { min: m.min, measured: 0, failing: 0, failingByScene: {}, worst: null });
    group.measured++;
    if (m.ratio < m.min) {
      group.failing++;
      group.failingByScene[scene] = (group.failingByScene[scene] ?? 0) + 1;
    }
    if (!group.worst || m.ratio < group.worst.ratio) group.worst = { ratio: m.ratio, text: m.text, color: m.color, behind: m.behind, rect: m.rect, scene };
  }
}

// Backdrops chosen to hurt: the extremes, the saturated primaries and secondaries (the tone
// audit's worst cases), fine and coarse stripes, and dense light/dark "text".
const STRESS = {
  white: "#fff",
  black: "#000",
  magenta: "#f0f",
  green: "#0f0",
  yellow: "#ff0",
  cyan: "#0ff",
  red: "#f00",
  blue: "#00f",
  "stripes-2px": "repeating-linear-gradient(90deg, #fff 0 2px, #000 2px 4px)",
  "stripes-13px": "repeating-linear-gradient(0deg, #fff 0 13px, #111 13px 26px)",
  "stripes-48px": "repeating-linear-gradient(45deg, #fff 0 48px, #f0f 48px 96px, #000 96px 144px)",
  "white-magenta-split": "linear-gradient(90deg, #fff 0 50%, #f0f 50% 100%)",
};

async function contrastSweep(win, sceneName, groups, { stress = true, listPositions = [] } = {}) {
  const scan = async (scene) => {
    summarize(scene, await measureContrast(win, GLASS_HOSTS), groups.glass);
    summarize(scene, await measureContrast(win, HERO_HOSTS), groups.hero);
  };
  await scan(sceneName);
  for (const y of listPositions) {
    await js(win, `__glassProbe.scrollList(${y})`);
    await sleep(140);
    await scan(`${sceneName} list@${y}`);
  }
  if (!stress) return;
  for (const [name, background] of Object.entries(STRESS)) {
    await js(win, `__glassProbe.stress(${JSON.stringify(background)})`);
    await sleep(160);
    // The stress layer covers the page, so only glass is meaningful here.
    summarize(`${sceneName} stress:${name}`, await measureContrast(win, GLASS_HOSTS), groups.glass);
  }
  await js(win, "__glassProbe.stress(null)");
  await sleep(120);
}

async function environmentReport(win) {
  const display = screen.getPrimaryDisplay();
  const gpu = await app.getGPUInfo("basic").catch(() => null);
  return {
    electron: process.versions.electron,
    chromium: process.versions.chrome,
    appUrl: win.webContents.getURL(),
    xdgSessionType: process.env.XDG_SESSION_TYPE ?? null,
    waylandDisplay: process.env.WAYLAND_DISPLAY ?? null,
    display: { size: display.size, scaleFactor: display.scaleFactor, colorSpace: display.colorSpace, colorDepth: display.colorDepth },
    gpuFeatureStatus: app.getGPUFeatureStatus(),
    gpuDevice: gpu?.gpuDevice?.find((d) => d.active) ?? gpu?.gpuDevice?.[0] ?? null,
    rendererDpr: await js(win, "window.devicePixelRatio"),
    contentSize: win.getContentSize(),
  };
}

// ---------------------------------------------------------------------------------------------
// interaction: hover, press and overlay motion, with real input events
// ---------------------------------------------------------------------------------------------

async function interactionChecks(win) {
  const wc = win.webContents;
  const centre = (id) => js(win, `__glassProbe.centre(${JSON.stringify(id)})`);
  const style = (id, prop) => js(win, `__glassProbe.styleOf(${JSON.stringify(id)}, ${JSON.stringify(prop)})`);
  const scaleOf = (matrix) => (matrix === "none" ? 1 : Number(Number(matrix.slice(7).split(",")[0]).toFixed(3)));
  const press = async (id, holdMs) => {
    const at = await centre(id);
    wc.sendInputEvent({ type: "mouseMove", x: at.x, y: at.y });
    wc.sendInputEvent({ type: "mouseDown", x: at.x, y: at.y, button: "left", clickCount: 1 });
    await sleep(holdMs);
    const held = await style(id, "transform");
    wc.sendInputEvent({ type: "mouseUp", x: at.x, y: at.y, button: "left", clickCount: 1 });
    return held;
  };
  const out = {};

  await setAppearance(win, { theme: "dark", reduceTransparency: false, reduceMotion: false });
  await navigate(win, "home");
  const nav = await centre("nav-search");
  const fillAtRest = await style("nav-search", "backgroundColor");
  for (let i = 0; i < 12; i++) {
    wc.sendInputEvent({ type: "mouseMove", x: nav.x - 60 + i * 10, y: nav.y });
    await sleep(16);
  }
  await sleep(300);
  out.hover = { fillAtRest, fillHovered: await style("nav-search", "backgroundColor"), highlightX: await js(win, '__glassProbe.inlineVar("app-sidebar", "--lg-light-x")') };

  out.press = { scaleHeld: scaleOf(await press("btn-play-pause", 160)), scaleAfterRelease: [] };
  for (let i = 0; i < 8; i++) {
    await sleep(45);
    out.press.scaleAfterRelease.push(scaleOf(await style("btn-play-pause", "transform")));
  }
  await sleep(400);
  out.press.scaleSettled = scaleOf(await style("btn-play-pause", "transform"));

  await press("btn-open-diagnostics", 30);
  out.dialogMorph = { scaleWhileOpening: [] };
  for (let i = 0; i < 4; i++) {
    await sleep(25);
    if (await js(win, '__glassProbe.has("accessible-dialog")')) out.dialogMorph.scaleWhileOpening.push(scaleOf(await style("accessible-dialog", "transform")));
  }
  await sleep(700);
  out.dialogMorph.scaleSettled = scaleOf(await style("accessible-dialog", "transform"));
  await js(win, '__glassProbe.click("dialog-close-btn")');

  await setAppearance(win, { reduceMotion: true });
  out.reducedMotion = { pressTransform: await press("btn-play-pause", 160) };
  await js(win, '__glassProbe.click("btn-open-diagnostics")');
  await until(win, '__glassProbe.has("accessible-dialog")');
  out.reducedMotion.dialogTransitionProperty = await style("accessible-dialog", "transitionProperty");
  out.reducedMotion.dialogTransform = await style("accessible-dialog", "transform");
  await js(win, '__glassProbe.click("dialog-close-btn")');
  await setAppearance(win, { reduceMotion: false });
  wc.sendInputEvent({ type: "mouseMove", x: 640, y: 400 });
  return out;
}

// ---------------------------------------------------------------------------------------------
// layout: player-bar controls at the window sizes the shell allows, focus rings inside clips
// ---------------------------------------------------------------------------------------------

const BAR_CONTROLS = ["player-track-title", "btn-shuffle", "btn-prev", "btn-play-pause", "btn-next", "btn-repeat", "player-seek-slider", "btn-mute", "player-volume-slider", "btn-open-lyrics", "btn-open-diagnostics", "player-observed-status"];
// Diagnostics also opens from Settings; the playback state moves into the subtitle when its chip goes.
const MAY_HIDE = new Set(["btn-open-diagnostics", "player-observed-status"]);

const BAR_LAYOUT = `((ids) => {
  const q = (s) => document.querySelector(s);
  const bar = q('[data-testid="player-bar"]').getBoundingClientRect();
  const controls = {};
  for (const id of ids) {
    const el = q('[data-testid="' + id + '"]');
    const r = el.getBoundingClientRect();
    const shown = el.checkVisibility() && r.width >= 1;
    controls[id] = !shown ? "hidden" : r.left < bar.left - 0.5 || r.right > bar.right + 0.5 ? "outside bar" : Math.round(r.width);
  }
  const title = q('[data-testid="player-track-title"]');
  const inline = q(".player-status-inline");
  return {
    viewport: [innerWidth, innerHeight],
    dock: Math.round(q(".app-player-dock").getBoundingClientRect().width),
    controls,
    title: { shownPx: title.clientWidth, fullPx: title.scrollWidth },
    statusInSubtitle: inline ? inline.checkVisibility() : false,
    barOverflows: q('[data-testid="player-bar"]').scrollWidth > q('[data-testid="player-bar"]').clientWidth,
  };
})`;

// How far the focus ring of the focused element stays inside every clipping ancestor (negative = cut).
const RING_MARGIN = `(() => {
  const el = document.activeElement;
  const cs = getComputedStyle(el);
  const grow = parseFloat(cs.outlineWidth) + parseFloat(cs.outlineOffset);
  const r = el.getBoundingClientRect();
  let margin = Infinity;
  for (let node = el.parentElement; node; node = node.parentElement) {
    const ns = getComputedStyle(node);
    if (ns.overflowX === "visible" && ns.overflowY === "visible") continue;
    const c = node.getBoundingClientRect();
    margin = Math.min(margin, r.top - grow - c.top, c.bottom - (r.bottom + grow), r.left - grow - c.left, c.right - (r.right + grow));
  }
  return { focused: el.dataset.testid ?? el.tagName, focusVisible: el.matches(":focus-visible"), ringPx: grow, marginPx: Math.round(margin * 10) / 10 };
})()`;

async function layoutChecks(win, save) {
  const out = { bar: {}, focusRing: {} };
  await setAppearance(win, { theme: "dark", reduceTransparency: false });
  for (const [width, height, collapsed] of [[WINDOW.width, WINDOW.height, false], [900, 600, false], [900, 600, true]]) {
    win.setContentSize(width, height);
    if ((await js(win, 'document.querySelector(".app-shell").dataset.sidebar === "collapsed"')) !== collapsed) await js(win, '__glassProbe.click("sidebar-collapse-btn")');
    await navigate(win, "home");
    await sleep(400);
    const key = `${width}x${height}${collapsed ? "-collapsed" : ""}`;
    const m = await js(win, `${BAR_LAYOUT}(${JSON.stringify(BAR_CONTROLS)})`);
    const missing = Object.entries(m.controls)
      .filter(([id, v]) => typeof v !== "number" && !(v === "hidden" && MAY_HIDE.has(id)))
      .map(([id]) => id);
    const stateShown = typeof m.controls["player-observed-status"] === "number" || m.statusInSubtitle;
    const sizeMatched = m.viewport[0] === width && m.viewport[1] === height;
    out.bar[key] = { ...m, missing, pass: sizeMatched && missing.length === 0 && stateShown && !m.barOverflows };
    if (key === "900x600") await save("bar-900x600@2x", { x: 0, y: height - 100, width, height: 100 });
  }
  win.setContentSize(WINDOW.width, WINDOW.height);
  if (await js(win, 'document.querySelector(".app-shell").dataset.sidebar === "collapsed"')) await js(win, '__glassProbe.click("sidebar-collapse-btn")');
  await sleep(400);

  // Keyboard focus: Tab moves off the element and Shift+Tab back, so :focus-visible holds.
  const keyboardFocus = async (id) => {
    win.webContents.focus();
    await js(win, `document.querySelector('[data-testid="${id}"]').focus({ focusVisible: true }); true`);
    for (const modifiers of [[], ["shift"]]) {
      win.webContents.sendInputEvent({ type: "keyDown", keyCode: "Tab", modifiers });
      win.webContents.sendInputEvent({ type: "keyUp", keyCode: "Tab", modifiers });
      await sleep(120);
    }
    return js(win, RING_MARGIN);
  };
  out.focusRing.firstNavItem = await keyboardFocus("nav-home");
  await save("focus-nav@2x", { x: 0, y: 60, width: 260, height: 90 });
  await js(win, "document.activeElement.blur(); true");
  return out;
}

// ---------------------------------------------------------------------------------------------
// shots: screenshots + contrast on the real UI
// ---------------------------------------------------------------------------------------------

const BAR_CROP = { x: 250, y: 560, width: 1030, height: 240 };
const SIDEBAR_CROP = { x: 0, y: 0, width: 420, height: 330 };

export async function runShots(outDir, { quick = false } = {}) {
  mkdirSync(outDir, { recursive: true });
  const win = await launchApp();
  const report = { capturedAt: new Date().toISOString(), environment: await environmentReport(win), scenes: {}, contrast: {} };
  const save = async (name, crop) => savePng(outDir, (await grab(win, crop)).image, `${name}.png`, crop ? undefined : WINDOW.width);
  const note = async (name, extra = {}) => { report.scenes[name] = { ...extra, surfaces: await js(win, "__glassProbe.surfaces()"), root: await js(win, "__glassProbe.root()") }; };

  for (const theme of ["dark", "light"]) {
    const groups = (report.contrast[theme] = { glass: {}, hero: {} });
    await setAppearance(win, { theme, reduceTransparency: false });

    // Home: the hero wash and the cover shelf under the sidebar.
    await navigate(win, "home");
    const cards = await js(win, "__glassProbe.lengthenShelf(6)");
    await js(win, "__glassProbe.scrollShelf(250)");
    await sleep(400);
    await save(`home-${theme}`);
    if (theme === "dark") await save("home-dark-sidebar@2x", SIDEBAR_CROP);
    await note(`home-${theme}`, { shelfCards: cards, shelfNote: "mock feed pins 2 playlists; the probe repeats the cards so the shelf scrolls" });
    await contrastSweep(win, `home-${theme}`, groups, { stress: !quick });

    // A 10,000-row list scrolled under the player bar.
    const geometry = await openLongList(win);
    await save(`list-${theme}`);
    if (theme === "dark") await save("list-dark-bar@2x", BAR_CROP);
    await note(`list-${theme}`, { geometry });
    await contrastSweep(win, `list-${theme}`, groups, { stress: !quick, listPositions: quick ? [] : [0, 5800, 40_000, 123_456, 579_000] });

    // Overlays: the track menu and the diagnostics dialog.
    await js(win, "__glassProbe.scrollList(2000)");
    await sleep(200);
    await js(win, `__glassProbe.clickFirst('[data-testid="track-actions-btn"]')`);
    await until(win, '__glassProbe.has("track-actions")');
    await sleep(700);
    if (theme === "light") await save("menu-light");
    await note(`menu-${theme}`);
    await contrastSweep(win, `menu-${theme}`, groups, { stress: !quick });
    await js(win, '__glassProbe.click("dialog-close-btn")');
    await sleep(200);
    await js(win, '__glassProbe.click("btn-open-diagnostics")');
    await until(win, '__glassProbe.has("accessible-dialog")');
    await sleep(700);
    if (theme === "dark") await save("dialog-dark");
    await contrastSweep(win, `dialog-${theme}`, groups, { stress: !quick });
    await js(win, '__glassProbe.click("dialog-close-btn")');
    await sleep(200);
  }

  // Fallbacks (dark): the app's own "reduce transparency" setting, then forced colours.
  const fallback = (report.contrast.reducedTransparencyDark = { glass: {}, hero: {} });
  await setAppearance(win, { theme: "dark", reduceTransparency: true });
  await openLongList(win);
  await save("reduced-transparency-dark");
  await note("reduced-transparency-dark", { filters: await js(win, "__glassProbe.filters()") });
  await contrastSweep(win, "reduced-transparency-dark", fallback, { stress: !quick });

  await setAppearance(win, { theme: "dark", reduceTransparency: false });
  await openLongList(win);
  await emulateMedia(win, [{ name: "forced-colors", value: "active" }]);
  await save("forced-colors");
  await note("forced-colors", { mediaMatches: await js(win, 'matchMedia("(forced-colors: active)").matches') });
  await emulateMedia(win, [{ name: "forced-colors", value: "" }, { name: "prefers-reduced-transparency", value: "reduce" }]);
  await note("prefers-reduced-transparency", { mediaMatches: await js(win, 'matchMedia("(prefers-reduced-transparency: reduce)").matches') });
  await emulateMedia(win, [{ name: "prefers-reduced-transparency", value: "" }]);

  report.summary = Object.fromEntries(Object.entries(report.contrast).map(([scheme, sets]) => [scheme, Object.fromEntries(Object.entries(sets).map(([set, groups]) => {
    const all = Object.values(groups);
    return [set, { measured: all.reduce((n, g) => n + g.measured, 0), failing: all.reduce((n, g) => n + g.failing, 0) }];
  }))]));
  report.layout = await layoutChecks(win, save);
  report.interaction = await interactionChecks(win);
  report.failedChecks = [];
  for (const [size, layout] of Object.entries(report.layout.bar)) {
    if (!layout.pass) report.failedChecks.push(`player bar layout at ${size}`);
  }
  for (const [scheme, sets] of Object.entries(report.summary)) {
    for (const [set, { measured, failing }] of Object.entries(sets)) {
      if (measured === 0 || failing > 0) {
        report.failedChecks.push(`${scheme}/${set} contrast (${failing} failing of ${measured} measured)`);
      }
    }
  }
  writeFileSync(join(outDir, "report.json"), JSON.stringify(report, null, 2));
  console.log(JSON.stringify({ layout: report.layout, interaction: report.interaction, summary: report.summary, worst: Object.fromEntries(Object.entries(report.contrast).map(([scheme, sets]) => [scheme, Object.fromEntries(Object.entries(sets.glass).map(([k, g]) => [k, `${g.worst.ratio} (min ${g.min}, ${g.failing}/${g.measured} failing) ${g.worst.scene}`]))])) }, null, 1));
  if (!quick && report.failedChecks.length > 0) {
    throw new Error(`Glass checks failed (${report.failedChecks.join(", ")}). See ${join(outDir, "report.json")}`);
  }
  return report;
}

/** Fast visual iteration: one theme, the main scenes, 2x crops; no contrast sweep. */
export async function runLook(outDir, theme = "dark") {
  mkdirSync(outDir, { recursive: true });
  const win = await launchApp();
  const save = async (name, crop) => savePng(outDir, (await grab(win, crop)).image, `look-${theme}-${name}.png`, crop ? undefined : win.getContentSize()[0]);
  await setAppearance(win, { theme, reduceTransparency: false });
  await navigate(win, "home");
  await save("home-rest");
  await js(win, "__glassProbe.lengthenShelf(6)");
  await js(win, "__glassProbe.scrollShelf(250)");
  await sleep(400);
  await save("home");
  await save("home-sidebar@2x", SIDEBAR_CROP);
  await openLongList(win);
  await save("list");
  await save("list-bar@2x", BAR_CROP);
  await save("list-sidebar@2x", SIDEBAR_CROP);
  await js(win, `__glassProbe.clickFirst('[data-testid="track-actions-btn"]')`);
  await until(win, '__glassProbe.has("track-actions")');
  await sleep(700);
  await save("menu");
  // Follow a synthetic playlist track to a populated album and its artist.
  await js(win, '__glassProbe.click("action-go-album")');
  await until(win, '__glassProbe.has("view-album") && __glassProbe.has("album-artist-link") && __glassProbe.has("virtual-track-list")');
  await sleep(350);
  await save("view-album");
  await js(win, '__glassProbe.click("album-artist-link")');
  await until(win, '__glassProbe.has("view-artist") && __glassProbe.has("virtual-track-list")');
  await sleep(350);
  await save("view-artist");
  // Remaining sidebar views, then the smallest window and the collapsed sidebar.
  for (const view of ["search", "library", "liked", "recent", "now-playing", "queue", "lyrics", "settings"]) {
    await navigate(win, view);
    await save(`view-${view}`);
  }
  win.setContentSize(900, 600);
  await openLongList(win);
  await save("list-900x600");
  await navigate(win, "home");
  await save("home-900x600");
  win.setContentSize(WINDOW.width, WINDOW.height);
  await openLongList(win);
  await js(win, '__glassProbe.click("sidebar-collapse-btn")');
  await sleep(500);
  await save("list-collapsed");
  console.log(JSON.stringify(await js(win, "__glassProbe.surfaces()")));
}

// ---------------------------------------------------------------------------------------------
// perf: frame pacing of automated scrolling in the real UI
// ---------------------------------------------------------------------------------------------

function percentile(sorted, p) {
  return sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1))];
}

// Budget is one refresh interval, learnt from the rAF deltas themselves. rAF timestamps are
// quantised to 0.1 ms, so a frame that missed exactly one vsync reads 16.6–16.7 ms at 120 Hz;
// LONG_FACTOR sits clear of that so "long" means two or more missed vsyncs.
const LONG_FACTOR = 2.25;
const MISS_FACTOR = 1.5;

// Refresh interval from rAF deltas: the mean of the deltas near the median.
function vsyncInterval(intervals) {
  const median = percentile([...intervals].sort((x, y) => x - y), 50);
  const near = intervals.filter((v) => Math.abs(v - median) < median * 0.2);
  return near.reduce((s, v) => s + v, 0) / near.length;
}

function pacing(intervals, refreshMs) {
  const sorted = [...intervals].sort((x, y) => x - y);
  const r = (v) => Number(v.toFixed(3));
  const long = sorted.filter((v) => v > refreshMs * LONG_FACTOR).length;
  const missed = sorted.filter((v) => v > refreshMs * MISS_FACTOR).length;
  const slow = [];
  let t = 0;
  for (const dt of intervals) {
    t += dt;
    if (dt > refreshMs * MISS_FACTOR && slow.length < 40) slow.push({ atMs: Math.round(t), ms: Number(dt.toFixed(1)) });
  }
  return {
    frames: sorted.length,
    p50Ms: r(percentile(sorted, 50)),
    p95Ms: r(percentile(sorted, 95)),
    p99Ms: r(percentile(sorted, 99)),
    maxMs: r(sorted[sorted.length - 1]),
    meanMs: r(sorted.reduce((s, v) => s + v, 0) / sorted.length),
    droppedFrames: missed,
    droppedFramePercent: Number(((missed / sorted.length) * 100).toFixed(3)),
    longFrames: long,
    longFramePercent: Number(((long / sorted.length) * 100).toFixed(3)),
    slowFrames: slow,
  };
}

// Toggling the sidebar re-lays the page and resizes both lenses.
async function measureCollapse(win) {
  await setAppearance(win, { theme: "dark", reduceTransparency: false });
  await openLongList(win, 0);
  const idle = js(win, "__glassProbe.measureIdle(4)");
  for (let i = 0; i < 6; i++) {
    await sleep(550);
    await js(win, '__glassProbe.click("sidebar-collapse-btn")');
  }
  const intervals = await idle;
  return { toggles: 6, ...pacing(intervals, vsyncInterval(intervals)) };
}

/** `only === "collapse"` measures just the sidebar transition and prints it. */
export async function runPerf(outFile, seconds = 60, only = null) {
  const win = await launchApp();
  if (only === "collapse") {
    console.log(JSON.stringify(await measureCollapse(win)));
    return null;
  }
  const report = {
    capturedAt: new Date().toISOString(),
    method: "rAF intervals while the main thread sets scrollTop every frame (eased round trips, 5.2 s per leg). dropped = interval > 1.5 refresh intervals; long = > 2.25 (two or more missed vsyncs). Refresh interval = mean of rAF deltas near the median, the shorter of a 3 s idle run and the scroll run.",
    seconds,
    runs: [],
  };

  const measure = async (name, expression, extra = {}) => {
    await sleep(800);
    const idle = await js(win, "__glassProbe.measureIdle(3)");
    const run = await js(win, expression);
    const refreshMs = Math.min(vsyncInterval(idle), vsyncInterval(run.intervals));
    const result = {
      name,
      ...extra,
      focused: win.isFocused(),
      visible: win.isVisible(),
      surfaces: (await js(win, "__glassProbe.surfaces()")).map((s) => `${s.testid}:${s.lens ? "lens" : "opaque"}`),
      measuredHz: Number((1000 / refreshMs).toFixed(2)),
      budgetMs: Number(refreshMs.toFixed(3)),
      seconds: Number((run.elapsedMs / 1000).toFixed(1)),
      scrollRangePx: run.rangePx,
      ...pacing(run.intervals, refreshMs),
    };
    report.runs.push(result);
    const { slowFrames: _slow, ...brief } = result;
    console.log(JSON.stringify(brief));
  };
  const LEG = 5200;

  // 10,000-row list crossing the player dock; the page itself stays put.
  await setAppearance(win, { theme: "dark", reduceTransparency: false });
  let geometry = await openLongList(win, 0);
  await measure("liquid glass: 10k list under the bar (sidebar + bar)", `__glassProbe.measureList(${seconds}, 12000, ${LEG})`, { geometry });

  await js(win, '__glassProbe.click("btn-open-diagnostics")');
  await until(win, '__glassProbe.has("accessible-dialog")');
  await sleep(600);
  await measure("liquid glass: same, diagnostics dialog open (three surfaces)", `__glassProbe.measureList(${seconds}, 12000, ${LEG})`);
  await js(win, '__glassProbe.click("dialog-close-btn")');

  // The whole page: hero wash under the sidebar, list card under the bar.
  await js(win, "__glassProbe.scrollMain(0)");
  await measure("liquid glass: page scroll (hero under the sidebar, list under the bar)", `__glassProbe.measureMain(${seconds}, 100000, 2600)`);

  await setAppearance(win, { theme: "light", reduceTransparency: false });
  await openLongList(win, 0);
  await measure("liquid glass, light theme: 10k list under the bar", `__glassProbe.measureList(${seconds}, 12000, ${LEG})`);

  // Control: the same build and scroll with the app's "reduce transparency" setting.
  await setAppearance(win, { theme: "dark", reduceTransparency: true });
  geometry = await openLongList(win, 0);
  await measure("opaque (reduce transparency): 10k list under the bar", `__glassProbe.measureList(${seconds}, 12000, ${LEG})`, { geometry });

  report.sidebarCollapse = await measureCollapse(win);
  console.log(JSON.stringify({ sidebarCollapse: { ...report.sidebarCollapse, slowFrames: undefined } }));

  report.environment = await environmentReport(win);
  mkdirSync(resolve(outFile, ".."), { recursive: true });
  writeFileSync(outFile, JSON.stringify(report, null, 2));
  return report;
}
