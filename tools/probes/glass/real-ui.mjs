// Drives the real application (apps/desktop/dist + ui/dist, YTMCP_MODE=mock) for the
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
  const listTop = () => list().getBoundingClientRect().top - main().getBoundingClientRect().top + main().scrollTop;
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
    // The list is part of the page: "list scrolled by y" puts the page y px past the list's top.
    scrollList: (y) => { const top = listTop(); main().scrollTop = top + y; return main().scrollTop - top; },
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
    measureList: (seconds, rangePx, legMs) => measureScroll(main(), seconds, rangePx, legMs),
  };
  return true;
})()`;

// ---------------------------------------------------------------------------------------------
// app control
// ---------------------------------------------------------------------------------------------

/** Starts the desktop shell exactly as `just dev-mock` does and returns its main window. */
export async function launchApp() {
  if (process.env.YTMCP_MODE !== "mock") throw new Error("real-ui probe only runs with YTMCP_MODE=mock");
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

// Errors name the script that failed: executeJavaScript alone only says that one did.
const js = (win, code) =>
  win.webContents.executeJavaScript(code).catch((err) => {
    throw new Error(`${err.message} (in: ${code.slice(0, 160)})`);
  });

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

/** Collapses or expands the sidebar and waits until its transition has settled (lens back). */
async function setSidebar(win, collapsed) {
  const now = await js(win, 'document.querySelector(".app-shell").dataset.sidebar === "collapsed"');
  if (now !== collapsed) await js(win, '__glassProbe.click("sidebar-collapse-btn")');
  await until(win, '!document.querySelector(".app-shell").hasAttribute("data-sidebar-motion")');
  await sleep(150);
}

/**
 * Where the sidebar's icons sit: each icon's, the mark's and the playback glyph's centre from
 * the panel's left edge (36 px in both states), the visible width (the panel stays put while its
 * frame's clip slides) and whether the nav column scrolls.
 */
const SIDEBAR_ANCHORS = `(() => {
  const panel = document.querySelector(".app-sidebar");
  const p = panel.getBoundingClientRect();
  const visibleRight = Math.min(p.right, document.querySelector(".sidebar-frame").getBoundingClientRect().right);
  const centres = [...panel.querySelectorAll(".sidebar-item > .icon, .sidebar-glyph, .sidebar-mark")].map((el) => {
    const r = el.getBoundingClientRect();
    return Math.round((r.left + r.width / 2 - p.left) * 100) / 100;
  });
  const nav = panel.querySelector(".sidebar-nav");
  return { left: Math.round(p.left * 100) / 100, width: Math.round((visibleRight - p.left) * 100) / 100, centres: [...new Set(centres)], navScrolls: nav.scrollHeight > nav.clientHeight, navScrollbarPx: nav.offsetWidth - nav.clientWidth };
})()`;

/** Toggles the sidebar and resolves with the milliseconds until its transition hold is released. */
const SIDEBAR_SETTLE = `new Promise((resolve) => {
  const shell = document.querySelector(".app-shell");
  const t0 = performance.now();
  const mo = new MutationObserver(() => {
    if (shell.hasAttribute("data-sidebar-motion")) return;
    mo.disconnect();
    resolve(Math.round(performance.now() - t0));
  });
  mo.observe(shell, { attributes: true, attributeFilter: ["data-sidebar-motion"] });
  document.querySelector('[data-testid="sidebar-collapse-btn"]').click();
})`;

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

/** Opens the first mock playlist with the 10,000-row fixture and scrolls the page `listScrollTop` px into the list. */
async function openLongList(win, listScrollTop = 2000) {
  // The mock app's Settings > Advanced opens the first playlist and fills it with 10,000 songs.
  await navigate(win, "settings");
  await until(win, '__glassProbe.has("btn-load-10k-fixture")');
  await js(win, '__glassProbe.click("btn-load-10k-fixture")');
  await until(win, '__glassProbe.has("view-playlist") && __glassProbe.has("virtual-track-list")');
  await until(win, "__glassProbe.geometry().listRows > 500000").catch(async (err) => {
    throw new Error(`${err.message}: ${JSON.stringify(await js(win, "({ g: __glassProbe.geometry(), rows: document.querySelectorAll('[data-testid=virtual-track-row]').length })"))}`);
  });
  await sleep(300);
  await js(win, `__glassProbe.scrollList(${listScrollTop})`);
  await sleep(350);
  return js(win, "__glassProbe.geometry()");
}

/** Closes the open overlay: a dialog through its Close button, the track menu with Escape. */
async function closeOverlay(win) {
  await js(win, `(() => {
    const close = document.querySelector('[data-testid="dialog-close-btn"]');
    if (close) return close.click(), true;
    document.querySelector('[data-testid="track-actions-menu"]')?.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    return true;
  })()`);
  await until(win, '!__glassProbe.has("accessible-dialog") && !__glassProbe.has("track-actions")');
  await sleep(200);
}

/** Shows Settings with its Diagnostics button scrolled into view (the entry moved there from the bar). */
async function showDiagnosticsEntry(win) {
  await navigate(win, "settings");
  await until(win, '__glassProbe.has("settings-open-diagnostics")');
  await js(win, `document.querySelector('[data-testid="settings-open-diagnostics"]').scrollIntoView({ block: "center" }); true`);
  await sleep(250);
}

/** Opens the Diagnostics dialog from Settings and waits for it. */
async function openDiagnostics(win) {
  await showDiagnosticsEntry(win);
  await js(win, '__glassProbe.click("settings-open-diagnostics")');
  await until(win, '__glassProbe.has("accessible-dialog")');
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
  out.hover = { fillAtRest, fillHovered: await style("nav-search", "backgroundColor"), lightShift: await js(win, '__glassProbe.inlineVar("app-sidebar", "--lg-light-shift")') };

  out.press = { scaleHeld: scaleOf(await press("btn-play-pause", 160)), scaleAfterRelease: [] };
  for (let i = 0; i < 8; i++) {
    await sleep(45);
    out.press.scaleAfterRelease.push(scaleOf(await style("btn-play-pause", "transform")));
  }
  await sleep(400);
  out.press.scaleSettled = scaleOf(await style("btn-play-pause", "transform"));

  await showDiagnosticsEntry(win);
  await press("settings-open-diagnostics", 30);
  out.dialogMorph = { scaleWhileOpening: [] };
  for (let i = 0; i < 4; i++) {
    await sleep(25);
    if (await js(win, '__glassProbe.has("accessible-dialog")')) out.dialogMorph.scaleWhileOpening.push(scaleOf(await style("accessible-dialog", "transform")));
  }
  await sleep(700);
  out.dialogMorph.scaleSettled = scaleOf(await style("accessible-dialog", "transform"));
  await closeOverlay(win);

  await setAppearance(win, { reduceMotion: true });
  out.reducedMotion = { pressTransform: await press("btn-play-pause", 160) };
  await openDiagnostics(win);
  out.reducedMotion.dialogTransitionProperty = await style("accessible-dialog", "transitionProperty");
  out.reducedMotion.dialogTransform = await style("accessible-dialog", "transform");
  await closeOverlay(win);
  await setAppearance(win, { reduceMotion: false });
  wc.sendInputEvent({ type: "mouseMove", x: 640, y: 400 });
  return out;
}

// ---------------------------------------------------------------------------------------------
// layout: player-bar controls at the window sizes the shell allows, focus rings inside clips
// ---------------------------------------------------------------------------------------------

// The bar's controls (PlayerBar artboard). Diagnostics opens from Settings; the play button is the state.
const BAR_CONTROLS = ["player-track-title", "player-like", "btn-shuffle", "btn-prev", "btn-play-pause", "btn-next", "btn-repeat", "player-seek-slider", "btn-mute", "player-volume-slider", "btn-open-lyrics", "btn-open-queue"];
// The dock width under which a control gives way (player-bar.css): the volume bar first (mute and
// the wheel still set the volume; 930 px while no quality badge shows, as in this scene), then the
// like button and the lyrics button.
const GIVES_WAY_UNDER = { "player-volume-slider": 930, "player-like": 776, "btn-open-lyrics": 544 };
const BAR_LAYOUT = `((ids) => {
  const q = (s) => document.querySelector(s);
  const bar = q('[data-testid="player-bar"]').getBoundingClientRect();
  const controls = {};
  for (const id of ids) {
    const el = q('[data-testid="' + id + '"]');
    const r = el?.getBoundingClientRect();
    const shown = el?.checkVisibility() && r.width >= 1;
    controls[id] = !el ? "missing" : !shown ? "hidden" : r.left < bar.left - 0.5 || r.right > bar.right + 0.5 ? "outside bar" : Math.round(r.width);
  }
  const title = q('[data-testid="player-track-title"]');
  return {
    viewport: [innerWidth, innerHeight],
    dock: Math.round(q(".app-player-dock").getBoundingClientRect().width),
    controls,
    title: { shownPx: title.clientWidth, fullPx: title.scrollWidth },
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
    await setSidebar(win, collapsed);
    await navigate(win, "home");
    await sleep(400);
    const key = `${width}x${height}${collapsed ? "-collapsed" : ""}`;
    const m = await js(win, `${BAR_LAYOUT}(${JSON.stringify(BAR_CONTROLS)})`);
    const missing = Object.entries(m.controls)
      .filter(([id, v]) => typeof v !== "number" && !(v === "hidden" && m.dock < (GIVES_WAY_UNDER[id] ?? 0)))
      .map(([id]) => id);
    const sizeMatched = m.viewport[0] === width && m.viewport[1] === height;
    out.bar[key] = { ...m, missing, pass: sizeMatched && missing.length === 0 && !m.barOverflows };
    if (key === "900x600") await save("bar-900x600@2x", { x: 0, y: height - 100, width, height: 100 });
  }
  win.setContentSize(WINDOW.width, WINDOW.height);
  await setSidebar(win, false);
  await sleep(250);

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
const TOAST_CROP = { x: 480, y: 600, width: 600, height: 120 };

/** Switches the mock app's test scenario through the real Settings control. */
async function setScenario(win, id) {
  await navigate(win, "settings");
  await until(win, '__glassProbe.has("mock-scenario-select")');
  await js(win, `__glassProbe.select("mock-scenario-select", ${JSON.stringify(id)})`);
  await sleep(600);
}
const SIDEBAR_CROP = { x: 0, y: 0, width: 420, height: 330 };

/** Native-pixel crops of both ends of each rim, with a common backdrop for comparing highlights. */
export async function runRims(outDir) {
  mkdirSync(outDir, { recursive: true });
  const win = await launchApp();
  const report = { environment: await environmentReport(win), captures: [] };
  await setScenario(win, "authExpired");
  for (const theme of ["dark", "light"]) {
    await setAppearance(win, { theme, reduceTransparency: false });
    await until(win, `__glassProbe.root().theme === "${theme}"`);
    await navigate(win, "home");
    await until(win, 'document.querySelector(".toast[data-lg-lens]")');
    await sleep(400);
    for (const backdrop of ["page", "neutral"]) {
      await js(win, `__glassProbe.stress(${backdrop === "neutral" ? '"#808080"' : "null"})`);
      await sleep(240);
      const surfaces = await js(win, "__glassProbe.surfaces()");
      for (const id of ["app-sidebar", "player-bar", surfaces.find((s) => s.testid?.startsWith("toast-"))?.testid]) {
        const surface = surfaces.find((s) => s.testid === id);
        if (!surface?.lens) throw new Error(`Missing refractive lens on ${id}`);
        const [x, y, width, height] = surface.rect;
        for (const [corner, left, top] of [
          ["tl", x - 3, y - 3], ["tr", x + width - 45, y - 3],
          ["bl", x - 3, y + height - 45], ["br", x + width - 45, y + height - 45],
        ]) {
          const rect = { x: Math.round(left), y: Math.round(top), width: 48, height: 48 };
          const shot = await grab(win, rect);
          const file = `${theme}-${backdrop}-${id}-${corner}.png`;
          writeFileSync(join(outDir, file), shot.image.toPNG());
          report.captures.push({ file, rect, pixels: [shot.pw, shot.ph], captureScale: shot.scale });
        }
      }
    }
    await js(win, "__glassProbe.stress(null)");
  }
  writeFileSync(join(outDir, "rims.json"), JSON.stringify(report, null, 2));
}

/** Capture the README views without the developer scenario picker. The library is the normal mock fixture. */
async function presentationShots(win, theme, save) {
  const key = await win.webContents.insertCSS('[data-testid="banner-mock-mode"] { display: none !important; }');
  try {
    for (const view of ["home", theme === "dark" ? "lyrics" : "now-playing"]) {
      await navigate(win, view);
      await js(win, "__glassProbe.scrollMain(0)");
      await sleep(350);
      await save(`readme-${view}-${theme}`);
    }
  } finally {
    await win.webContents.removeInsertedCSS(key);
  }
}

export async function runShots(outDir, { quick = false, clean = false } = {}) {
  mkdirSync(outDir, { recursive: true });
  const win = await launchApp();
  const report = { capturedAt: new Date().toISOString(), environment: await environmentReport(win), scenes: {}, contrast: {} };
  const save = async (name, crop) => savePng(outDir, (await grab(win, crop)).image, `${name}.png`, crop ? undefined : WINDOW.width);
  const note = async (name, extra = {}) => { report.scenes[name] = { ...extra, surfaces: await js(win, "__glassProbe.surfaces()"), root: await js(win, "__glassProbe.root()") }; };

  for (const theme of ["dark", "light"]) {
    const groups = (report.contrast[theme] = { glass: {}, hero: {} });
    await setAppearance(win, { theme, reduceTransparency: false });

    if (clean) await presentationShots(win, theme, save);

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
    // A list position far above the list is the top of the page, with the playlist's hero.
    await contrastSweep(win, `list-${theme}`, groups, { stress: !quick, listPositions: quick ? [-100_000] : [-100_000, 0, 5800, 40_000, 123_456, 579_000] });

    // Overlays: the track menu and the diagnostics dialog.
    await js(win, "__glassProbe.scrollList(2000)");
    await sleep(200);
    await js(win, `__glassProbe.clickFirst('[data-testid="track-actions-btn"]')`);
    await until(win, '__glassProbe.has("track-actions")');
    await sleep(700);
    if (theme === "light") await save("menu-light");
    await note(`menu-${theme}`);
    await contrastSweep(win, `menu-${theme}`, groups, { stress: !quick });
    await closeOverlay(win);
    await openDiagnostics(win);
    await sleep(700);
    if (theme === "dark") await save("dialog-dark");
    await contrastSweep(win, `dialog-${theme}`, groups, { stress: !quick });
    await closeOverlay(win);

    // A toast (L4): the sticky one of the "Session expired" scenario, over the Settings page
    // (that scenario has no library to scroll); the stress layers then go behind it.
    await setScenario(win, "authExpired");
    // Selecting a mock scenario restores its default settings, including the theme.
    await setAppearance(win, { theme, reduceTransparency: false });
    await until(win, `__glassProbe.root().theme === "${theme}"`);
    await until(win, 'document.querySelector(".toast")');
    await js(win, "__glassProbe.scrollMain(0)");
    await sleep(500);
    await save(`toast-${theme}`);
    await save(`toast-${theme}@2x`, TOAST_CROP);
    await note(`toast-${theme}`);
    await contrastSweep(win, `toast-${theme}`, groups, { stress: !quick });
    await setScenario(win, "normal");
    await until(win, '!document.querySelector(".toast")');
  }

  // Fallbacks (dark): the app's own "reduce transparency" setting, then forced colours.
  const fallback = (report.contrast.reducedTransparencyDark = { glass: {}, hero: {} });
  await setAppearance(win, { theme: "dark", reduceTransparency: true });
  await openLongList(win);
  await save("reduced-transparency-dark");
  await note("reduced-transparency-dark", { filters: await js(win, "__glassProbe.filters()") });
  // Also at the top of the page (a list position far above the list), where the hero is.
  await contrastSweep(win, "reduced-transparency-dark", fallback, { stress: !quick, listPositions: [-100_000] });

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

const LENS_GRID =
  "repeating-linear-gradient(0deg, rgb(255 255 255 / 0.9) 0 2px, transparent 2px 22px), " +
  "repeating-linear-gradient(90deg, rgb(255 255 255 / 0.9) 0 2px, transparent 2px 22px), " +
  "linear-gradient(120deg, #c2185b, #6a3fd0 50%, #1e88e5)";

/** Follow the open track menu to its album and artist, capturing the populated detail views. */
async function captureTrackDetails(win, save) {
  await until(win, '__glassProbe.has("action-go-album")');
  await js(win, '__glassProbe.click("action-go-album")');
  await until(win, '__glassProbe.has("view-album") && __glassProbe.has("album-artist-link") && __glassProbe.has("virtual-track-list")');
  await sleep(350);
  await save("album");
  await js(win, '__glassProbe.click("album-artist-link")');
  await until(win, '__glassProbe.has("view-artist") && __glassProbe.has("virtual-track-list")');
  await sleep(350);
  await save("artist");
}

/** Fast visual iteration: one theme, the main scenes, 2x crops; no contrast sweep. */
export async function runLook(outDir, theme = "dark", { clean = false } = {}) {
  mkdirSync(outDir, { recursive: true });
  const win = await launchApp();
  // "clean" hides the mock-mode banner (a developer scenario picker) for presentation shots;
  // the sidebar's "Mock data" chip stays, and captions must still say the data is synthetic.
  if (clean) await js(win, `document.head.append(Object.assign(document.createElement("style"), { textContent: '[data-testid="banner-mock-mode"] { display: none !important; }' }))`);
  const save = async (name, crop) => savePng(outDir, (await grab(win, crop)).image, `look-${theme}-${name}.png`, crop ? undefined : win.getContentSize()[0]);
  await setAppearance(win, { theme, reduceTransparency: false });
  await navigate(win, "home");
  await save("home-rest");
  await js(win, "__glassProbe.lengthenShelf(6)");
  await js(win, "__glassProbe.scrollShelf(250)");
  await sleep(400);
  await save("home");
  await save("home-sidebar@2x", SIDEBAR_CROP);
  await save("home-bar@2x", BAR_CROP);
  // The shelf while the sidebar glides (slowed to 2.4 s): its scroll position and first card stay
  // put, and both ends stay past the window's edges.
  const shelfState = `(() => {
    const shelf = document.querySelector(".shelf");
    const r = shelf.getBoundingClientRect();
    const card = shelf.querySelector(".shelf-card").getBoundingClientRect();
    return { scrollLeft: Math.round(shelf.scrollLeft), shelfLeft: Math.round(r.left), shelfRight: Math.round(r.right), firstCardX: Math.round(card.left), windowWidth: innerWidth };
  })()`;
  await js(win, `document.head.append(Object.assign(document.createElement("style"), { id: "probe-slow-sidebar", textContent: ":root { --duration-slow: 2400ms !important; }" }))`);
  const shelfGlide = {};
  for (const [name, collapsed] of [["home-collapse", true], ["home-expand", false]]) {
    shelfGlide[name] = { before: await js(win, shelfState) };
    await js(win, '__glassProbe.click("sidebar-collapse-btn")');
    await sleep(60);
    shelfGlide[name].start = await js(win, shelfState);
    await sleep(1100);
    shelfGlide[name].mid = await js(win, shelfState);
    await save(`${name}-mid`);
    await setSidebar(win, collapsed);
    shelfGlide[name].after = await js(win, shelfState);
  }
  await js(win, 'document.getElementById("probe-slow-sidebar").remove()');
  console.log(JSON.stringify({ shelfGlide }));
  await openLongList(win);
  await save("list");
  await save("list-bar@2x", BAR_CROP);
  // The seek bar while pressed: grown around the pointer.
  await js(win, `(() => { const i = document.querySelector('[data-testid="player-seek-slider"]'); const r = i.getBoundingClientRect(); i.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, clientX: r.left + r.width * 0.3, clientY: r.top + r.height / 2 })); })()`);
  await sleep(450);
  await save("list-bar-pressed@2x", BAR_CROP);
  await js(win, `document.querySelector('[data-testid="player-seek-slider"]').dispatchEvent(new PointerEvent("pointerup", { bubbles: true }))`);
  await sleep(300);
  await save("list-sidebar@2x", SIDEBAR_CROP);
  await js(win, `__glassProbe.clickFirst('[data-testid="track-actions-btn"]')`);
  await until(win, '__glassProbe.has("track-actions")');
  await sleep(700);
  await save("menu");
  await captureTrackDetails(win, (name) => save(`view-${name}`));
  // Remaining sidebar views, then the smallest window and the collapsed sidebar.
  for (const view of ["search", "library", "liked", "recent", "now-playing", "queue", "lyrics", "settings"]) {
    await navigate(win, view);
    await save(`view-${view}`);
  }
  // A grid on a colour ramp behind the glass: straight lines show how the rim bends them.
  await openLongList(win);
  await js(win, `__glassProbe.stress(${JSON.stringify(LENS_GRID)})`);
  await sleep(300);
  await save("lens-grid");
  await save("lens-grid-bar@2x", BAR_CROP);
  await save("lens-grid-sidebar@2x", SIDEBAR_CROP);
  await js(win, "__glassProbe.stress(null)");
  win.setContentSize(900, 600);
  await openLongList(win);
  await save("list-900x600");
  await navigate(win, "home");
  await js(win, 'document.querySelector(\'[data-testid="main-scroll-region"]\').scrollTop = 0');
  await sleep(200);
  await save("home-900x600");
  win.setContentSize(WINDOW.width, WINDOW.height);
  await openLongList(win);
  const anchors = { expanded: await js(win, SIDEBAR_ANCHORS) };
  // Click to lens back at normal speed: the sidebar's transitionend should release the hold
  // at about --duration-slow, well before the fallback (--duration-slow + 120 ms).
  anchors.settleMs = [await js(win, SIDEBAR_SETTLE), (await sleep(300), await js(win, SIDEBAR_SETTLE))];
  await setSidebar(win, true);
  await save("list-collapsed");
  anchors.collapsed = await js(win, SIDEBAR_ANCHORS);
  // The collapse transition slowed to 2.4 s and captured mid-way: both surfaces on the plain blur,
  // the labels gone or going, the icons where they were.
  await js(win, `document.head.append(Object.assign(document.createElement("style"), { id: "probe-slow-sidebar", textContent: ":root { --duration-slow: 2400ms !important; }" }))`);
  for (const [name, collapsed] of [["expand-mid", false], ["collapse-mid", true]]) {
    await js(win, '__glassProbe.click("sidebar-collapse-btn")');
    await sleep(200);
    await save(name);
    await save(`${name}-bar@2x`, { x: 0, y: WINDOW.height - 100, width: WINDOW.width, height: 100 });
    anchors[name] = { ...(await js(win, SIDEBAR_ANCHORS)), surfaces: (await js(win, "__glassProbe.surfaces()")).map((s) => `${s.testid}:${s.lens ? "lens" : "plain"}`) };
    await setSidebar(win, collapsed);
  }
  // A second click mid-way turns back from where the panel is: widths just before and after it.
  await js(win, '__glassProbe.click("sidebar-collapse-btn")');
  await sleep(300);
  const before = await js(win, SIDEBAR_ANCHORS);
  await js(win, '__glassProbe.click("sidebar-collapse-btn")');
  await sleep(120);
  const after = await js(win, SIDEBAR_ANCHORS);
  anchors.reverse = { beforeWidth: before.width, afterWidth: after.width, centres: [...new Set([...before.centres, ...after.centres])] };
  await setSidebar(win, false);
  await js(win, 'document.getElementById("probe-slow-sidebar").remove()');
  await setSidebar(win, false);
  win.setContentSize(900, 600);
  await sleep(400);
  anchors["900x600"] = await js(win, SIDEBAR_ANCHORS);
  win.setContentSize(WINDOW.width, WINDOW.height);
  console.log(JSON.stringify({ sidebar: anchors, surfaces: await js(win, "__glassProbe.surfaces()") }));
}

/**
 * Mid-transition frames, to see what a page change paints over the glass: page transitions are
 * slowed to 5 s and the window is captured while one runs. Saves the settled and the mid frame of
 * a few page changes in both themes, and prints, for each, what runs (the view-transition
 * animations and the element they belong to), whether both surfaces keep their lens, and the
 * mean colour of an empty patch of the sidebar and of the player bar, mid and settled: a glass
 * surface that loses its backdrop goes flat there.
 */
export async function runTransition(outDir) {
  mkdirSync(outDir, { recursive: true });
  const win = await launchApp();
  const [w, h] = win.getContentSize();
  // Empty glass: below the sidebar's last nav item, above its foot; the bar's gap between the
  // transport and its tail.
  const patches = { sidebar: { x: 40, y: Math.round(h * 0.68), width: 160, height: 24 }, bar: { x: Math.round(w * 0.73), y: h - 76, width: 24, height: 16 } };
  const mean = async () => {
    const out = {};
    for (const [name, rect] of Object.entries(patches)) {
      const shot = await grab(win, rect);
      const sum = [0, 0, 0];
      let n = 0;
      for (let py = 0; py < shot.ph; py++) for (let px = 0; px < shot.pw; px++) { shot.devicePixel(px, py).forEach((v, i) => (sum[i] += v)); n++; }
      out[name] = sum.map((v) => Math.round(v / n));
    }
    return out;
  };
  const state = () => js(win, `({
    running: document.getAnimations().filter((a) => a.effect?.pseudoElement?.startsWith("::view-transition")).map((a) => (a.effect.target.dataset.testid ?? a.effect.target.tagName) + a.effect.pseudoElement),
    lens: [...document.querySelectorAll(".app-sidebar, .app-player-bar")].map((el) => "lgLens" in el.dataset),
  })`);
  const report = {};
  for (const theme of ["dark", "light"]) {
    await setAppearance(win, { theme, reduceTransparency: false });
    await navigate(win, "home");
    const save = async (name) => savePng(outDir, (await grab(win)).image, `transition-${theme}-${name}.png`, w);
    await save("home-settled");
    await js(win, `document.head.append(Object.assign(document.createElement("style"), { id: "probe-slow-page", textContent: ':root { --duration-normal: 5s !important; }' })); true`);
    for (const [from, to] of [["home", "liked"], ["liked", "now-playing"], ["now-playing", "queue"]]) {
      await js(win, `__glassProbe.click("nav-${to}")`);
      await sleep(1800);
      await save(`${from}-to-${to}-mid`);
      const mid = { ...(await state()), patches: await mean() };
      await sleep(4200);
      await save(`${to}-settled`);
      report[`${theme} ${from}-to-${to}`] = { mid, settled: { ...(await state()), patches: await mean() } };
    }
    await js(win, 'document.getElementById("probe-slow-page").remove(); true');
  }
  console.log(JSON.stringify(report, null, 1));
}

/** The four layout tiers of the design language (content width under 880, 880-1399, 1400-2099, 2100 up). */
const TIER_WINDOWS = [
  ["compact", 900, 600],
  ["regular", 1440, 900],
  ["wide", 1920, 1080],
  ["ultrawide", 2560, 1080],
];

/**
 * One screenshot per main view at each layout tier and theme, for before/after comparison. Sizes
 * are CSS pixels; pass --force-device-scale-factor=1 when the largest does not fit the display.
 * `only` limits it to one theme ("dark" or "light").
 */
export async function runTiers(outDir, only = null) {
  mkdirSync(outDir, { recursive: true });
  const win = await launchApp();
  await js(win, `document.head.append(Object.assign(document.createElement("style"), { textContent: '[data-testid="banner-mock-mode"] { display: none !important; }' }))`);
  const sizes = {};
  for (const theme of only ? [only] : ["dark", "light"]) {
    await setAppearance(win, { theme, reduceTransparency: false });
    for (const [tier, width, height] of TIER_WINDOWS) {
      win.setContentSize(width, height);
      await sleep(400);
      sizes[tier] = win.getContentSize();
      const save = async (name) => savePng(outDir, (await grab(win)).image, `tier-${theme}-${tier}-${name}.png`, win.getContentSize()[0]);
      for (const view of ["home", "liked", "recent", "search", "now-playing", "queue", "library", "lyrics", "agent-activity", "settings"]) {
        await navigate(win, view);
        await sleep(350);
        await save(view);
      }
      await openLongList(win, 0);
      await save("playlist");
      await setSidebar(win, true);
      await save("playlist-collapsed");
      await setSidebar(win, false);
      await js(win, `__glassProbe.clickFirst('[data-testid="track-actions-btn"]')`);
      await captureTrackDetails(win, save);
    }
  }
  win.setContentSize(WINDOW.width, WINDOW.height);
  console.log(JSON.stringify({ sizes }));
}

/**
 * Real-input checks of the listening-flow UI in the mock app: the mouse wheel over the volume
 * control (a real wheel event, so a passive listener would show), closing lyrics with Ctrl+L back
 * to the list's scroll position, and the playback indicator while paused, playing and minimized.
 */
export async function runFlow(outFile) {
  const win = await launchApp();
  const wc = win.webContents;
  const report = {};
  const rect = (testid) => js(win, `(() => { const r = document.querySelector('[data-testid="${testid}"]').getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height }; })()`);
  const volume = () => js(win, `Number(document.querySelector('[data-testid="player-volume-slider"] input, input[data-testid="player-volume-slider"]')?.value ?? NaN)`);
  const scrolls = () => js(win, `({ main: document.querySelector('[data-testid="main-scroll-region"]').scrollTop, list: document.querySelector('[data-testid="virtual-track-list"]')?.scrollTop ?? null })`);
  const key = async (keyCode, modifiers = []) => {
    wc.sendInputEvent({ type: "keyDown", keyCode, modifiers });
    wc.sendInputEvent({ type: "keyUp", keyCode, modifiers });
    await sleep(120);
  };
  const indicator = () => js(win, `[...document.querySelectorAll('[data-testid="playback-indicator"]')].map((el) => ({
    where: el.closest('[data-testid="app-sidebar"]') ? "sidebar" : el.closest('[data-testid="view-queue"]') ? "queue" : "other",
    state: el.dataset.state, animated: el.dataset.animated,
    running: el.getAnimations({ subtree: true }).filter((a) => a.playState === "running").length,
  }))`);

  // 1. Wheel over the volume control on a scrolled list page.
  await openLongList(win, 2000);
  const vol0 = await volume();
  const s0 = await scrolls();
  const r = await rect("player-volume-area");
  const x = Math.round(r.x + r.w * 0.8);
  const y = Math.round(r.y + r.h / 2);
  wc.sendInputEvent({ type: "mouseMove", x, y });
  for (let i = 0; i < 3; i++) {
    wc.sendInputEvent({ type: "mouseWheel", x, y, deltaX: 0, deltaY: -100, wheelTicksY: -1, canScroll: true });
    await sleep(90);
  }
  await sleep(500);
  report.volumeWheel = { volumeBefore: vol0, volumeAfter: await volume(), scrollBefore: s0, scrollAfter: await scrolls() };

  // 2. Ctrl+L opens lyrics, Ctrl+L again comes back to the same scroll position.
  await js(win, `__glassProbe.scrollList(3000)`);
  await sleep(300);
  const before = await scrolls();
  await key("L", ["control"]);
  await until(win, '__glassProbe.has("view-lyrics")');
  await sleep(400);
  await key("L", ["control"]);
  await until(win, '__glassProbe.has("view-playlist") && __glassProbe.has("virtual-track-list")');
  await sleep(900);
  report.lyricsToggle = { before, after: await scrolls(), backOnPlaylist: await js(win, '__glassProbe.has("view-playlist")') };

  // 3. The indicator: paused, playing, minimized.
  await navigate(win, "queue");
  report.indicatorPaused = await indicator();
  await js(win, '__glassProbe.click("btn-play-pause")');
  await sleep(1200);
  report.indicatorPlaying = await indicator();
  win.minimize();
  await sleep(1200);
  report.minimized = { visibility: await js(win, "document.visibilityState"), indicator: await indicator() };
  win.restore();
  await sleep(800);
  report.restored = await indicator();
  writeFileSync(outFile, `${JSON.stringify(report, null, 2)}\n`);
  console.log(JSON.stringify(report, null, 1));
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

// Toggling the sidebar re-lays the page once, then glides the panel, the dock and the page column
// for 320 ms with both surfaces on the plain blur. Six toggles, 550 ms apart.
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

/**
 * `only === "collapse"` measures just the sidebar transition, three times in one launch, and
 * prints each run; the first includes warm-up (first layout of both states, shader compiles).
 */
export async function runPerf(outFile, seconds = 60, only = null) {
  const win = await launchApp();
  if (only === "collapse") {
    for (let run = 1; run <= 3; run++) console.log(JSON.stringify({ run, warm: run > 1, ...(await measureCollapse(win)) }));
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
  await js(win, "__glassProbe.scrollMain(0)");
  await measure("opaque (reduce transparency): page scroll", `__glassProbe.measureMain(${seconds}, 100000, 2600)`);

  // A third surface: Diagnostics (it opens from Settings now) over the Settings page scrolling
  // beneath it, as far as that page goes. The track menu cannot stand in: a scroll closes it.
  await setAppearance(win, { theme: "dark", reduceTransparency: false });
  await openDiagnostics(win);
  await sleep(600);
  await js(win, "__glassProbe.scrollMain(0)");
  await measure("liquid glass: Diagnostics dialog open over Settings (three surfaces), page scroll", `__glassProbe.measureMain(${seconds}, 100000, ${LEG})`);
  await closeOverlay(win);

  report.sidebarCollapse = await measureCollapse(win);
  console.log(JSON.stringify({ sidebarCollapse: { ...report.sidebarCollapse, slowFrames: undefined } }));

  report.environment = await environmentReport(win);
  mkdirSync(resolve(outFile, ".."), { recursive: true });
  writeFileSync(outFile, JSON.stringify(report, null, 2));
  return report;
}

// Sample frame pacing without forcing style or layout. A second pass reads the tracks for evidence.
async function measureOverlay(win, selector, duration, trigger, inspect = false) {
  const sample = js(win, `new Promise((resolve) => {
    const t0 = performance.now();
    const frames = [];
    let previous = null;
    const sample = (t) => {
      const el = document.querySelector(${JSON.stringify(selector)});
      const dt = previous === null ? null : t - previous;
      previous = t;
      if (dt === null || dt > 0) {
        const frame = { at: t - t0, dt, present: Boolean(el) };
        if (el && ${inspect}) {
          const style = getComputedStyle(el);
          const content = getComputedStyle(el.querySelector(".lg-overlay-content") ?? el);
          const rect = el.getBoundingClientRect();
          const id = el.style.getPropertyValue("--lg-backdrop").match(/#([^)]*)/)?.[1];
          const filter = id ? document.getElementById(id) : null;
          Object.assign(frame, {
            opacity: Number(style.opacity),
            scale: style.transform === "none" ? 1 : new DOMMatrixReadOnly(style.transform).a,
            background: style.backgroundColor, backdrop: style.backdropFilter, width: rect.width,
            frost: filter?.querySelector('[result="frost"]')?.getAttribute("stdDeviation") ?? null,
            tint: Number(style.getPropertyValue("--lg-morph-tint")),
            rim: Number(style.getPropertyValue("--lg-morph-rim")),
            content: Number(content.opacity),
            contentFilter: content.filter,
            leaving: el.dataset.leaving ?? null,
          });
        }
        frames.push(frame);
      }
      if (t - t0 < ${duration}) requestAnimationFrame(sample);
      else resolve(frames);
    };
    requestAnimationFrame(sample);
  })`);
  if (trigger) await trigger();
  return sample;
}

export async function runOverlay(outDir, { spike = false, dialogProfile = false } = {}) {
  mkdirSync(outDir, { recursive: true });
  const win = await launchApp();
  await setAppearance(win, { theme: "dark", reduceTransparency: false, reduceMotion: false });
  await until(win, '__glassProbe.has("mock-scenario-select")');
  const refreshMs = vsyncInterval(await js(win, "__glassProbe.measureIdle(2)"));
  if (dialogProfile) return profileDialog(win, outDir, refreshMs);
  const toast = ".toast";
  const scenario = (id) => () => js(win, `__glassProbe.select("mock-scenario-select", ${JSON.stringify(id)})`);
  const enter = scenario("authExpired");
  const exit = scenario("normal");
  const runs = [];
  for (let i = 0; i < 12; i++) {
    const entry = await measureOverlay(win, toast, 360, enter);
    await until(win, 'document.querySelector(".toast[data-lg-lens]")');
    const leave = await measureOverlay(win, toast, 450, exit);
    await until(win, '!document.querySelector(".toast")');
    runs.push({ entry, leave });
  }
  const timing = overlayPacing(runs, refreshMs);
  const first = await measureOverlay(win, toast, 380, enter, true);
  await until(win, 'document.querySelector(".toast[data-lg-lens]")');
  const stamp = await grab(win, TOAST_CROP);
  savePng(outDir, stamp.image, "overlay-toast-settled.png");
  const last = await measureOverlay(win, toast, 450, exit, true);
  const report = { spike, capturedAt: new Date().toISOString(), environment: await environmentReport(win), refreshMs, timing, first, last, runs, muteAudio: app.commandLine.hasSwitch("mute-audio") };
  if (!spike) {
    report.toast = await testToastMotion(win, outDir);
    report.menu = await testMenuMotion(win, refreshMs);
    report.dialog = await testDialogMotion(win, refreshMs);
    report.fallback = await testOverlayFallbacks(win);
    report.spring = await testSpring(win);
    report.failedChecks = [
      ...Object.entries(report.toast.checks),
      ...Object.entries(report.menu.checks),
      ...Object.entries(report.dialog.checks),
      ...Object.entries(report.fallback.checks),
      ...Object.entries(report.spring.checks),
    ].filter(([, ok]) => !ok).map(([name]) => name);
  }
  writeFileSync(join(outDir, spike ? "overlay-spike.json" : "overlay.json"), JSON.stringify(report, null, 2));
  console.log(JSON.stringify({
    spike, refreshMs, toast: motionSummary(timing), menu: motionSummary(report.menu?.timing),
    dialog: motionSummary(report.dialog?.timing), failedChecks: report.failedChecks,
  }, null, 2));
  if (report.failedChecks?.length) throw new Error(`Overlay checks failed: ${report.failedChecks.join(", ")}`);
}

function overlayPacing(runs, refreshMs) {
  return Object.fromEntries(["entry", "leave"].map((name) => {
    const intervals = (series) => series.flatMap((run) => run[name]).filter((f) => f.present && f.dt !== null).map((f) => f.dt);
    return [name, { all: pacing(intervals(runs), refreshMs), warm: pacing(intervals(runs.slice(1)), refreshMs) }];
  }));
}

const TOAST_SELECTOR = '[data-testid="toast-command-unknown"]';
const TOAST_LEAVING = '.toast[data-leaving]';
const OPEN_COMMAND_TOAST = '__glassProbe.click("btn-next")';

async function testToastMotion(win, outDir) {
  await setScenario(win, "indeterminateNext");
  // Keep input and screenshot capture off the frame pacing passes.
  const capture = async (name) => {
    const shot = await grab(win, TOAST_CROP);
    savePng(outDir, shot.image, `${name}.png`);
  };
  const entry = measureOverlay(win, ".toast", 350, () => js(win, OPEN_COMMAND_TOAST), true);
  const entryFrames = (await entry).filter((f) => f.present);
  await sleep(2750);
  const expiry = measureOverlay(win, ".toast", 700, null, true);
  await until(win, `document.querySelector(${JSON.stringify(TOAST_LEAVING)})`);
  const expired = await leavingState(win, TOAST_LEAVING);
  const expiryFrames = await expiry;
  await until(win, '!document.querySelector(".toast")');
  await js(win, OPEN_COMMAND_TOAST);
  await until(win, `document.querySelector(${JSON.stringify(TOAST_SELECTOR)})`);
  await sleep(300);
  await js(win, OPEN_COMMAND_TOAST);
  await until(win, 'document.querySelector(".toast[data-leaving=dismiss]")');
  const replacement = await leavingState(win, TOAST_LEAVING);
  await until(win, '!document.querySelector(".toast[data-leaving]")');

  // The official-page button's mock client action is a no-op, so nothing outside mock opens.
  await js(win, '__glassProbe.click("toast-action")');
  await until(win, 'document.querySelector(".toast[data-leaving=dismiss]")');
  const action = await leavingState(win, TOAST_LEAVING);
  await until(win, '!document.querySelector(".toast")');
  await js(win, OPEN_COMMAND_TOAST);
  await sleep(90);
  await capture("overlay-toast-entering");
  await until(win, 'document.querySelector(".toast[data-leaving=dissolve]")', 4000);
  await sleep(55);
  await capture("overlay-toast-dissolving");
  await until(win, '!document.querySelector(".toast")');

  const widths = [];
  for (const scenario of ["indeterminateNext", "authExpired", "offlineCached", "unsupported", "bridgeCrashed"]) {
    if (scenario === "indeterminateNext") {
      await js(win, OPEN_COMMAND_TOAST);
      await sleep(300);
    } else {
      await setScenario(win, scenario);
    }
    const width = await js(win, `(() => {
      const toast = document.querySelector(".toast:not([data-leaving])");
      const text = toast?.querySelector(".toast-text");
      return toast && text ? { kind: toast.dataset.testid, px: toast.getBoundingClientRect().width,
        textClips: text.scrollWidth > text.clientWidth, whiteSpace: getComputedStyle(text).whiteSpace } : null;
    })()`);
    widths.push({ scenario, ...width });
    // Clear a transient command toast before selecting the sticky scenarios.
    if (scenario === "indeterminateNext") {
      await js(win, '__glassProbe.click("toast-action")');
      await until(win, '!document.querySelector(".toast")');
    }
  }
  await setScenario(win, "normal");
  const first = entryFrames[0];
  const last = entryFrames.at(-1);
  const leaving = expiryFrames.filter((f) => f.leaving === "dissolve");
  const end = leaving.at(-1);
  return {
    checks: {
      clearToastEntry: first?.opacity === 1 && first?.scale >= 0.92 && first?.scale < 1 &&
        Number(first.frost) < 5 && first.backdrop.startsWith("url("),
      fiveToastTracks: entryFrames.every((f) => f.opacity === 1 && f.scale <= 1) &&
        last?.scale === 1 && Number(last?.frost) === 5 && last?.tint === 1 && last?.rim === 1 &&
        first?.content < 1 && first?.contentFilter !== "none" && last?.content === 1,
      naturalToastExpiry: retired(expired, 360) && end?.scale <= 0.911 && Number(end?.frost) === 0 &&
        end?.content === 0 && end.at - leaving[0].at >= 340 && end.at - leaving[0].at <= 415,
      toastReplacement: retired(replacement, 160) && replacement.activeToastCount === 1,
      toastActionDismissal: retired(action, 160) && action.activeToastCount === 0,
    },
    entryFrames, expiryFrames, expired, replacement, action, widths,
  };
}

async function leavingState(win, selector) {
  return js(win, `(() => {
    const el = document.querySelector(${JSON.stringify(selector)});
    if (!el) return null;
    const { x, y, width, height } = el.getBoundingClientRect();
    const hit = document.elementFromPoint(x + width / 2, y + height / 2);
    const surface = el.querySelector(".lg-overlay") ?? el;
    const time = getComputedStyle(surface).getPropertyValue("--lg-overlay-exit-duration").trim();
    return {
      inert: el.inert,
      ariaHidden: el.getAttribute("aria-hidden"),
      ids: Number(el.hasAttribute("data-testid")) + el.querySelectorAll("[data-testid]").length,
      pointerEvents: getComputedStyle(el).pointerEvents,
      pointerPasses: !hit || !el.contains(hit),
      mode: el.dataset.leaving,
      expectedMs: parseFloat(time) * (time.endsWith("ms") ? 1 : 1000),
      focused: document.activeElement?.dataset.testid,
      activeToastCount: document.querySelectorAll(".toast:not([data-leaving])").length,
    };
  })()`);
}

function retired(state, expectedMs) {
  return state?.inert && state.ariaHidden === "true" && state.ids === 0 &&
    state.pointerEvents === "none" && state.pointerPasses && state.expectedMs === expectedMs;
}

async function overlayRuns(win, selector, open, close, refreshMs) {
  const runs = [];
  for (let i = 0; i < 6; i++) {
    const entry = await measureOverlay(win, selector, 320, open);
    const leave = await measureOverlay(win, selector, 220, close);
    await until(win, `!document.querySelector(${JSON.stringify(selector)})`);
    runs.push({ entry, leave });
  }
  return overlayPacing(runs, refreshMs);
}

async function testMenuMotion(win, refreshMs) {
  await openLongList(win);
  const trigger = (index) => () => js(win, `(() => {
    const btn = document.querySelectorAll('[data-testid="track-actions-btn"]')[${index}];
    btn.focus({ preventScroll: true });
    btn.click();
    return true;
  })()`);
  const selector = ".track-menu";
  const leaving = ".track-menu[data-leaving]";
  const open = trigger(4);
  const close = () => {
    win.webContents.sendInputEvent({ type: "keyDown", keyCode: "Escape" });
    win.webContents.sendInputEvent({ type: "keyUp", keyCode: "Escape" });
  };
  const timing = await overlayRuns(win, selector, open, close, refreshMs);
  await open();
  await until(win, '__glassProbe.has("track-actions-menu")');
  await sleep(300);
  const first = await js(win, `(() => {
    window.__retainedMenu = document.querySelector("[data-testid=track-actions-menu]");
    return __retainedMenu.getAttribute("aria-label");
  })()`);
  close();
  await until(win, `document.querySelector(${JSON.stringify(leaving)})`);
  const closed = await leavingState(win, leaving);
  await open();
  await until(win, '__glassProbe.has("track-actions-menu")');
  const reopened = await js(win, `(() => {
    const menu = document.querySelector("[data-testid=track-actions-menu]");
    return { sameNode: menu === __retainedMenu, focused: menu.contains(document.activeElement),
      copies: document.querySelectorAll(".track-menu").length };
  })()`);
  close();
  await until(win, `document.querySelector(${JSON.stringify(leaving)})`);
  await trigger(5)();
  await until(win, '__glassProbe.has("track-actions-menu")');
  const changedRow = await js(win, `(() => {
    const menu = document.querySelector("[data-testid=track-actions-menu]");
    return { label: menu.getAttribute("aria-label"), focused: menu.contains(document.activeElement),
      copies: document.querySelectorAll(".track-menu").length };
  })()`);
  close();
  await until(win, '!document.querySelector(".track-menu")');
  return {
    timing, closed, reopened, changedRow,
    checks: {
      menuRetiresImmediately: retired(closed, 160) && closed.focused === "track-actions-btn",
      menuReversesOnSameNode: reopened.sameNode && reopened.focused && reopened.copies === 1,
      menuReopensFromAnotherRow: changedRow.label !== first && changedRow.focused && changedRow.copies === 1,
    },
  };
}

async function testDialogMotion(win, refreshMs) {
  await showDiagnosticsEntry(win);
  const selector = ".app-dialog-layer > .lg-overlay";
  const leaving = ".app-dialog-layer[data-leaving]";
  const open = () => js(win, `(() => {
    const btn = document.querySelector("[data-testid=settings-open-diagnostics]");
    btn.focus({ preventScroll: true });
    btn.click();
    return true;
  })()`);
  const close = () => js(win, '__glassProbe.click("dialog-close-btn")');
  const timing = await overlayRuns(win, selector, open, close, refreshMs);
  await open();
  await until(win, '__glassProbe.has("accessible-dialog")');
  await sleep(300);
  await js(win, 'window.__retainedDialog = document.querySelector("[data-testid=accessible-dialog]"); true');
  await close();
  await until(win, `document.querySelector(${JSON.stringify(leaving)})`);
  const closed = await leavingState(win, leaving);
  await open();
  await until(win, '__glassProbe.has("accessible-dialog")');
  const reopened = await js(win, `(() => ({
    sameNode: document.querySelector("[data-testid=accessible-dialog]") === __retainedDialog,
    copies: document.querySelectorAll(".app-dialog-layer").length,
    focused: document.activeElement.dataset.testid,
  }))()`);
  await close();
  await until(win, '!document.querySelector(".app-dialog-layer")');
  return {
    timing, closed, reopened,
    checks: {
      dialogRetiresImmediately: retired(closed, 160) && closed.focused === "settings-open-diagnostics",
      dialogReversesOnSameNode: reopened.sameNode && reopened.copies === 1 && reopened.focused === "dialog-close-btn",
    },
  };
}

async function testOverlayFallbacks(win) {
  const pick = (reduceMotion, reduceTransparency) => setAppearance(win, { reduceMotion, reduceTransparency });
  const style = `(() => {
    const cs = getComputedStyle(document.querySelector("[data-testid=accessible-dialog]"));
    return {
      transition: cs.transitionProperty, duration: cs.transitionDuration, scale: cs.transform, opacity: cs.opacity,
      backdrop: cs.backdropFilter, color: cs.backgroundColor,
    };
  })()`;
  const shot = async () => {
    await openDiagnostics(win);
    const state = await js(win, style);
    await closeOverlay(win);
    return state;
  };
  await pick(true, false);
  const reduceMotion = await shot();
  await pick(false, true);
  const reduceTransparency = await shot();
  await pick(false, false);
  await emulateMedia(win, [{ name: "prefers-reduced-motion", value: "reduce" }]);
  const systemMotion = await shot();
  await emulateMedia(win, [{ name: "prefers-reduced-motion", value: "" }, { name: "prefers-reduced-transparency", value: "reduce" }]);
  const systemTransparency = await shot();
  await emulateMedia(win, [{ name: "prefers-reduced-transparency", value: "" }]);
  const fade = (s) => s.transition === "opacity" && s.duration === "0.14s" && s.scale === "none";
  return {
    reduceMotion, reduceTransparency, systemMotion, systemTransparency,
    checks: {
      reducedMotionOverlayFade: fade(reduceMotion) && reduceMotion.backdrop.startsWith("url("),
      reducedTransparencyOverlayFade: fade(reduceTransparency) && reduceTransparency.backdrop === "none",
      systemMotionOverlayFade: fade(systemMotion),
      systemTransparencyOverlayFade: fade(systemTransparency) && systemTransparency.backdrop === "none",
    },
  };
}

async function testSpring(win) {
  await navigate(win, "home");
  const wc = win.webContents;
  const sample = async (id) => {
    const at = await js(win, `__glassProbe.centre(${JSON.stringify(id)})`);
    wc.sendInputEvent({ type: "mouseMove", ...at });
    wc.sendInputEvent({ type: "mouseDown", button: "left", clickCount: 1, ...at });
    await sleep(160);
    const frames = js(win, `new Promise((resolve) => {
      const el = document.querySelector('[data-testid="${id}"]');
      const scale = () => {
        const value = getComputedStyle(el).transform;
        return value === "none" ? 1 : new DOMMatrixReadOnly(value).a;
      };
      const held = scale();
      el.addEventListener("pointerup", () => {
        const release = performance.now();
        const frames = [];
        const frame = (t) => {
          frames.push({ at: t - release, scale: scale() });
          if (t - release < 320) requestAnimationFrame(frame);
          else resolve({ held, frames });
        };
        requestAnimationFrame(frame);
      }, { once: true });
    })`);
    await sleep(25);
    wc.sendInputEvent({ type: "mouseUp", button: "left", clickCount: 1, ...at });
    const result = await frames;
    const peak = Math.max(...result.frames.map(({ scale }) => scale));
    const lastMotion = result.frames.findLastIndex(({ scale }) => scale !== 1);
    const restMs = result.frames[lastMotion + 1]?.at;
    const peakIndex = result.frames.findIndex(({ scale }) => scale === peak);
    const pass = result.held < 1 && peak > 1.002 && restMs >= 220 && restMs <= 260 &&
      result.frames.slice(1, peakIndex + 1).every((f, i) => f.scale >= result.frames[i].scale) &&
      result.frames.slice(peakIndex + 1).every((f, i) => f.scale <= result.frames[peakIndex + i].scale);
    return { ...result, peak, restMs, pass };
  };
  const play = await sample("btn-play-pause");
  const nav = await sample("nav-home");
  return { play, nav, checks: { oneSmallButtonOvershoot: play.pass, oneNavItemOvershoot: nav.pass } };
}

function motionSummary(timing) {
  if (!timing) return undefined;
  return Object.fromEntries(Object.entries(timing).map(([phase, { warm }]) => [phase, {
    p95Ms: warm.p95Ms, missed: warm.droppedFramePercent, long: warm.longFramePercent,
  }]));
}

// Compare the shipped fixed shadow with the original moving shadow on the same dialog.
async function profileDialog(win, outDir, refreshMs) {
  const selector = ".app-dialog-layer > .lg-overlay";
  const open = () => js(win, '__glassProbe.click("settings-open-diagnostics")');
  const close = () => js(win, '__glassProbe.click("dialog-close-btn")');
  const movingShadow = `
    ${selector} {
      box-shadow:
        var(--lg-elevation),
        inset 0 var(--lg-px) 0 0 var(--lg-bezel-light),
        inset 0 -1px 1px -1px var(--lg-bezel-shade),
        inset 0 0 calc(var(--lg-bezel) * 0.9) calc(var(--lg-bezel) * -0.6) var(--lg-sheen) !important;
    }
    @starting-style { ${selector} { box-shadow: none !important; } }
    ${selector}[data-leaving] { box-shadow: none !important; }
  `;
  await showDiagnosticsEntry(win);
  const runs = { fixedShadow: await overlayRuns(win, selector, open, close, refreshMs) };
  await js(win, `(() => {
    const style = document.createElement("style");
    style.textContent = ${JSON.stringify(movingShadow)};
    document.head.append(style);
    return true;
  })()`);
  runs.movingShadow = await overlayRuns(win, selector, open, close, refreshMs);
  const summary = Object.fromEntries(Object.entries(runs).map(([name, timing]) => [name, motionSummary(timing)]));
  const environment = await environmentReport(win);
  writeFileSync(join(outDir, "dialog-profile.json"), JSON.stringify({ environment, refreshMs, runs, summary }, null, 2));
  console.log(JSON.stringify(summary, null, 2));
}
