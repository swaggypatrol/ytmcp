import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { en } from "../../i18n/en.js";
import { zhCN } from "../../i18n/zh-CN.js";

const glassMock = vi.hoisted(() => ({ holdPlainBlur: vi.fn() }));
vi.mock("../../glass/liquid-glass.js", () => ({ glass: () => undefined, holdPlainBlur: glassMock.holdPlainBlur }));

import Sidebar from "../Sidebar.svelte";
import { SIDEBAR_EDGE_PROPERTY, SidebarMotion } from "../sidebar-motion.svelte.js";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});
beforeEach(() => glassMock.holdPlainBlur.mockClear());

/** A transitionend as the shell receives it, from `target` for `propertyName`. */
function transitionEnd(target: Element, propertyName: string): TransitionEvent {
  const event = new Event("transitionend", { bubbles: true });
  Object.defineProperty(event, "propertyName", { value: propertyName });
  Object.defineProperty(event, "target", { value: target });
  return event as TransitionEvent;
}

function panel(): HTMLElement {
  const el = document.createElement("aside");
  el.className = "app-sidebar";
  return el;
}

describe("Sidebar collapse transition", () => {
  it("holds the plain blur once for a move, turns back on a second click mid-way and gives the lens back when the panel's edge arrives", () => {
    const motion = new SidebarMotion();
    motion.toggle(false);
    expect(motion.collapsed).toBe(true);
    expect(motion.moving).toBe(true);
    expect(glassMock.holdPlainBlur.mock.calls).toEqual([[true]]);

    // Reversing mid-way flips the target without dropping or re-taking the hold.
    motion.toggle(false);
    expect(motion.collapsed).toBe(false);
    expect(motion.moving).toBe(true);
    expect(glassMock.holdPlainBlur.mock.calls).toEqual([[true]]);

    // Only the sidebar's own edge ends the move: labels fading or the dock arriving do not.
    motion.onTransitionEnd(transitionEnd(panel(), "opacity"));
    motion.onTransitionEnd(transitionEnd(document.createElement("div"), SIDEBAR_EDGE_PROPERTY));
    const frame = document.createElement("div");
    frame.className = "sidebar-frame";
    motion.onTransitionEnd(transitionEnd(frame, SIDEBAR_EDGE_PROPERTY));
    expect(motion.moving).toBe(true);
    motion.onTransitionEnd(transitionEnd(panel(), SIDEBAR_EDGE_PROPERTY));
    expect(motion.moving).toBe(false);
    expect(glassMock.holdPlainBlur.mock.calls).toEqual([[true], [false]]);
  });

  it("gives the lens back within 120 ms of the move's end when no transitionend arrives", () => {
    vi.useFakeTimers();
    const motion = new SidebarMotion();
    motion.toggle(false);
    vi.advanceTimersByTime(320 + 119);
    expect(motion.moving).toBe(true);
    vi.advanceTimersByTime(1);
    expect(motion.moving).toBe(false);
    expect(glassMock.holdPlainBlur).toHaveBeenLastCalledWith(false);
    motion.dispose();
  });

  it("with reduced motion only cross-fades: labels leave before a collapse, a second click brings them back, and the lens is never swapped", () => {
    vi.useFakeTimers();
    const motion = new SidebarMotion();
    motion.toggle(true);
    expect(motion.textHidden).toBe(true);
    expect(motion.collapsed).toBe(false);
    vi.advanceTimersByTime(90);
    expect(motion.collapsed).toBe(true);

    motion.toggle(true);
    expect(motion.collapsed).toBe(false);
    expect(motion.textHidden).toBe(false);

    motion.toggle(true);
    motion.toggle(true);
    vi.advanceTimersByTime(500);
    expect(motion.collapsed).toBe(false);
    expect(motion.textHidden).toBe(false);
    expect(motion.moving).toBe(false);
    expect(glassMock.holdPlainBlur).not.toHaveBeenCalled();
  });
});

