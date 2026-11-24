// Supplemental phase 2 checks on the built, muted desktop app with synthetic data.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { app, screen } from "electron";
import { launchApp } from "./real-ui.mjs";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const tid = (id) => `[data-testid="${id}"]`;
const sidebarToggleGlyph = `${tid("sidebar-collapse-btn")} > .icon`;
const rect = (selector) => `__phase2.box(document.querySelector(${JSON.stringify(selector)}))`;

function installHelpers() {
  const q = (selector, root = document) => root.querySelector(selector);
  const all = (selector, root = document) => [...root.querySelectorAll(selector)];
  const id = (name) => q(`[data-testid="${name}"]`);
  const box = (element) => {
    if (!element) return null;
    const { x, y, width, height } = element.getBoundingClientRect();
    return { x, y, width, height, right: x + width, bottom: y + height };
  };
  const visible = (el) => !!el && el.checkVisibility() && box(el).width > 0 && box(el).height > 0;
  const style = (el, key) => getComputedStyle(el)[key];
  const main = () => id("main-scroll-region");
  const select = (name, value) => {
    const el = id(name);
    el.value = value;
    el.dispatchEvent(new Event("change", { bubbles: true }));
  };
  const frames = (read, ms, limit = Infinity) => new Promise((resolve) => {
    const samples = [];
    const start = performance.now();
    const tick = (now) => {
      const t = Math.max(0, Math.round(now - start));
      samples.push({ t, ...read() });
      if (t < ms && samples.length < limit) requestAnimationFrame(tick);
      else resolve(samples);
    };
    requestAnimationFrame(tick);
  });
  const transform = (el) => new DOMMatrixReadOnly(style(el, "transform"));
  const artwork = new Map();
  const ambient = () => {
    const root = id("ambient-light");
    return {
      rect: box(root), scroll: main().scrollTop, opacity: Number(style(root, "opacity")),
      full: root.hasAttribute("data-full"),
      layers: all(".ambient-layer", root).map((el) => ({
        box: box(el), opacity: Number(style(el, "opacity")), leaving: el.hasAttribute("data-leaving"),
        art: all("img", el).map((img) => {
          // The mock uses data URLs. Keep a short identifier in the frame report.
          const src = img.src;
          if (!artwork.has(src)) artwork.set(src,
            Array.from(src).reduce((sum, ch) => (Math.imul(31, sum) + ch.charCodeAt(0)) | 0, 0));
          return { sig: artwork.get(src), ready: img.complete && img.naturalWidth > 0 };
        }),
      })),
    };
  };
  const sidebar = () => {
    const shell = q(".app-shell");
    const frame = q(".sidebar-frame");
    const sidebar = id("app-sidebar");
    const panel = box(sidebar);
    const glyph = box(q(".sidebar-item > .icon", sidebar));
    const label = q(".sidebar-item .sidebar-text", sidebar);
    const glass = getComputedStyle(sidebar);
    return {
      width: Math.min(box(frame).right, panel.right) - panel.x,
      glyphX: glyph.x + glyph.width / 2, glyphY: glyph.y + glyph.height / 2,
      textOpacity: Number(style(label, "opacity")), textTransform: style(label, "transform"),
      collapsed: shell.dataset.sidebar === "collapsed", moving: shell.hasAttribute("data-sidebar-motion"),
      lens: sidebar.hasAttribute("data-lg-lens"), backdrop: glass.backdropFilter,
      theme: document.documentElement.dataset.theme, viewport: [innerWidth, innerHeight],
      transitionDuration: style(frame, "transitionDuration"),
      stack: [style(id("ambient-light"), "zIndex"), style(main(), "zIndex"), style(frame, "zIndex")],
      layerHit: document.elementFromPoint(glyph.x + glyph.width / 2, glyph.y + glyph.height / 2)?.closest(".app-sidebar") === sidebar,
      snapshots: document.getAnimations().flatMap((anim) => {
        const effect = anim.effect;
        return effect?.pseudoElement?.startsWith("::view-transition")
          ? [(effect.target?.dataset.testid ?? effect.target?.tagName) + effect.pseudoElement]
          : [];
      }),
    };
  };
  const colours = new Map();
  function rgb(color) {
    if (!colours.has(color)) {
      const canvas = document.createElement("canvas");
      canvas.width = canvas.height = 1;
      const context = canvas.getContext("2d", { willReadFrequently: true });
      context.fillStyle = color;
      context.fillRect(0, 0, 1, 1);
      colours.set(color, Array.from(context.getImageData(0, 0, 1, 1).data));
    }
    return colours.get(color);
  }
  function focused() {
    const el = document.activeElement;
    const paint = el.matches(".scrub-input")
      ? q(".scrub-track", el.closest(".scrub"))
      : el.matches(".search-field input") ? el.closest(".search-field") : el;
    const cs = getComputedStyle(paint);
    return {
      target: el.dataset.testid ?? (el.getAttribute("aria-label") || el.textContent.trim().slice(0, 65) || el.tagName),
      tag: el.tagName, rect: box(paint), source: document.documentElement.dataset.focusSource,
      focusVisible: el.matches(":focus-visible"), outline: cs.outlineStyle, width: parseFloat(cs.outlineWidth),
      offset: parseFloat(cs.outlineOffset), color: rgb(cs.outlineColor),
      shadow: cs.boxShadow,
    };
  }
  window.__phase2 = { q, all, id, box, visible, style, main, select, frames, transform, ambient, sidebar, focused };
}