function renderSidebar(extra: Record<string, unknown> = {}) {
  const onNavigate = vi.fn();
  const onToggleCollapse = vi.fn();
  const result = render(Sidebar, {
    props: { activeView: "home", collapsed: false, mockMode: false, onNavigate, onToggleCollapse, ...extra },
  });
  return { ...result, onNavigate, onToggleCollapse };
}

describe("Sidebar layout", () => {
  it("puts the collapse control at the foot of the nav column, above Settings, as a nav item", async () => {
    const { onToggleCollapse } = renderSidebar();
    const toggle = screen.getByTestId("sidebar-collapse-btn");
    expect(toggle.classList.contains("nav-item")).toBe(true);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(toggle.getAttribute("aria-label")).toBe("Collapse sidebar");
    const following = (a: Element, b: Element) => Boolean(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);
    expect(following(screen.getByTestId("nav-agent-activity"), toggle)).toBe(true);
    expect(following(toggle, screen.getByTestId("nav-settings"))).toBe(true);
    await fireEvent.click(toggle);
    expect(onToggleCollapse).toHaveBeenCalledTimes(1);
  });

  it("names every collapsed item and gives it a tooltip, and hides the labels", () => {
    const { container } = renderSidebar({ collapsed: true });
    const toggle = screen.getByTestId("sidebar-collapse-btn");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(toggle.getAttribute("aria-label")).toBe("Expand sidebar");
    expect(toggle.getAttribute("title")).toBe("Expand sidebar");
    for (const id of ["home", "search", "liked", "lyrics", "settings"]) {
      const item = screen.getByTestId(`nav-${id}`);
      expect(item.getAttribute("aria-label")).toBeTruthy();
      expect(item.getAttribute("title")).toBe(item.getAttribute("aria-label"));
    }
    expect(container.querySelector(".app-sidebar")!.classList.contains("is-text-hidden")).toBe(true);
  });

  it("draws hairlines only between the nav groups: none above Collapse at the foot (Sidebar artboard)", () => {
    const { container } = renderSidebar();
    expect(container.querySelectorAll(".sidebar-nav .nav-divider")).toHaveLength(3);
    expect(container.querySelector(".sidebar-foot .nav-divider")).toBeNull();
  });

  it("hides the labels while they leave ahead of a collapse, before the panel is collapsed", () => {
    const { container } = renderSidebar({ collapsed: false, textHidden: true });
    expect(container.querySelector(".app-sidebar")!.classList.contains("is-text-hidden")).toBe(true);
    expect(screen.getByTestId("nav-home").getAttribute("aria-label")).toBeNull();
  });
});

describe("Sidebar glide", () => {
  const css = readFileSync(resolve(import.meta.dirname, "../../../styles/sidebar.css"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");

  it("puts the glass panel directly inside a plain frame, so the frame's sliding clip never cuts the panel's backdrop off", () => {
    const { container } = renderSidebar();
    const panel = container.querySelector(".app-sidebar")!;
    expect(panel.classList.contains("lg")).toBe(true);
    const frame = panel.parentElement!;
    expect(frame.classList.contains("sidebar-frame")).toBe(true);
    expect(frame.classList.contains("lg")).toBe(false);
  });

  it("moves every box that follows the edge by transform only: the edge never feeds a width, a left or a margin", () => {
    const uses = [...css.matchAll(/^\s*([\w-]+)\s*:[^;{}]*var\(--sidebar-edge\)/gm)].map((m) => m[1]);
    expect(uses.length).toBeGreaterThan(0);
    expect(new Set(uses)).toEqual(new Set(["transform"]));
    // No ancestor of a glass surface may become a backdrop root while it moves.
    expect(css).not.toMatch(/clip-path|(^|[^-])filter\s*:|mask\s*:/m);
  });

  it("anchors every icon and the mark 36 px from the panel's left edge, the middle of the collapsed panel, so no icon moves when it collapses", () => {
    const base = readFileSync(resolve(import.meta.dirname, "../../../styles/base.css"), "utf8");
    const token = (name: string) => {
      const match = new RegExp(`${name}:\\s*([\\d.]+)px`).exec(base);
      if (!match) throw new Error(`no ${name} in base.css`);
      return Number(match[1]);
    };
    // One declaration of the rule with exactly this selector, in px (a var() resolved from base.css).
    const decl = (selector: string, property: string) => {
      const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      const body = new RegExp(`(?:^|})\\s*${escaped}\\s*{([^}]*)}`, "m").exec(css)?.[1];
      const value = body && new RegExp(`(?:^|;)\\s*${property}\\s*:\\s*([^;]+)`).exec(body)?.[1]?.trim();
      if (!value) throw new Error(`no ${property} in ${selector}`);
      return value.split(/\s+/).map((part) => (part.startsWith("var(") ? token(part.slice(4, -1)) : parseFloat(part)));
    };
    const half = token("--sidebar-collapsed-width") / 2;
    const [panelPad] = decl(".app-sidebar", "padding");
    const itemStart = decl(".app-sidebar .sidebar-item", "padding")[3]!;
    const [icon] = decl(".sidebar-item > .icon,\n.sidebar-glyph", "width");
    const [headStart] = decl(".sidebar-head", "padding-left");
    const [mark] = decl(".sidebar-mark", "width");
    expect(half).toBe(36);
    expect(panelPad! + itemStart + icon! / 2).toBe(half);
    expect(panelPad! + headStart! + mark! / 2).toBe(half);
  });

  it("names the collapse control with the word it shows, in English and Chinese", () => {
    for (const dict of [en, zhCN]) expect(dict.app.collapseSidebar).toContain(dict.app.collapseShort);
  });
});

describe("Sidebar mark", () => {
  const ring = (container: HTMLElement) => container.querySelector<SVGElement>(".brand-mark-ring")!;
  const art = (container: HTMLElement) => container.querySelector<SVGElement>(".brand-mark-art")!;

  it("is a named button drawn as the app icon's parts, not a link", () => {
    const { container, onNavigate } = renderSidebar();
    const mark = screen.getByRole("button", { name: "YTMcp" });
    expect(mark.dataset.testid).toBe("brand-mark");
    for (const part of ["tile", "ring", "disc", "play", "star"]) expect(container.querySelector(`.brand-mark-${part}`)).not.toBeNull();
    void fireEvent.click(mark);
    expect(onNavigate).not.toHaveBeenCalled();
  });

  it("turns the ring once and twinkles the star once per click, and clicks during a turn do not stack", async () => {
    vi.useFakeTimers();
    const { container } = renderSidebar();
    const mark = screen.getByTestId("brand-mark");
    expect(ring(container).style.transform).toBe("rotate(0deg)");

    await fireEvent.click(mark);
    expect(ring(container).style.transform).toBe("rotate(360deg)");
    expect(art(container).classList.contains("is-twinkle")).toBe(true);
    await vi.advanceTimersByTimeAsync(200);
    expect(art(container).classList.contains("is-twinkle")).toBe(false);

    await fireEvent.click(mark);
    expect(ring(container).style.transform).toBe("rotate(360deg)");
    expect(art(container).classList.contains("is-twinkle")).toBe(false);

    await vi.advanceTimersByTimeAsync(700);
    await fireEvent.click(mark);
    expect(ring(container).style.transform).toBe("rotate(720deg)");
  });

  it("with reduced motion only twinkles the star", async () => {
    vi.useFakeTimers();
    const { container } = renderSidebar({ reducedMotion: true });
    await fireEvent.click(screen.getByTestId("brand-mark"));
    expect(ring(container).style.transform).toBe("rotate(0deg)");
    expect(art(container).classList.contains("is-twinkle")).toBe(true);
    await vi.advanceTimersByTimeAsync(140);
    expect(art(container).classList.contains("is-twinkle")).toBe(false);
  });
});