function listGeometry() {
  const { q, all, id, box, main, visible, style } = window.__phase2;
  const list = id("virtual-track-list");
  const header = q(".track-list-header", list.closest(".track-list"));
  const sentinel = '.tl-row [data-testid="track-like-btn"]';
  const rows = all(".tl-row", list).slice(0, 4);
  const cell = (element) => visible(element) ? box(element) : null;
  const labelLeft = (element) => {
    const range = document.createRange();
    range.selectNodeContents(element);
    return range.getBoundingClientRect().left;
  };
  const columns = (row, isHeader) => ({
    index: cell(q(".tl-num", row)), title: cell(q(".tl-title", row)),
    labelLeft: labelLeft(q(isHeader ? ".tl-title-head" : ".tl-title-text", row)),
    album: cell(q(".tl-album", row)), like: cell(q(".tl-like", row)),
    duration: cell(q(".tl-dur", row)), more: cell(q(".tl-more", row)),
  });
  const page = q(".app-page");
  const overflow = all("*", page)
    .filter((el) => /auto|scroll/.test(style(el, "overflowY")) && el.scrollHeight > el.clientHeight + 1)
    .map((el) => el.dataset.testid ?? el.className).slice(0, 20);
  return {
    viewport: [innerWidth, innerHeight], dpr: devicePixelRatio,
    content: box(q(".app-content")), page: box(page),
    main: { box: box(main()), scrollTop: main().scrollTop, scrollHeight: main().scrollHeight, overflowY: style(main(), "overflowY") },
    header: columns(header, true), rows: rows.map((row) => columns(row, false)),
    headerGap: style(header, "columnGap"), rowGap: style(rows[0], "columnGap"),
    overflow, list: box(list), bar: box(id("player-bar")),
    likeButtons: all(sentinel, list).filter(visible).length,
    loadMore: all("button", page).filter((el) => /load more/i.test(el.textContent.trim())).map((el) => el.textContent.trim()),
  };
}

function layoutGuard() {
  const { q, all, id, box, main, frames } = window.__phase2;
  const fixed = [q(".app-page"), q(".track-list-header"), id("virtual-track-list"), id("player-bar")];
  const row = q(".tl-row");
  const measure = () => [...fixed, row, ...all(".tl-row").slice(1, 4)].map(box)
    .flatMap((r) => r ? [r.x, r.y, r.width, r.height] : [null]).concat(main().scrollTop, main().scrollHeight);
  const baseline = measure();
  const shifts = [];
  const observer = new PerformanceObserver((list) => {
    for (const entry of list.getEntries()) shifts.push({ value: entry.value, hadRecentInput: entry.hadRecentInput });
  });
  observer.observe({ type: "layout-shift" });
  return frames(() => {
    const next = measure();
    return { maxDelta: next.length === baseline.length
      ? Math.max(...next.map((value, index) => Math.abs(value - baseline[index]))) : null };
  }, 650).then((samples) => {
    observer.disconnect();
    return { shifts, samples: samples.length, maxDelta: samples.some((sample) => sample.maxDelta === null)
      ? null : Math.max(...samples.map((sample) => sample.maxDelta)) };
  });
}

function dockGeometry() {
  const { q, all, id, box, visible } = window.__phase2;
  const bar = id("player-bar");
  const metric = (selector) => {
    const el = q(selector);
    const r = box(el);
    return { ...r, text: el.textContent.trim(), clipped: el.scrollWidth > el.clientWidth + 1 };
  };
  return {
    viewport: innerWidth, dock: box(q(".app-player-dock")), bar: box(bar), cover: box(q(".player-cover")),
    buttons: all("button", bar).filter(visible).map((el) => ({
      id: el.dataset.testid ?? el.getAttribute("aria-label"), width: el.offsetWidth, height: el.offsetHeight,
      title: el.title, disabled: el.disabled,
    })),
    seek: box(id("player-seek-slider")), track: box(q(".player-seek .scrub-track")),
    elapsed: metric(".player-pos"), duration: metric(".player-duration"),
    song: metric(".player-track-title"), artist: metric(".player-track-artist"),
  };
}

function near(a, b, tolerance = 1) {
  return Number.isFinite(a) && Number.isFinite(b) && Math.abs(a - b) <= tolerance;
}

function luminance(rgb) {
  const linear = rgb.slice(0, 3).map((channel) => {
    const value = channel / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
}

async function focusContrast(win, state, viewportWidth, hideRing) {
  const image = await win.webContents.capturePage();
  await hideRing(true);
  let baseline;
  try {
    baseline = (await win.webContents.capturePage()).toBitmap();
  } finally {
    await hideRing(false);
  }
  const { width, height } = image.getSize();
  const scale = width / viewportWidth;
  const pixels = image.toBitmap();
  const sample = (x, y, bitmap = pixels) => {
    const px = Math.floor(x * scale), py = Math.floor(y * scale);
    if (px < 0 || py < 0 || px >= width || py >= height) return null;
    const from = (py * width + px) * 4;
    return [bitmap[from + 2], bitmap[from + 1], bitmap[from]];
  };
  const { x, y, width: w, height: h } = state.rect;
  const distance = state.offset + state.width / 2;
  const sides = [
    [(along, off) => [x + w * along, y - off], w],
    [(along, off) => [x + w * along, y + h + off], w],
    [(along, off) => [x - off, y + h * along], h],
    [(along, off) => [x + w + off, y + h * along], h],
  ];
  const colorError = (rgb) => Math.max(...rgb.map((value, index) => Math.abs(value - state.color[index])));
  const values = [];
  for (const [point, length] of sides) {
    if (length < 12) continue;
    for (const along of [0.3, 0.4, 0.5, 0.6, 0.7]) {
      const samples = [-0.5, 0, 0.5].map((offset) => {
        const p = point(along, distance + offset / scale);
        return { line: sample(...p), under: sample(...p, baseline) };
      });
      const match = samples.filter(({ line }) => line).sort((a, b) => colorError(a.line) - colorError(b.line))[0];
      if (!match || colorError(match.line) > 24) continue;
      const { line, under } = match;
      const a = luminance(line), b = luminance(under);
      values.push((Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05));
    }
  }
  return { matched: values.length, min: values.length ? Number(Math.min(...values).toFixed(2)) : null };
}

async function recordFrames(wc, dir, name, operation) {
  const debuggerClient = wc.debugger;
  if (!debuggerClient.isAttached()) debuggerClient.attach("1.3");
  const frames = [];
  function onMessage(_event, method, payload) {
    if (method !== "Page.screencastFrame") return;
    const filename = `${name}-${String(frames.length).padStart(3, "0")}.jpg`;
    frames.push({ filename, timestamp: payload.metadata.timestamp, device: [payload.metadata.deviceWidth, payload.metadata.deviceHeight] });
    writeFileSync(join(dir, filename), Buffer.from(payload.data, "base64"));
    void debuggerClient.sendCommand("Page.screencastFrameAck", { sessionId: payload.sessionId }).catch(() => {});
  }
  debuggerClient.on("message", onMessage);
  await debuggerClient.sendCommand("Page.startScreencast", { format: "jpeg", quality: 93, everyNthFrame: 1 });
  try {
    await sleep(130);
    return { ...(await operation()), frames };
  } finally {
    await debuggerClient.sendCommand("Page.stopScreencast");
    debuggerClient.removeListener("message", onMessage);
  }
}

export async function runPhase2Acceptance(outDir, sections = []) {
  if (!app.commandLine.hasSwitch("mute-audio")) throw new Error("phase 2 acceptance requires --mute-audio");
  mkdirSync(outDir, { recursive: true });
  const win = await launchApp();
  const wc = win.webContents;
  wc.setAudioMuted(true);
  wc.focus();
  await wc.executeJavaScript(`(${installHelpers.toString()})()`);
  const js = async (source) => {
    const reply = await wc.executeJavaScript(`(async () => {
      try { return { value: await (${source}) }; }
      catch (error) { return { error: String(error.stack ?? error) }; }
    })()`);
    if (reply.error) throw new Error(`${reply.error} (in ${source.slice(0, 140)})`);
    return reply.value;
  };
  const read = (fn) => js(`(${fn.toString()})()`);
  const helper = (expr) => js(`((p) => (${expr}))(window.__phase2)`);
  const until = async (predicate, ms = 4000) => {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      if (await js(`Boolean(${predicate})`)) return;
      await sleep(40);
    }
    throw new Error(`Timed out waiting for ${predicate}`);
  };
  const save = async (name) => writeFileSync(join(outDir, `${name}.png`), (await wc.capturePage()).toPNG());
  const go = async (name) => {
    await helper(`p.id("nav-${name}").click(), true`);
    await until(`__phase2.id("view-${name}")`);
    await sleep(280);
  };
  const size = async (width, height, { zoomToFit = false } = {}) => {
    wc.setZoomFactor(1);
    win.setContentSize(width, height);
    await sleep(350);
    const actual = win.getContentSize();
    if (zoomToFit && actual[0] < width - 1) {
      wc.setZoomFactor(actual[0] / width);
      await sleep(350);
    }
    return { requested: [width, height], actual, zoom: wc.getZoomFactor(), viewport: await js("[innerWidth, innerHeight]") };
  };
  const cssPoint = async (selector) => {
    const r = await js(rect(selector));
    if (!r || r.width < 1 || r.height < 1) throw new Error(`No target box: ${selector}`);
    const scale = win.getContentSize()[0] / await js("innerWidth");
    return { x: Math.round((r.x + r.width / 2) * scale), y: Math.round((r.y + r.height / 2) * scale) };
  };
  const pointer = (type, point, modifiers = {}) => wc.sendInputEvent({ type, ...point, ...modifiers });
  const press = async (selector) => {
    const at = await cssPoint(selector);
    pointer("mouseMove", at);
    pointer("mouseDown", at, { button: "left", clickCount: 1 });
    return () => pointer("mouseUp", at, { button: "left", clickCount: 1 });
  };
  const click = async (selector) => (await press(selector))();
  const key = (keyCode) => {
    wc.sendInputEvent({ type: "keyDown", keyCode });
    wc.sendInputEvent({ type: "keyUp", keyCode });
  };
  const check = (pass, evidence) => ({ status: pass ? "PASS_MOCK" : "FAIL", evidence });
  const theme = async (value) => {
    await go("settings");
    await helper(`p.select("setting-theme-select", "${value}"), true`);
    await sleep(440);
  };
  const frames = (expression, ms) => helper(`p.frames(() => (${expression}), ${ms})`);
  const record = (name, action) => recordFrames(wc, join(outDir, "frames"), name, action);
  const repoRoot = resolve(import.meta.dirname, "../../..");
  const buildFiles = ["apps/desktop/dist/main.mjs", "ui/dist/index.html"];
  const report = {
    command: process.argv.slice(1),
    sections,
    head: execFileSync("git", ["rev-parse", "HEAD"], { cwd: repoRoot, encoding: "utf8" }).trim(),
    buildSha256: Object.fromEntries(buildFiles.map((file) =>
      [file, createHash("sha256").update(readFileSync(join(repoRoot, file))).digest("hex")])),
    environment: {
      electron: process.versions.electron, chromium: process.versions.chrome,
      ozone: app.commandLine.getSwitchValue("ozone-platform") || "(default)",
      forceDsf: app.commandLine.getSwitchValue("force-device-scale-factor") || "(default)",
      nativeSession: process.env.XDG_SESSION_TYPE ?? null,
      display: screen.getPrimaryDisplay().size, dpr: await js("devicePixelRatio"),
      gpuDevice: (await app.getGPUInfo("basic")).gpuDevice,
      gpuFeatures: app.getGPUFeatureStatus(),
      mock: await helper('p.q(".sidebar-brand .chip")?.innerText === "Mock data"'), muted: wc.isAudioMuted(),
    },
  };
  const reportPath = join(outDir, "report.json");
  const flush = () => writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);
  async function group(name, action) {
    if (sections.length && !sections.some((section) => section === name || name.startsWith(section + "-"))) return;
    try {
      await action();
      console.log(`phase2-acceptance ${name} complete`);
    } catch (error) {
      (report.errors ??= {})[name] = String(error.stack ?? error);
      console.error(`phase2-acceptance ${name}: ${error.message}`);
    } finally {
      flush();
    }
  }

  await group("lists", async () => {
    await go("settings");
    await helper('p.id("btn-load-10k-fixture").click(), true');
    await until('__phase2.id("view-playlist") && __phase2.id("virtual-track-row")');
    const tiers = [
      ["compact", 900, 600, 0, 880],
      ["regular", 1440, 900, 880, 1400],
      ["wide", 1920, 1080, 1400, 2100],
      ["ultrawide", 2560, 1080, 2100, Infinity],
    ];
    const selector = tid("track-like-btn");
    const layout = {};
    const shift = {};
    for (const [name, w, h, min, max] of tiers) {
      const sizing = await size(w, h, { zoomToFit: true });
      await helper("p.main().scrollTop = 0, true");
      await sleep(360);
      // Bring the list header into frame without using scrollIntoView on a virtualized row.
      await helper('p.main().scrollTop = Math.max(0, p.box(p.q(".track-list-header")).y - 110), true');
      await sleep(480);
      const result = await read(listGeometry);
      const edges = ["index", "title", "album", "like", "duration", "more"];
      const alignment = result.rows.map((row) => Object.fromEntries(edges.map((col) => [col,
        result.header[col] && row[col] ? [row[col].x - result.header[col].x, row[col].right - result.header[col].right] : null,
      ])));
      const labelOffsets = result.rows.map((row) => row.labelLeft - result.header.labelLeft);
      const aligned = alignment.flatMap(Object.values).filter(Boolean).every(([left, right]) => near(left, 0) && near(right, 0))
        && labelOffsets.every((offset) => near(offset, 0));
      await save(`C-01-${name}`);
      const pass = result.content.width >= min && result.content.width < max &&
        (name !== "wide" || (near(result.viewport[0], 1920, 2) && result.page.width >= 1500)) &&
        result.main.overflowY === "auto" && result.main.scrollHeight > result.main.box.height &&
        result.overflow.length === 0 && result.loadMore.length === 0 &&
        result.list.y < result.bar.y && result.list.bottom > result.bar.bottom && aligned &&
        (name === "compact" ? result.likeButtons === 0 : result.likeButtons > 0);
      layout[name] = check(pass, { sizing, result, alignment, labelOffsets });
      report["C-01"] = layout;

      if (name === "compact") continue;
      if (result.likeButtons === 0) {
        shift[name] = { status: "BLOCKED", evidence: "No rendered Like button at this tier" };
        report["D-03"] = shift;
        continue;
      }
      const first = await helper('p.id("track-like-btn").getAttribute("aria-pressed")');
      const transitions = [];
      for (const wanted of [first === "true" ? "false" : "true", first]) {
        const track = await helper('p.id("track-like-btn").getAttribute("aria-label")');
        const sampling = js(`(${layoutGuard.toString()})()`);
        await sleep(65);
        await click(selector);
        let changed = true;
        await until(`__phase2.id("track-like-btn").getAttribute("aria-pressed") === "${wanted}"`, 1800)
          .catch(() => { changed = false; });
        const geometry = await sampling;
        const done = await js('document.body.innerText.includes("Done:") || !!__phase2.id("toast-write-done")');
        transitions.push({ wanted, track, changed, done, ...geometry });
        await save(`D-03-${name}-${wanted}`);
      }
      const both = transitions.every((entry) => entry.changed);
      layout[name].evidence.likeChangedAndRestored = both;
      if (!both) layout[name].status = "FAIL";
      shift[name] = check(transitions.every((entry) =>
        entry.changed && !entry.done && entry.maxDelta === 0 && entry.shifts.length === 0), transitions);
      report["D-03"] = shift;
      flush();
    }
    await size(1280, 800);
  });

  await group("browse-spacing", async () => {
    await size(1280, 800);
    const spaces = {};
    for (const view of ["home", "library", "liked", "recent", "agent-activity"]) {
      await go(view);
      spaces[view] = await helper(`(() => {
        const h = p.id("view-${view}").querySelector("h1");
        const section = p.id("view-${view}");
        const header = h.closest(".page-hero");
        const ruler = getComputedStyle(section);
        return { title: h.textContent.trim(), titleRect: p.box(h), headingClass: h.className,
          header: p.box(header), first: p.box(header?.nextElementSibling), page: p.box(p.q(".app-page")),
          sectionGaps: [...section.children].slice(1).map((el) => getComputedStyle(el).marginTop),
          rulers: ["--space-12", "--space-16", "--space-24", "--space-32", "--space-48"].map((token) => ruler.getPropertyValue(token).trim()),
        };
      })()`);
    }
    const home = spaces.home;
    report["S-01"] = check(["home", "library", "liked", "recent"].every((name) => {
      const item = spaces[name];
      const gapToken = ["liked", "recent"].includes(name) ? 2 : 3;
      return near(item.header?.x, home.header?.x) && near(item.header?.y, home.header?.y)
        && near(item.first?.y - item.header?.bottom, parseFloat(item.rulers[gapToken]));
    }), spaces);
    const dj = spaces["agent-activity"];
    report.djHeader = check(near(dj.first?.y - dj.header?.bottom, 32) &&
      near(dj.titleRect.x, home.titleRect.x) && dj.headingClass === "browse-title", dj);
    await save("DJ-activity");

    await go("home");
    const homeText = await helper('p.id("view-home").innerText');
    report["D-01"] = check(!/personalized recommendations/i.test(homeText), { homeText });
    await save("D-01-home");
  });

  await group("browse-search", async () => {
    await size(1280, 800);
    await go("search");
    await until('__phase2.id("search-input")');
    const startState = () => helper(`(() => {
      const start = p.id("search-start"), results = p.id("search-results");
      return { startActive: !start.inert, resultsActive: !results.inert,
        headings: p.all("h2", start).map((h) => h.textContent.trim()),
        sources: p.all('[data-testid^="search-library-"], [data-testid^="search-jump-"], [data-testid="search-recent-chip"]', start)
          .map((el) => ({ id: el.dataset.testid, label: el.textContent.trim() })), resultRows: p.all(".tl-row", results).length,
        opacity: [p.style(start, "opacity"), p.style(results, "opacity")],
      };
    })()`);
    await click(tid("search-input"));
    const pointerRing = await helper("p.focused()");
    const term = (await helper('p.id("player-track-title").textContent.trim()')).split(" ")[0];
    await wc.insertText(term);
    // Commit through the same form submit handler triggered by Enter.
    await helper('p.q(".search-form").requestSubmit(), true');
    await until('__phase2.id("search-results").querySelector(".tl-row")');
    await click(tid("search-clear-btn"));
    await sleep(420);
    const start = await startState();
    await save("C-02-search-start");
    const cross = frames('{ opacity: [p.style(p.id("search-start"), "opacity"), p.style(p.id("search-results"), "opacity")] }', 460);
    await sleep(65);
    await wc.insertText(term);
    const crossFade = await cross;
    await until('__phase2.id("search-results").querySelector(".tl-row")');
    const results = await startState();
    const interpolates = crossFade.some((frame) => frame.opacity.some((opacity) => Number(opacity) > 0 && Number(opacity) < 1));
    report["C-02"] = check(start.startActive && start.headings.length >= 3 &&
      results.resultsActive && results.resultRows > 0 && interpolates,
    { fixtureOnly: true, term, start, results, crossFade });
    await save("C-02-search-results");
    report.searchPointerFocus = check(pointerRing.outline === "none", pointerRing);
  });

  await group("browse-player", async () => {
    const now = {};
    await go("now-playing");
    await until('__phase2.id("np-lyrics-preview")');
    for (const [name, w, h] of [["wide", 1920, 1080], ["compact", 900, 650]]) {
      const sizing = await size(w, h, { zoomToFit: true });
      const geometry = await helper(`(() => {
        const view = p.id("view-now-playing");
        return { cover: p.box(p.id("np-cover-wrap")), info: p.box(p.q(".np-info")), lyrics: p.box(p.id("np-lyrics-preview")),
          ambient: p.ambient(), screen: [innerWidth, innerHeight], progress: p.all('input[type="range"], [role="slider"]', view).length,
          internalState: /player page state|^Playback state/m.test(view.innerText),
          rawChips: p.all(".chip", view).filter((chip) => /codec|audio mode|quality/i.test(chip.textContent)).map((chip) => chip.textContent.trim()),
          badges: p.all('[data-testid="quality-badge"]').map((badge) => ({ tag: badge.tagName, role: badge.getAttribute("role"), tabIndex: badge.tabIndex, text: badge.innerText })),
        };
      })()`);
      now[name] = { sizing, geometry };
      await save(`S-02-now-playing-${name}`);
    }
    const wide = now.wide.geometry, compact = now.compact.geometry;
    report["S-02"] = check(wide.cover.width >= 400 && wide.lyrics.x >= wide.cover.right &&
      compact.info.y >= compact.cover.bottom && Object.values(now).every(({ geometry: g }) =>
        g.lyrics.height > 0 && g.ambient.full && near(g.ambient.rect.width, g.screen[0]) &&
        near(g.ambient.rect.height, g.screen[1]) && g.progress === 0), now);
    await size(1280, 800);
    const badges = () => helper(`p.all('[data-testid="quality-badge"]').map((badge) => ({
      tag: badge.tagName, label: badge.getAttribute("aria-label"), role: badge.getAttribute("role"),
      tabIndex: badge.tabIndex, text: badge.innerText.trim(), shown: p.visible(badge),
      frame: getComputedStyle(badge).boxShadow,
    }))`);
    await click(tid("btn-next"));
    await until('__phase2.all(\'[data-testid="quality-badge"][data-quality="high"]\').length === 2');
    const knownBadges = await badges();
    await save("D-02-known-quality-fixture");
    await click(tid("btn-prev"));
    await until('!__phase2.id("quality-badge")');
    const restoredBadges = await badges();
    await go("settings");
    await helper('p.id("settings-open-diagnostics").scrollIntoView({ block: "center" }), true');
    const trigger = await helper(`(() => {
      const button = p.id("settings-open-diagnostics");
      return { advanced: !!button.closest('[data-testid="settings-advanced"]'),
        inBar: /diagnostic/i.test(p.id("player-bar").innerText) || !!p.q('[data-testid*="diagnostic"]', p.id("player-bar")) };
    })()`);
    await save("D-05-advanced");
    await click(tid("settings-open-diagnostics"));
    await until('__phase2.id("diagnostics-content")');
    await sleep(380);
    const diagnostic = await helper('Object.fromEntries(p.all("dt", p.id("diagnostics-content")).map((dt) => [dt.textContent.trim(), dt.nextElementSibling.textContent.trim()]))');
    report["D-02"] = check(wide.rawChips.length === 0 && wide.badges.length === 0 && restoredBadges.length === 0 &&
      knownBadges.length === 2 && knownBadges.every((badge) => badge.shown && badge.tag === "SPAN" &&
        badge.role === "img" && badge.tabIndex === -1 && badge.text === "HIGH" &&
        badge.frame.includes("inset")),
    { rawChips: wide.rawChips, badgesForUnknownQuality: wide.badges, knownBadges, restoredBadges, diagnostics: diagnostic });
    report["D-04"] = check(!wide.internalState && !compact.internalState &&
      Object.entries(diagnostic).some(([key, value]) => /playback state|player page state/i.test(key) && value),
    { internalState: wide.internalState, diagnostic });
    report["D-05"] = check(!trigger.inBar && trigger.advanced && Object.keys(diagnostic).length >= 10,
      { ...trigger, diagnostic });
    await save("D-04-D-05-diagnostics");
    await helper('p.id("dialog-close-btn").click(), true');
    await sleep(370);
  });

  await group("input-mark", async () => {
    await size(1280, 800);
    await go("home");
    const logoTrace = () => helper(`p.frames(() => {
      const mark = p.id("brand-mark");
      const ring = p.q(".brand-mark-ring", mark), star = p.q(".brand-mark-star", mark);
      const rm = p.transform(ring), sm = p.transform(star);
      return { ring: Math.atan2(rm.b, rm.a), star: Math.hypot(sm.a, sm.b) };
    }, 1450)`);
    const trace = logoTrace();
    await sleep(70);
    await click(tid("brand-mark"));
    await sleep(150);
    await click(tid("brand-mark"));
    await save("B-01-logo-mid");
    const logo = await trace;
    let rotation = 0, peakRotation = 0;
    for (let i = 1; i < logo.length; i++) {
      const delta = (logo[i].ring - logo[i - 1].ring + 3 * Math.PI) % (2 * Math.PI) - Math.PI;
      rotation += delta;
      peakRotation = Math.max(peakRotation, rotation);
    }
    report["B-01"] = check(near(rotation, 2 * Math.PI, 0.08) && peakRotation < 2.2 * Math.PI &&
      logo.some((frame) => frame.star > 1.25) &&
      near(logo.at(-1).star, 1, 0.01) && logo.slice(-7).every((frame) => near(frame.ring, 0, 0.01)),
    { rotation, peakRotation, logo, nativeIconSurfaces: "NOT_RUN — mock only" });

    const held = await press(tid("nav-search"));
    await sleep(180);
    const heldScale = await helper('p.transform(p.id("nav-search")).a');
    const springTrace = frames('{ scale: p.transform(p.id("nav-search")).a }', 420);
    await sleep(50);
    held();
    const spring = await springTrace;
    const overshoot = Math.max(...spring.map((frame) => frame.scale));
    const crossed = spring.findIndex((frame) => frame.scale > 1);
    report.navSpring = check(heldScale < 1 && overshoot > 1 &&
      spring.slice(crossed).every((frame) => frame.scale >= 0.999) &&
      near(spring.at(-1).scale, 1, 0.001), { heldScale, overshoot, spring });
  });

  await group("input-shelf", async () => {
    await go("home");

    const shelfMetrics = () => helper(`(() => {
      const shelf = p.id("cover-shelf");
      return { x: shelf.scrollLeft, max: shelf.scrollWidth - shelf.clientWidth, y: p.main().scrollTop,
        maxY: p.main().scrollHeight - p.main().clientHeight, cards: shelf.children.length,
        prev: p.id("cover-shelf-prev")?.disabled ?? null, next: p.id("cover-shelf-next")?.disabled ?? null };
    })()`);
    await size(900, 600);
    const original = await shelfMetrics();
    // The pinned mock feed contains just two cards. Repeat its cards using the common probe
    // helper; keep the real shelf, arrows, page, scroll handler and snapping in charge.
    if (original.max <= 1) await js("__glassProbe.lengthenShelf(4)");
    const shelfAt = async () => {
      const box = await js(rect(tid("cover-shelf")));
      const windowWidth = await js("innerWidth");
      return { x: Math.round(windowWidth * 0.65), y: Math.round(box.y + box.height * 0.45) };
    };
    const wheel = async (deltaY) => {
      const at = await shelfAt();
      pointer("mouseMove", at);
      pointer("mouseWheel", at, { deltaX: 0, deltaY, wheelTicksY: Math.sign(deltaY), canScroll: true });
      await sleep(540);
    };
    const initial = await shelfMetrics();
    await wheel(-135);
    const horizontal = await shelfMetrics();
    await helper('p.id("cover-shelf").scrollLeft = 100000, true');
    await sleep(300);
    const atEnd = await shelfMetrics();
    await wheel(-135);
    const pagePropagated = await shelfMetrics();
    await helper("p.main().scrollTop = 0, true");
    await sleep(240);
    await click(tid("cover-shelf-prev"));
    await sleep(520);
    const arrowBack = await shelfMetrics();
    await click(tid("cover-shelf-next"));
    await sleep(520);
    const arrowNext = await shelfMetrics();
    report["B-02"] = check(horizontal.x > initial.x && horizontal.y === initial.y &&
      near(atEnd.x, atEnd.max) && near(pagePropagated.x, atEnd.x) && pagePropagated.y > atEnd.y &&
      arrowBack.x < atEnd.x && arrowNext.x > arrowBack.x,
    { original, initial, horizontal, atEnd, pagePropagated, arrowBack, arrowNext });
    await save("B-02-shelf");
  });

  await group("input-focus", async () => {
    // Sample the rendered ring over the same pixels with the ring's ink made transparent.
    // Keep its halo, outline dimensions and all neighbouring controls in both captures.
    const focusSelectors = [":focus-visible", ".scrub:has(.scrub-input:focus-visible) .scrub-track", ".search-field:has(input:focus-visible)"];
    const hideCss = focusSelectors.map((selector) => `[data-probe-focus-clear] ${selector}`).join(", ")
      + " { outline-color: transparent !important; }";
    await js(`(document.head.append(Object.assign(document.createElement("style"), { textContent: ${JSON.stringify(hideCss)} })), true)`);
    const hideRing = (hide) => js(`(document.documentElement.toggleAttribute("data-probe-focus-clear", ${hide}),
      __phase2.frames(() => ({}), 24, 3))`);
    const runFocus = async (color) => {
      await theme(color);
      await go("search");
      await click(tid("search-input"));
      await sleep(120);
      const pointerFocus = await helper("p.focused()");
      // A real native Tab traversal, beginning at a clicked input and continuing past the
      // player controls, app chrome and sidebar. Track when the DOM order wraps.
      const visited = [];
      const seen = new Set();
      for (let count = 0; count < 32; count++) {
        key("Tab");
        await sleep(75);
        const state = await helper("p.focused()");
        if (seen.has(state.target)) break;
        if (!["BUTTON", "INPUT", "A", "SELECT"].includes(state.tag)) continue;
        seen.add(state.target);
        if (state.width > 0) state.contrast = await focusContrast(win, state, await js("innerWidth"), hideRing);
        visited.push(state);
        if (["btn-play-pause", "btn-mute", "nav-home", "search-input"].includes(state.target)) {
          await save(`S-03-${color}-${state.target}`);
        }
      }
      const dpr = await js("devicePixelRatio");
      const snapped = Math.floor(1.5 * dpr + 0.001) / dpr;
      return check(pointerFocus.source === "pointer" && pointerFocus.outline === "none" &&
        visited.length >= 8 && visited.every((state) => state.source === "keyboard" &&
        state.focusVisible && state.outline === "solid" && near(state.width, snapped, 0.1) &&
        state.contrast?.min !== null && state.contrast?.min >= 3),
      { dpr, expectedComputedOutline: snapped, pointerFocus, visited });
    };
    await size(1280, 800);
    report["S-03"] = { dark: await runFocus("dark"), light: await runFocus("light") };
    await theme("dark");
  });

  await group("ambient-scroll", async () => {
    await size(1280, 800);
    await theme("dark");
    const scrolls = {};
    for (const view of ["home", "liked", "recent", "now-playing", "queue", "library", "search", "settings", "playlist"]) {
      if (view === "playlist") {
        await helper('p.id("btn-load-10k-fixture").click(), true');
        await until('__phase2.id("view-playlist") && __phase2.id("virtual-track-row")');
      } else {
        await go(view);
      }
      await helper("p.main().scrollTop = 0, true");
      await until('__phase2.all(".ambient-layer").length === 1', 2800);
      await sleep(320);
      const top = await helper("p.ambient()");
      const max = await helper("p.main().scrollHeight - p.main().clientHeight");
      await helper("p.main().scrollTop = p.main().scrollHeight, true");
      await sleep(470);
      const end = await helper("p.ambient()");
      scrolls[view] = { top, end, max };
    }
    const artFade = async (name, action) => {
      const before = await helper("p.ambient()");
      const reading = frames("p.ambient()", 1300);
      await sleep(75);
      await action();
      await sleep(320);
      await save(`A-01-${name}-mid`);
      const trace = await reading;
      const last = trace.at(-1);
      const overlap = trace.filter((frame) => frame.layers.length >= 2 &&
        frame.layers[0].leaving && frame.layers[0].opacity > 0 &&
        frame.layers.at(-1).opacity > 0 && frame.layers.at(-1).opacity < 1);
      return {
        before, after: last, trace,
        pass: overlap.length > 0 && last.layers.length === 1 &&
          JSON.stringify(before.layers.at(-1).art) !== JSON.stringify(last.layers[0].art) &&
          trace.every((frame) =>
            JSON.stringify(frame.rect) === JSON.stringify(before.rect) &&
            frame.layers.some((layer) => layer.opacity > 0.05 && layer.art.some((image) => image.ready))),
      };
    };
    await go("home");
    await sleep(850);
    const pageFade = await artFade("page-change", () => helper('p.id("nav-now-playing").click(), true'));
    await sleep(250);
    const trackFade = await artFade("track-change", () => click(tid("btn-next")));
    report["A-01"] = check(
      Object.values(scrolls).every(({ top, end, max }) =>
        JSON.stringify(top.rect) === JSON.stringify(end.rect) &&
        JSON.stringify(top.layers.map((layer) => layer.box)) === JSON.stringify(end.layers.map((layer) => layer.box)) &&
        near(end.scroll, max, 2) &&
        (max < 2 || (top.scroll < max && end.opacity < top.opacity))),
      { scrolls, pageFade, trackFade },
    );
    if (!pageFade.pass || !trackFade.pass) report["A-01"].status = "FAIL";
  });

  await group("ambient-transitions", async () => {
    await size(1280, 800);
    mkdirSync(join(outDir, "frames"), { recursive: true });
    const a02 = {};
    const recordLayering = async (name, action, ms) => {
      const evidence = await record(name, async () => {
        const trace = frames("p.sidebar()", ms);
        await sleep(60);
        await action();
        return { samples: await trace };
      });
      const layerOrder = evidence.samples.every((frame) => {
        const [ambient, content, sidebar] = frame.stack.map(Number);
        return ambient < content && content < sidebar && frame.layerHit &&
          (frame.lens ? frame.backdrop.includes("url(") : frame.backdrop.includes("blur(")) &&
          frame.snapshots.every((name) => name.startsWith("main-scroll-region"));
      });
      const changed = {
        page: () => evidence.samples.some((sample) => sample.snapshots.length > 0),
        collapse: () => new Set(evidence.samples.map((sample) => sample.width)).size > 4,
        resize: () => new Set(evidence.samples.map((sample) => sample.viewport[0])).size > 2,
        theme: () => new Set(evidence.samples.map((sample) => sample.theme)).size > 1,
      }[name.split("-").at(-1)]();
      a02[name] = check(changed && layerOrder && evidence.samples.length >= 8 && evidence.frames.length >= 2,
        { ...evidence, changed, layerOrder });
      report["A-02"] = a02;
      flush();
    };
    for (const color of ["dark", "light"]) {
      await theme(color);
      await go("home");
      await sleep(720);
      await recordLayering(`${color}-page`, () => click(tid("nav-liked")), 1250);
      await recordLayering(`${color}-collapse`, () => click(sidebarToggleGlyph), 750);
      await recordLayering(`${color}-resize`, async () => {
        for (const width of [1240, 1190, 1140, 1090, 1040, 990, 950, 910, 870]) {
          win.setContentSize(width, 800);
          await sleep(45);
        }
      }, 1000);
      await size(1280, 800);
      await click(sidebarToggleGlyph);
      await sleep(470);
      await go("settings");
      await recordLayering(`${color}-theme`, () =>
        helper(`p.select("setting-theme-select", "${color === "dark" ? "light" : "dark"}"), true`), 800);
    }
  });

  await group("dock", async () => {
    await theme("dark");
    await go("home");
    // The panel keeps its full layout width during the glide, but its middle can be clipped.
    // Click where the visible icon is anchored so a real second click reaches the control.
    const sidebar = sidebarToggleGlyph;
    const toggle = (reduced) => helper(`(() => {
      const checkbox = p.id("setting-reduce-motion");
      if (checkbox.checked !== ${reduced}) checkbox.click();
      return checkbox.checked;
    })()`);
    await go("settings");
    await toggle(false);
    await go("home");
    await size(1280, 800);
    const normal = frames("p.sidebar()", 680);
    await sleep(55);
    await click(sidebar);
    const fullCollapse = await normal;
    await click(sidebar);
    await sleep(470);
    const reverse = frames("p.sidebar()", 740);
    await sleep(55);
    await click(sidebar);
    await sleep(125);
    await click(sidebar);
    const reversal = await reverse;
    const dpr = await js("devicePixelRatio");
    const stableIcons = (trace) => trace.every((frame) =>
      near(frame.glyphX, trace[0].glyphX, 1 / dpr) && near(frame.glyphY, trace[0].glyphY, 1 / dpr));
    const widths = fullCollapse.map((sample) => sample.width);
    const minWidth = Math.min(...widths), maxWidth = Math.max(...widths);
    const firstMotion = fullCollapse.find((sample) => sample.width < maxWidth - 0.01);
    const widthSettled = fullCollapse.find((sample) => near(sample.width, minWidth, 0.01));
    const normalDuration = firstMotion && widthSettled ? widthSettled.t - firstMotion.t : null;
    const holdBegins = fullCollapse.findIndex((sample) => sample.moving);
    const holdEnds = fullCollapse.findIndex((sample, i) => i > holdBegins && !sample.moving);
    const holdDuration = holdEnds > holdBegins && holdBegins >= 0
      ? fullCollapse[holdEnds].t - fullCollapse[holdBegins].t : null;
    await go("settings");
    await toggle(true);
    await go("home");
    const reduced = frames("p.sidebar()", 550);
    await sleep(55);
    await click(sidebar);
    const reducedMotion = await reduced;
    const intermediateWidth = reducedMotion.some((sample) => sample.width > minWidth + 1 && sample.width < maxWidth - 1);
    const intermediateText = reducedMotion.some((sample) => sample.textOpacity > 0 && sample.textOpacity < 1);
    await click(sidebar);
    await sleep(410);
    await go("settings");
    await toggle(false);
    await go("home");
    const reversingWidths = reversal.map((sample) => sample.width);
    const declaredDuration = fullCollapse[0].transitionDuration;
    report["B-03"] = check(declaredDuration === "0.32s" &&
      new Set(widths).size > 7 && normalDuration !== null && normalDuration <= 340 &&
      stableIcons(fullCollapse) && stableIcons(reversal) &&
      fullCollapse.some((frame) => frame.textOpacity > 0 && frame.textOpacity < 1) &&
      Math.min(...reversingWidths) > minWidth + 3 && near(reversingWidths.at(-1), maxWidth) &&
      !intermediateWidth && intermediateText && reducedMotion.every((sample) => sample.textTransform === "none"),
    { declaredDuration, normalDuration, holdDuration, dpr, fullCollapse, reversal, reducedMotion });

    const dock = {};
    for (const width of [1280, 1080, 1024, 960, 900]) {
      await size(width, 700);
      dock[width] = await read(dockGeometry);
    }
    await save("S-04-dock-900");
    const barPass = Object.values(dock).every((item) => near(item.bar.height, 80) &&
      near(item.cover.width, 56) && near(item.cover.height, 56) && near(item.seek.height, 20) &&
      item.buttons.every((button) => button.width >= 40 && button.height >= 40) &&
      item.buttons.some((button) => button.id === "btn-play-pause" && near(button.width, 44) && near(button.height, 44)));
    report["S-04"] = check(barPass, dock);
  });

  await group("dock-labels", async () => {
    await go("home");
    if (await helper('p.sidebar().collapsed')) {
      await click(sidebarToggleGlyph);
      await sleep(480);
    }
    await click(tid("btn-prev"));
    await sleep(150);
    const thresholdPx = 10;
    const narrow = {};
    async function measureWidth(width, quality) {
      await size(width, 700);
      const result = {};
      for (const state of ["collapsed", "expanded"]) {
        const before = await read(dockGeometry);
        const trace = frames(`(() => {
          const elapsed = p.box(p.q(".player-pos")), duration = p.box(p.q(".player-duration"));
          const track = p.box(p.q(".player-seek .scrub-track"));
          return { moving: p.q(".app-shell").hasAttribute("data-sidebar-motion"),
            elapsed: elapsed.x, duration: duration.x, track: [track.x, track.width] };
        })()`, 660);
        await sleep(55);
        await click(sidebarToggleGlyph);
        const samples = await trace;
        const after = await read(dockGeometry);
        await save(`narrow-dock-${quality}-${width}-${state}`);
        const release = samples.findIndex((frame, i) => i > 0 && !frame.moving && samples[i - 1].moving);
        const prev = samples[release - 1], next = samples[release];
        const releaseDelta = release < 0 ? null : {
          elapsed: next.elapsed - prev.elapsed, duration: next.duration - prev.duration,
          seekX: next.track[0] - prev.track[0], seekWidth: next.track[1] - prev.track[1],
        };
        result[state] = { before, after, samples, releaseDelta,
          abrupt: Object.fromEntries(["elapsed", "duration"].map((which) => [which,
            Math.max(...samples.slice(1).map((frame, i) => Math.abs(frame[which] - samples[i][which]))),
          ])) };
      }
      return result;
    }
    for (const width of [900, 960, 1024, 1080]) {
      (narrow.unknown ??= {})[width] = await measureWidth(width, "unknown");
    }
    await click(tid("btn-next"));
    await sleep(150);
    if (await helper('!!p.q(\'[data-testid="quality-badge"][data-quality="high"]\')')) {
      for (const width of [900, 960, 1024, 1080]) {
        (narrow.high ??= {})[width] = await measureWidth(width, "high");
      }
      await click(tid("btn-prev"));
    }
    report.narrowDockLabels = check(Object.values(narrow).every((widths) =>
      Object.values(widths).every((directions) => Object.values(directions).every(({ releaseDelta }) =>
        releaseDelta && Object.values(releaseDelta).every((px) => Math.abs(px) <= thresholdPx)))),
    { thresholdPx, widths: narrow });
  });

  flush();
  const failures = [];
  function collect(item, prefix) {
    if (item?.status === "FAIL" || item?.status === "BLOCKED") failures.push(prefix);
    else if (item && typeof item === "object" && !Array.isArray(item) && !item.status) {
      for (const [name, child] of Object.entries(item)) collect(child, `${prefix} ${name}`.trim());
    }
  }
  collect(report, "");
  if (report.errors || failures.length) {
    throw new Error(`Supplemental acceptance recorded ${failures.length} failures (${failures.join(", ")}) and ${Object.keys(report.errors ?? {}).length} probe errors; see ${reportPath}`);
  }
}
