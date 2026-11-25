import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import {
  FakeCoreClient,
  type AgentActivitySnapshot,
  type AppSnapshot,
  type CommandReceipt,
  type LyricsResult,
  type MutationResult,
  type PlayerCommandPayload,
  type StateDelta,
} from "@ytmcp/protocol";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { tick } from "svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "../App.svelte";
import AmbientLayer from "../../lib/components/AmbientLayer.svelte";
import ToastHost from "../../lib/components/ToastHost.svelte";
import { AmbientStore, ambientSources } from "../../lib/ambient/ambient.svelte.js";
import { TOAST_DWELL_MS, ToastStore, currentProblem, type Toast } from "../../lib/feedback/toasts.svelte.js";
import { artworkSrc, type Artwork } from "../../lib/cover.js";
import { CoreRequestError, IpcTimeoutError, createUiClient, type YtmcpUiBridge } from "../../lib/ipc/client.js";
import { en, type NavView } from "../../lib/i18n/en.js";
import { zhCN } from "../../lib/i18n/zh-CN.js";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const NO_LYRICS: LyricsResult = {
  videoId: "mock-vid-00001",
  kind: "disabled",
  source: "disabled",
  language: null,
  matchBasis: null,
  fetchedAtMs: 0,
  fileOffsetMs: 0,
  userOffsetMs: 0,
  syncedLines: [],
  plainText: null,
  error: null,
};

/** A non-mock preload bridge over the synthetic fake core. */
function preload(options: { snapshot?: (s: AppSnapshot) => AppSnapshot; sendMutation?: YtmcpUiBridge["sendMutation"] } = {}) {
  const fake = new FakeCoreClient("normal");
  const base: AppSnapshot = { ...fake.subscribeSnapshot(() => {}).snapshot, mockMode: false };
  const snapshot = options.snapshot ? options.snapshot(base) : base;
  let listener: ((delta: StateDelta) => void) | null = null;
  const bridge = {
    subscribeSnapshot: (l: (delta: StateDelta) => void) => {
      listener = l;
      return { snapshot: structuredClone(snapshot), unsubscribe: () => (listener = null) };
    },
    sendPlayerCommand: vi.fn((cmd: PlayerCommandPayload) => fake.sendPlayerCommand(cmd)),
    queryCatalog: vi.fn((req: Parameters<YtmcpUiBridge["queryCatalog"]>[0]) => fake.queryCatalog(req)),
    queryLyrics: () => NO_LYRICS,
    updateSettings: vi.fn(),
    momentSettings: vi.fn((req: Parameters<FakeCoreClient["momentSettings"]>[0]) => fake.momentSettings(req)),
    openPlayerWindow: vi.fn(),
    clearCache: vi.fn(),
    sendMutation: options.sendMutation,
    logout: vi.fn(),
  } satisfies YtmcpUiBridge;
  return { bridge, snapshot, emit: (d: StateDelta) => listener?.(d) };
}

const renderPreload = (bridge: YtmcpUiBridge, initialView: NavView = "home") =>
  render(App, { props: { client: createUiClient({ preloadBridge: bridge }), initialView } });
const renderMock = (initialView: NavView = "home") =>
  render(App, { props: { client: createUiClient({ fakeClient: new FakeCoreClient("normal") }), initialView } });

const art = (seed: string): Artwork => ({ cover: null, seed, label: seed.toUpperCase() });
const mockSrc = (seed: string) => artworkSrc(art(seed), true)!;

describe("Ambient light (L1)", () => {
  it("is one fixed layer before and outside the scroll container; pages report art instead of painting it", async () => {
    const { container } = renderMock("home");
    await screen.findByTestId("view-home");
    const shell = container.querySelector(".app-shell")!;
    const light = screen.getByTestId("ambient-light");
    expect(shell.firstElementChild).toBe(light);
    expect(light.closest('[data-testid="main-scroll-region"]')).toBeNull();
    expect(light.getAttribute("aria-hidden")).toBe("true");
    expect(screen.getByTestId("main-scroll-region").querySelector(".hero-wash, img.hero-wash-art")).toBeNull();
    // Home reports its covers, so the light shows them rather than the theme's colours.
    await waitFor(() => expect(light.querySelectorAll(".ambient-layer:not([data-leaving]) img").length).toBeGreaterThan(0));
  });

  it("follows the page's cover, then the playing track's, then the theme's colours, skipping broken covers", () => {
    const store = new AmbientStore();
    const playing = art("playing");
    expect(ambientSources(store.current, playing, true, new Set())).toEqual([mockSrc("playing")]);
    expect(ambientSources(store.current, null, true, new Set())).toEqual([]);

    const page = {};
    store.report(page, { art: [art("a"), art("b"), art("c"), art("d")], mockMode: true, full: false });
    expect(ambientSources(store.current, playing, true, new Set())).toEqual([mockSrc("a"), mockSrc("b"), mockSrc("c")]);
    store.report(page, { art: [art("a"), art("a"), art("b"), art("c")], mockMode: true, full: false });
    expect(ambientSources(store.current, playing, true, new Set())).toEqual([mockSrc("a"), mockSrc("b"), mockSrc("c")]);
    // A page with no usable cover falls through to the playing track.
    const failed = new Set([mockSrc("a"), mockSrc("b"), mockSrc("c")]);
    expect(ambientSources(store.current, playing, true, failed)).toEqual([mockSrc("playing")]);

    // The new page may mount before the old one unmounts: the old withdrawal leaves the new report.
    const next = {};
    store.report(next, { art: [art("n")], mockMode: true, full: true });
    store.withdraw(page);
    expect(store.current?.full).toBe(true);
    expect(ambientSources(store.current, playing, true, new Set())).toEqual([mockSrc("n")]);
    store.withdraw(next);
    expect(store.current).toBeNull();
  });

  it("cross-fades a change in a new layer, drops the old one afterwards, and breathes only while allowed", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const store = new AmbientStore();
    const owner = {};
    store.report(owner, { art: [art("a")], mockMode: true, full: false });
    const props = { store, playing: null, mockMode: true, breathing: true, reducedMotion: false, scroller: null };
    const { rerender } = render(AmbientLayer, { props });
    const light = screen.getByTestId("ambient-light");
    const layers = () => Array.from(light.querySelectorAll<HTMLElement>(".ambient-layer"));
    expect(layers()).toHaveLength(1);
    expect(light.dataset.motion).toBe("breathing");

    // The same art reported again is not a change.
    store.report(owner, { art: [art("a")], mockMode: true, full: false });
    await vi.advanceTimersByTimeAsync(0);
    expect(layers()).toHaveLength(1);

    store.report(owner, { art: [art("b")], mockMode: true, full: true });
    await vi.advanceTimersByTimeAsync(0);
    expect(layers().map((l) => [l.dataset.leaving ?? null, l.classList.contains("is-full")])).toEqual([
      ["true", false],
      [null, true],
    ]);
    expect(light.dataset.full).toBe("true");
    await vi.advanceTimersByTimeAsync(1000);
    expect(layers()).toHaveLength(1);
    expect(layers()[0]!.querySelector("img")!.getAttribute("src")).toBe(mockSrc("b"));

    // No report and no track: the theme's colours (an empty art box).
    store.withdraw(owner);
    await vi.advanceTimersByTimeAsync(1000);
    expect(layers()).toHaveLength(1);
    expect(layers()[0]!.querySelector(".ambient-art")!.children).toHaveLength(0);

    await rerender({ ...props, breathing: false });
    expect(light.dataset.motion).toBe("still");
    await rerender({ ...props, breathing: false, reducedMotion: true });
    expect(light.dataset.motion).toBe("reduced");
  });

  it("dims with scroll distance from full strength to 0.5 / 0.85, without moving", async () => {
    // Runs the frame at once; returning 0 tells the layer no frame is still pending.
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      cb(0);
      return 0;
    });
    const scroller = document.createElement("div");
    document.body.append(scroller);
    render(AmbientLayer, {
      props: { store: new AmbientStore(), playing: null, mockMode: true, breathing: false, reducedMotion: false, scroller },
    });
    const light = screen.getByTestId("ambient-light");
    expect(light.style.getPropertyValue("--ambient-scroll")).toBe("1");
    scroller.scrollTop = 160;
    scroller.dispatchEvent(new Event("scroll"));
    expect(Number(light.style.getPropertyValue("--ambient-scroll"))).toBeCloseTo(1 - (1 - 0.5 / 0.85) / 2, 5);
    scroller.scrollTop = 5000;
    scroller.dispatchEvent(new Event("scroll"));
    expect(Number(light.style.getPropertyValue("--ambient-scroll"))).toBeCloseTo(0.5 / 0.85, 5);
    expect(light.getAttribute("style") ?? "").not.toMatch(/transform|top|left/);
    scroller.remove();
  });
});

describe("Toasts (L4)", () => {
  const toast = (kind: string, tone: Toast["tone"] = "neutral"): Omit<Toast, "id"> => ({ kind, tone, text: kind });

  it("keeps one transient toast for 3.2s, replaces it with a newer one and pauses while held", () => {
    vi.useFakeTimers();
    const store = new ToastStore();
    store.show(toast("first"));
    vi.advanceTimersByTime(TOAST_DWELL_MS - 100);
    store.show(toast("second"));
    expect(store.current?.kind).toBe("second");
    vi.advanceTimersByTime(TOAST_DWELL_MS - 100);
    expect(store.current?.kind).toBe("second");

    store.hold("hover");
    vi.advanceTimersByTime(60_000);
    expect(store.current?.kind).toBe("second");
    store.release("hover");
    vi.advanceTimersByTime(99);
    expect(store.current?.kind).toBe("second");
    vi.advanceTimersByTime(1);
    expect(store.current).toBeNull();
  });

  it("stays while focus is inside it, even after the pointer has passed over and left", () => {
    vi.useFakeTimers();
    const store = new ToastStore();
    store.show(toast("failed", "danger"));
    store.hold("focus");
    store.hold("hover");
    store.release("hover");
    vi.advanceTimersByTime(60_000);
    expect(store.current?.kind).toBe("failed");
    store.release("focus");
    vi.advanceTimersByTime(TOAST_DWELL_MS);
    expect(store.current).toBeNull();
  });

  it("keeps a toast with focus inside after the pointer passes over it and leaves", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    const store = new ToastStore();
    store.show({ ...toast("failed", "danger"), action: { label: "View diagnostics", run: () => {}, testId: "toast-act" } });
    render(ToastHost, { props: { store, sticky: null } });
    await vi.advanceTimersByTimeAsync(0);
    const host = screen.getByTestId("toast-failed");
    screen.getByTestId("toast-act").focus();
    await fireEvent.pointerEnter(host);
    await fireEvent.pointerLeave(host);
    await vi.advanceTimersByTimeAsync(TOAST_DWELL_MS * 2);
    expect(store.current?.kind).toBe("failed");
    expect(screen.getByTestId("toast-failed")).toBeTruthy();
  });

  it("shows the whole sentence and its detail in the tooltip", () => {
    const store = new ToastStore();
    store.show({ kind: "long", tone: "danger", text: "Couldn’t add a long title", detail: "TIMEOUT: player timed out" });
    render(ToastHost, { props: { store, sticky: null } });
    const text = screen.getByTestId("toast-long").querySelector(".toast-text")!;
    expect(text.getAttribute("title")).toBe("Couldn’t add a long title\nTIMEOUT: player timed out");
  });

  it("brings a sticky problem back after a transient toast without announcing it again; only failures are alerts", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    const store = new ToastStore();
    const sticky: Toast = { id: "problem-auth-expired", kind: "auth-expired", tone: "danger", text: "Your sign-in expired" };
    render(ToastHost, { props: { store, sticky } });
    expect(screen.getByTestId("toast-auth-expired").getAttribute("role")).toBe("status");

    store.show(toast("write-done", "success"));
    await vi.advanceTimersByTimeAsync(0);
    expect(screen.getByTestId("toast-write-done").getAttribute("role")).toBe("status");
    expect(screen.queryByTestId("toast-auth-expired")).toBeNull();
    // The leaving toast shares the grid cell for its exit, then is removed.
    expect(screen.getByTestId("toast-region").children).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(200);
    expect(screen.getByTestId("toast-region").children).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(TOAST_DWELL_MS);
    expect(screen.getByTestId("toast-auth-expired").hasAttribute("role")).toBe(false);

    store.show(toast("write-failed", "danger"));
    await vi.advanceTimersByTimeAsync(0);
    expect(screen.getByTestId("toast-write-failed").getAttribute("role")).toBe("alert");
  });

  it("picks the most fundamental of several account and connection problems", () => {
    const fake = new FakeCoreClient("normal");
    const snap = fake.subscribeSnapshot(() => {}).snapshot;
    const input = (s: AppSnapshot | null, mode: "preload" | "mock" | "unconnected" = "preload", connectionError: string | null = null) => ({
      mode,
      connectionError,
      snapshot: s,
    });
    const broken: AppSnapshot = {
      ...snap,
      networkState: "offline",
      authState: { ...snap.authState, state: "expired" },
      player: { ...snap.player, connection: "disconnected" },
    };
    expect(currentProblem(input(snap))).toBeNull();
    expect(currentProblem(input(broken, "unconnected"))).toBe("unconnected");
    expect(currentProblem(input(broken, "preload", "boom"))).toBe("connection-error");
    expect(currentProblem(input(broken))).toBe("offline");
    expect(currentProblem(input({ ...broken, networkState: "online" }))).toBe("auth-expired");
    expect(currentProblem(input({ ...broken, networkState: "online", authState: snap.authState }))).toBe("bridge-disconnected");
  });

  it("says nothing about a confirmed like: the heart is the confirmation and nothing is pushed", async () => {
    const sendMutation = vi.fn(async (): Promise<MutationResult> => ({ kind: "setLike", outcome: "confirmed", error: null }));
    const { bridge } = preload({
      snapshot: (s) => ({
        ...s,
        capabilities: { ...s.capabilities, likeMutation: "supported" },
        authState: { ...s.authState, state: "authenticated", playerCatalogMatched: true },
      }),
      sendMutation,
    });
    renderPreload(bridge, "now-playing");
    const like = await screen.findByTestId("np-like");
    const page = screen.getByTestId("main-scroll-region").querySelector(".app-page")!;
    const before = page.children.length;
    await fireEvent.click(like);
    await waitFor(() => expect(like.getAttribute("aria-pressed")).toBe("false"));
    expect(sendMutation).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("toast-region").children).toHaveLength(0);
    expect(page.children.length).toBe(before);
    expect(document.body.textContent).not.toContain("Done:");
  });

  it("does not report a command the core dropped for a newer one of the same kind", async () => {
    const { bridge } = preload();
    const client = createUiClient({ preloadBridge: bridge });
    render(App, { props: { client, initialView: "home" } });
    await screen.findByTestId("player-bar");
    const dropped = (cmd: PlayerCommandPayload): CommandReceipt => ({
      commandId: cmd.commandId, op: cmd.op, status: "cancelled", acceptedAtMs: 0, observedAtMs: null, deduplicated: false,
      error: { code: "STALE_CONTEXT", retriable: false, retryAfterMs: null, safeMessage: "coalesced by newer SetVolume command", requestId: null },
    });
    let settleFirst!: () => void;
    bridge.sendPlayerCommand
      .mockImplementationOnce((cmd) => new Promise<CommandReceipt>((resolve) => (settleFirst = () => resolve(dropped(cmd)))) as never)
      .mockImplementationOnce(() => new Promise<CommandReceipt>(() => {}) as never);
    const first = client.sendPlayerCommand("setVolume", { volume: 0.4 });
    void client.sendPlayerCommand("setVolume", { volume: 0.5 });
    settleFirst();
    await first;
    expect(client.getState().pendingOverlay.lastError).toBeNull();
    expect(client.getState().pendingOverlay.desiredVolume).toBe(0.5);
    expect(screen.queryByTestId("toast-command-failed")).toBeNull();

    // A cancellation with nothing of the same kind behind it is still reported.
    bridge.sendPlayerCommand.mockImplementationOnce((cmd) => dropped(cmd));
    await client.sendPlayerCommand("seek", { positionMs: 1000 });
    expect(client.getState().pendingOverlay.lastError?.code).toBe("STALE_CONTEXT");
  });

  it("reports a failed command once, even when the same receipt arrives again", async () => {
    const receipt: CommandReceipt = {
      commandId: "cmd-1",
      op: "play",
      status: "failed",
      acceptedAtMs: 0,
      observedAtMs: null,
      deduplicated: false,
      error: { code: "TIMEOUT", retriable: false, retryAfterMs: null, safeMessage: "player timed out", requestId: null },
    };
    const { bridge, emit, snapshot } = preload({ snapshot: (s) => ({ ...s, pending: { ...s.pending, lastReceipt: receipt } }) });
    renderPreload(bridge);
    const failure = await screen.findByTestId("toast-command-failed");
    expect(failure.getAttribute("role")).toBe("alert");
    expect(failure.textContent).toContain("Couldn’t start playing");
    expect(failure.textContent).not.toMatch(/Done:|TIMEOUT|player timed out/);
    const viewDiagnostics = screen.getByTestId("toast-view-diagnostics");
    viewDiagnostics.focus();
    await fireEvent.click(viewDiagnostics);
    await screen.findByTestId("diagnostics-content");
    await waitFor(() => expect(screen.queryByTestId("toast-command-failed")).toBeNull());
    await fireEvent.keyDown(screen.getByTestId("accessible-dialog"), { key: "Escape" });
    await waitFor(() => expect(screen.queryByTestId("accessible-dialog")).toBeNull());
    // The toast that opened the dialog is gone, so focus returns to the page, not to <body>.
    await waitFor(() => expect(document.activeElement?.tagName).toBe("H1"));
    emit({ deltaType: "commandUpdated", revision: snapshot.revision + 1, receipt });
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByTestId("toast-command-failed")).toBeNull();
  });

  it("says a failed command in plain words without the core's message, generic when the request is not known", async () => {
    const { bridge } = preload();
    bridge.sendPlayerCommand.mockRejectedValue(new Error("player bridge transport is not attached"));
    renderPreload(bridge, "now-playing");
    await fireEvent.click(await screen.findByTestId("btn-play-pause"));
    const failure = await screen.findByTestId("toast-command-failed");
    expect(failure.textContent).toContain("The player didn’t do that");
    expect(failure.textContent).not.toContain("transport");
  });
});

describe("Shell: diagnostics, dialogs and focus", () => {
  it("opens Diagnostics from Settings > Advanced, not from the player bar", async () => {
    renderMock("settings");
    const advanced = await screen.findByTestId("settings-advanced");
    expect(screen.queryByTestId("btn-open-diagnostics")).toBeNull();
    // Mock mode only: the scenario select sits in Advanced.
    expect(advanced.querySelector('[data-testid="mock-scenario-select"]')).not.toBeNull();
    await fireEvent.click(screen.getByTestId("settings-open-diagnostics"));
    expect((await screen.findByTestId("diagnostics-content")).textContent).toContain("Core epoch");
  });

  it("keeps the core's connection error reachable in Diagnostics while there is no snapshot", async () => {
    const { bridge } = preload();
    const failing: YtmcpUiBridge = { ...bridge, subscribeSnapshot: () => Promise.reject(new Error("sidecar crashed during boot")) };
    render(App, {
      props: { client: createUiClient({ preloadBridge: failing, connectRetryDelaysMs: [] }), initialView: "settings" },
    });
    expect((await screen.findByTestId("toast-connection-error")).textContent).not.toContain("sidecar");
    await fireEvent.click(screen.getByTestId("settings-open-diagnostics"));
    expect((await screen.findByTestId("diagnostics-content")).textContent).toContain("sidecar crashed during boot");
  });

  it("puts a dialog opened inside a page on the shell's top layer, never inside the scroll container", async () => {
    const { bridge } = preload();
    const { container } = renderPreload(bridge, "settings");
    await fireEvent.click(await screen.findByTestId("settings-logout"));
    const dialog = await screen.findByTestId("accessible-dialog");
    const layer = dialog.closest(".app-dialog-layer")!;
    expect(layer.parentElement).toBe(container.querySelector(".app-shell"));
    expect(layer.closest('[data-testid="main-scroll-region"]')).toBeNull();
    await fireEvent.click(screen.getByTestId("dialog-close-btn"));
    await waitFor(() => expect(container.querySelector(".app-dialog-layer")).toBeNull());
  });

  it("moves focus to the new page's title after a page change, but leaves focus kept in the player bar", async () => {
    renderMock("home");
    await screen.findByTestId("view-home");
    const nav = screen.getByTestId("nav-search");
    nav.focus();
    await fireEvent.click(nav);
    await screen.findByTestId("view-search");
    await waitFor(() => expect(document.activeElement?.tagName).toBe("H1"));
    expect(document.activeElement?.getAttribute("tabindex")).toBe("-1");
    expect(screen.getByTestId("view-search").contains(document.activeElement)).toBe(true);

    const lyrics = screen.getByTestId("btn-open-lyrics");
    lyrics.focus();
    await fireEvent.click(lyrics);
    await screen.findByTestId("view-lyrics");
    expect(document.activeElement).toBe(lyrics);
  });
});

describe("Ambient light contrast audit", () => {
  const css = (path: string) => readFileSync(resolve(import.meta.dirname, path), "utf8");
  const hero = css("../../styles/hero.css");
  const base = css("../../styles/base.css");
  /** The top-level block opened by `selector {`, up to its closing brace at the start of a line. */
  const block = (source: string, selector: string) => {
    const start = source.indexOf(`\n${selector} {`);
    expect(start).toBeGreaterThanOrEqual(0);
    return source.slice(start, source.indexOf("\n}", start));
  };
  const num = (source: string, pattern: RegExp) => Number(source.match(pattern)![1]);
  const lightness = (source: string, token: string) => num(source, new RegExp(`${token}:\\s*oklch\\(([\\d.]+)`));
  /** sRGB channel of a neutral grey with OKLab lightness L (its luminance is L cubed). */
  const encode = (lum: number) => (lum <= 0.0031308 ? 12.92 * lum : 1.055 * lum ** (1 / 2.4) - 0.055);
  const decode = (v: number) => (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
  const ratio = (a: number, b: number) => (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
  const peak = Math.max(...[...block(hero, "@keyframes ambient-breath").matchAll(/opacity:\s*([\d.]+)/g)].map((m) => Number(m[1])));

  /** Contrast of a text token on the light at the breath's peak, over the worst cover for that theme. */
  function onLight(theme: "dark" | "light", textToken: string) {
    const heroBlock = theme === "dark" ? block(hero, ":root") : block(hero, ':root[data-theme="light"]');
    const tokens = theme === "dark" ? block(base, ":root") : block(base, ':root[data-theme="light"]');
    const strength = num(heroBlock, /--ambient-strength:\s*([\d.]+)/);
    const c = num(heroBlock, /contrast\(([\d.]+)\)/);
    const b = num(heroBlock, /brightness\(([\d.]+)\)/);
    // Night: a white cover is the brightest light; day: a black one is the darkest.
    const cover = theme === "dark" ? 1 : 0;
    const art = Math.min(1, ((cover - 0.5) * c + 0.5) * b);
    const canvas = encode(lightness(tokens, "--bg-canvas") ** 3);
    const alpha = strength * peak;
    const bg = decode(alpha * art + (1 - alpha) * canvas);
    return ratio(lightness(tokens, textToken) ** 3, bg);
  }

  it("keeps secondary text at 4.5:1 on the brightest light, and page muted text uses the secondary tone", () => {
    expect(peak).toBeCloseTo(0.94, 5);
    expect(onLight("dark", "--text-secondary")).toBeGreaterThanOrEqual(4.5);
    expect(onLight("light", "--text-secondary")).toBeGreaterThanOrEqual(4.5);
    // Muted text alone would not hold 4.5:1 there, which is why pages lift it.
    expect(onLight("dark", "--text-muted")).toBeLessThan(4.5);
    expect(block(hero, ".app-page")).toMatch(/--text-muted:\s*var\(--text-secondary\)/);
  });
});

describe("Shell: feedback review fixes", () => {
  const openDiagnostics = async () => {
    await fireEvent.click(await screen.findByTestId("settings-open-diagnostics"));
    return screen.findByTestId("diagnostics-content");
  };
  const row = (content: HTMLElement, label: string) =>
    [...content.querySelectorAll("dl > div")].find((el) => el.querySelector("dt")?.textContent === label)?.querySelector("dd")
      ?.textContent;

  it.each([["en", en], ["zh-CN", zhCN]] as const)(
    "localizes page and lyrics errors in %s and keeps technical detail available",
    async (uiLanguage, copy) => {
      const { bridge } = preload({ snapshot: (s) => ({ ...s, settings: { ...s.settings, uiLanguage } }) });
      const error = { code: "TIMEOUT", safeMessage: "provider request timed out during read", retriable: true } as const;
      const detail = `TIMEOUT${copy.common.colon}${error.safeMessage}`;
      renderPreload({
        ...bridge,
        queryCatalog: () => ({ resultType: "error", ...error }),
        queryLyrics: () => ({ ...NO_LYRICS, kind: "unavailable", error }),
      });
      const page = await screen.findByTestId("home-state-error");
      expect(page.textContent).toContain(copy.states.requestTimeout);
      expect(page.textContent).not.toContain(error.safeMessage);
      expect(page.title).toBe(detail);

      await fireEvent.click(screen.getByTestId("btn-open-lyrics"));
      const lyrics = await screen.findByTestId("lyrics-status-empty");
      expect(lyrics.textContent).toContain(copy.states.requestTimeout);
      expect(lyrics.textContent).not.toContain(error.safeMessage);
      expect(lyrics.title).toBe(detail);

      await fireEvent.click(screen.getByTestId("nav-settings"));
      expect(row(await openDiagnostics(), copy.dialog.lastContentError)).toBe(detail);
    }
  );

  it.each([["en", en], ["zh-CN", zhCN]] as const)(
    "reports a Moment settings write failure in %s without showing the raw error",
    async (uiLanguage, copy) => {
      const { bridge } = preload({ snapshot: (s) => ({ ...s, settings: { ...s.settings, uiLanguage } }) });
      const error = { code: "STORAGE_ERROR", safeMessage: "fixture database is read-only", retriable: false } as const;
      renderPreload({
        ...bridge,
        momentSettings: (request) => {
          if (request.action === "update") throw new CoreRequestError(error);
          return bridge.momentSettings(request);
        },
      }, "settings");
      const toggle = await screen.findByTestId("moment-share-time") as HTMLInputElement;
      expect(toggle.checked).toBe(false);
      await fireEvent.click(toggle);
      const failure = await screen.findByTestId("moment-save-error");
      expect(toggle.checked).toBe(false);
      expect(failure.textContent?.trim()).toBe(copy.settings.moment.saveFailed);
      expect(failure.title).toBe(`STORAGE_ERROR${copy.common.colon}${error.safeMessage}`);
    }
  );

  it("keeps the playback state and the full audio facts in Diagnostics", async () => {
    const { bridge } = preload({
      snapshot: (s) => ({
        ...s,
        player: {
          ...s.player,
          playback: "paused",
          codec: "opus",
          audioItag: 251,
          audioMode: "AUDIO_MODE_STEREO",
          audioQuality: "high",
          audioBitrateKbps: 160,
          qualityLabel: "auto",
          drmProtected: false,
        },
      }),
    });
    renderPreload(bridge, "settings");
    const content = await openDiagnostics();
    expect(row(content, "Playback state")).toBe("Paused");
    expect(row(content, "Network")).toBe("Online");
    expect(row(content, "Audio codec (stream)")).toBe("opus (251)");
    expect(row(content, "Audio mode")).toBe("AUDIO_MODE_STEREO");
    expect(row(content, "Audio quality")).toBe("High");
    expect(row(content, "Audio bit rate")).toBe("160 kbps");
    expect(row(content, "Page quality label")).toBe("auto");
    expect(row(content, "DRM protected")).toBe("No");
  });

  it("reports a confirmed Play next once, only when the player confirms the command sent from here", async () => {
    const { bridge, emit, snapshot } = preload({
      snapshot: (s) => ({ ...s, capabilities: { ...s.capabilities, queueEnqueue: "supported" } }),
    });
    const accepted = (commandId: string): CommandReceipt => ({
      commandId,
      op: "enqueue",
      status: "accepted",
      acceptedAtMs: 0,
      observedAtMs: null,
      deduplicated: false,
      error: null,
    });
    bridge.sendPlayerCommand.mockImplementation((cmd: PlayerCommandPayload) => accepted(cmd.commandId));
    renderPreload(bridge, "now-playing");
    await fireEvent.click(await screen.findByTestId("np-actions"));
    await fireEvent.click(await screen.findByTestId("action-play-next"));
    await waitFor(() => expect(bridge.sendPlayerCommand).toHaveBeenCalledTimes(1));
    const sentId = bridge.sendPlayerCommand.mock.calls[0]![0].commandId;
    await new Promise((r) => setTimeout(r, 20));
    // Accepted is not done yet, and someone else's confirmed enqueue is not ours to report.
    expect(screen.queryByTestId("toast-enqueued")).toBeNull();
    emit({ deltaType: "commandUpdated", revision: snapshot.revision + 1, receipt: { ...accepted("other"), status: "observed" } });
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByTestId("toast-enqueued")).toBeNull();

    emit({ deltaType: "commandUpdated", revision: snapshot.revision + 2, receipt: { ...accepted(sentId), status: "observed" } });
    const toast = await screen.findByTestId("toast-enqueued");
    expect(toast.textContent).toContain("Added to play next");
    expect(toast.getAttribute("data-tone")).toBe("success");
  });

  it("shows a failed refresh as a toast with Retry that leaves when the page changes, not as a note in the page", async () => {
    const { bridge, emit, snapshot } = preload();
    const fake = new FakeCoreClient("normal");
    let fail = false;
    bridge.queryCatalog.mockImplementation((req) =>
      fail && req.kind === "libraryPlaylists" ? (Promise.reject(new IpcTimeoutError("ui.catalogQuery")) as never) : fake.queryCatalog(req)
    );
    renderPreload(bridge, "library");
    await screen.findByTestId("playlist-card-mock-pl-favorites");
    fail = true;
    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 1, resource_kind: "library" });
    const toast = await screen.findByTestId("toast-refresh-failed");
    expect(toast.closest('[data-testid="main-scroll-region"]')).toBeNull();
    expect(document.querySelector(".page-note")).toBeNull();

    fail = false;
    await fireEvent.click(screen.getByTestId("toast-retry"));
    await waitFor(() => expect(screen.queryByTestId("toast-refresh-failed")).toBeNull());

    fail = true;
    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 2, resource_kind: "library" });
    await screen.findByTestId("toast-refresh-failed");
    await fireEvent.click(screen.getByTestId("nav-home"));
    await screen.findByTestId("view-home");
    await waitFor(() => expect(screen.queryByTestId("toast-refresh-failed")).toBeNull());
  });

  it("says a failed library refresh seen through the playlist picker, and only while the picker is open", async () => {
    const { bridge, emit, snapshot } = preload();
    const fake = new FakeCoreClient("normal");
    let fail = false;
    bridge.queryCatalog.mockImplementation((req) =>
      fail && req.kind === "libraryPlaylists" ? (Promise.reject(new IpcTimeoutError("ui.catalogQuery")) as never) : fake.queryCatalog(req)
    );
    renderPreload(bridge, "now-playing");
    await fireEvent.click(await screen.findByTestId("np-actions"));
    await fireEvent.click(await screen.findByTestId("action-add-to-playlist"));
    await waitFor(() => expect(bridge.queryCatalog.mock.calls.some(([req]) => req.kind === "libraryPlaylists")).toBe(true));
    await new Promise((r) => setTimeout(r, 20));
    fail = true;
    emit({ deltaType: "resourceInvalidated", revision: snapshot.revision + 1, resource_kind: "library" });
    const toast = await screen.findByTestId("toast-refresh-failed");
    expect(toast.textContent).toContain("Retry");

    await fireEvent.keyDown(screen.getByTestId("track-actions-menu"), { key: "Escape" });
    await waitFor(() => expect(screen.queryByTestId("toast-refresh-failed")).toBeNull());
  });

  it("gives the offline toast a way to Diagnostics, which shows the network state", async () => {
    const { bridge } = preload({ snapshot: (s) => ({ ...s, networkState: "offline" }) });
    renderPreload(bridge, "home");
    const toast = await screen.findByTestId("toast-offline");
    await fireEvent.click(within(toast).getByTestId("toast-view-diagnostics"));
    const content = await screen.findByTestId("diagnostics-content");
    expect(row(content, "Network")).toBe("Offline");
  });

  it("keeps mock affordances in Settings > Advanced: no mock action in the toast, no test button on a playlist", async () => {
    render(App, { props: { client: createUiClient({ connectRetryDelaysMs: [] }), initialView: "settings" } });
    const toast = await screen.findByTestId("toast-unconnected");
    expect(within(toast).queryByTestId("btn-enable-mock")).toBeNull();
    expect(within(toast).getByTestId("toast-view-diagnostics")).toBeTruthy();
    const advanced = screen.getByTestId("settings-advanced");
    await fireEvent.click(within(advanced).getByTestId("btn-enable-mock"));
    await waitFor(() => expect(screen.queryByTestId("toast-unconnected")).toBeNull());
    cleanup();

    renderMock("settings");
    await fireEvent.click(await screen.findByTestId("btn-load-10k-fixture"));
    const list = await screen.findByTestId("view-playlist");
    // 10,000 rows of at least 40px each.
    await waitFor(() =>
      expect(parseFloat(list.querySelector<HTMLElement>('[data-testid="virtual-track-list"]')?.style.height ?? "0")).toBeGreaterThan(400_000)
    );
    expect(screen.queryByTestId("btn-load-10k-fixture")).toBeNull();
  });

  it("reports an AI DJ change to the queue once, and a DJ sign-in request until it is answered", async () => {
    const { bridge, emit, snapshot } = preload();
    type Op = AgentActivitySnapshot["operations"][number];
    const op = (phase: Op["phase"], resource = "queue", operationId = "op-1"): Op => ({
      principal: "claude-desktop",
      operationId,
      phase,
      resource,
      verifiedItems: 0,
      totalItems: 1,
      canCancel: false,
    });
    let operations: Op[] = [op("submitted")];
    const agentActivity = vi.fn(async () => ({ operations }));
    renderPreload({ ...bridge, agentActivity }, "home");
    await screen.findByTestId("view-home");
    let revision = snapshot.revision;
    const changed = async (operation_id = "op-1") => {
      const calls = agentActivity.mock.calls.length;
      emit({ deltaType: "controlOperationChanged", revision: ++revision, operation_id, principal: "claude-desktop" });
      await waitFor(() => expect(agentActivity.mock.calls.length).toBe(calls + 1));
      await new Promise((r) => setTimeout(r, 0));
    };
    // Nothing is read at connect, and a submitted operation has not settled.
    expect(agentActivity).not.toHaveBeenCalled();
    await changed();
    expect(screen.queryByTestId("toast-agent-changed")).toBeNull();

    operations = [op("submitted"), op("acknowledged", "accountHandoff", "op-2")];
    await changed("op-2");
    const handoff = await screen.findByTestId("toast-agent-handoff");
    expect(within(handoff).getByRole("button").textContent).toContain("Open player");
    operations = [op("submitted"), op("verified", "accountHandoff", "op-2")];
    await changed("op-2");
    await waitFor(() => expect(screen.queryByTestId("toast-agent-handoff")).toBeNull());
    // A settled sign-in request is not a queue change.
    expect(screen.queryByTestId("toast-agent-changed")).toBeNull();

    operations = [op("verified"), op("verified", "accountHandoff", "op-2")];
    await changed();
    const toast = await screen.findByTestId("toast-agent-changed");
    expect(toast.getAttribute("data-tone")).toBe("agent");
    expect(toast.textContent).toContain("The AI DJ changed the queue");
    expect(toast.textContent).not.toContain("claude-desktop");
    expect(toast.querySelector(".toast-text")!.getAttribute("title")).toContain("claude-desktop");
  });

  it("reports only the exact operation that settles during the first pending read", async () => {
    const { bridge, emit, snapshot } = preload();
    const operation = {
      operationId: "same-id", principal: "agent-a", phase: "verified", resource: "queue",
      verifiedItems: 1, totalItems: 1, canCancel: false,
    } as const;
    const result = { operations: [
      { ...operation, principal: "agent-b", resource: "interruption" },
      operation,
      { ...operation, operationId: "older", resource: "interruption" },
    ] };
    let complete!: (snapshot: AgentActivitySnapshot) => void;
    const firstRead = new Promise<AgentActivitySnapshot>((resolve) => { complete = resolve; });
    const agentActivity = vi.fn().mockReturnValueOnce(firstRead).mockResolvedValue(result);
    const client = createUiClient({ preloadBridge: { ...bridge, agentActivity } });
    render(App, { props: { client } });
    await waitFor(() => expect(client.getState().snapshot).not.toBeNull());
    expect(agentActivity).not.toHaveBeenCalled();
    let revision = snapshot.revision;
    emit({ deltaType: "manualTakeover", revision: ++revision, generation: 1 });
    await tick();
    expect(agentActivity).not.toHaveBeenCalled();
    const changed = () => emit({
      deltaType: "controlOperationChanged", revision: ++revision, operation_id: "same-id", principal: "agent-a",
    });
    changed();
    await waitFor(() => expect(agentActivity).toHaveBeenCalledTimes(1));
    changed();
    complete(result);
    const toast = await screen.findByTestId("toast-agent-changed");
    expect(toast.textContent).toContain(en.toast.agentChanged.queue);
    expect(toast.querySelector(".toast-text")?.getAttribute("title")).toBe(`${en.toast.agentChanged.queue}\nagent-a`);
    await waitFor(() => expect(agentActivity).toHaveBeenCalledTimes(2));
    expect(screen.getAllByTestId("toast-agent-changed")).toEqual([toast]);
    // Let the first toast expire so a wrongly queued duplicate could take its place.
    await waitFor(() => expect(screen.queryByTestId("toast-agent-changed")).toBeNull(), { timeout: TOAST_DWELL_MS + 1000 });
  });

  it("waits for visibility to look up a sign-in prompt but leaves hidden queue changes in DJ activity", async () => {
    const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(true);
    const { bridge, emit, snapshot } = preload();
    const agentActivity = vi.fn(async (): Promise<AgentActivitySnapshot> => ({ operations: [
      {
        operationId: "op-login", principal: "agent-a", phase: "acknowledged", resource: "accountHandoff",
        verifiedItems: 0, totalItems: 1, canCancel: false,
      },
      {
        operationId: "op-queue", principal: "agent-a", phase: "verified", resource: "queue",
        verifiedItems: 1, totalItems: 1, canCancel: false,
      },
    ] }));
    const client = createUiClient({ preloadBridge: { ...bridge, agentActivity } });
    render(App, { props: { client } });
    await waitFor(() => expect(client.getState().snapshot).not.toBeNull());
    emit({ deltaType: "controlOperationChanged", revision: snapshot.revision + 1, operation_id: "op-login", principal: "agent-a" });
    emit({ deltaType: "controlOperationChanged", revision: snapshot.revision + 2, operation_id: "op-queue", principal: "agent-a" });
    await tick();
    expect(agentActivity).not.toHaveBeenCalled();
    hidden.mockReturnValue(false);
    await fireEvent(document, new Event("visibilitychange"));
    const handoff = await screen.findByTestId("toast-agent-handoff");
    expect(agentActivity).toHaveBeenCalledTimes(1);
    expect(screen.queryByTestId("toast-agent-changed")).toBeNull();
    hidden.mockReturnValue(true);
    await fireEvent(document, new Event("visibilitychange"));
    hidden.mockReturnValue(false);
    await fireEvent(document, new Event("visibilitychange"));
    expect(screen.getByTestId("toast-agent-handoff")).toBe(handoff);
    expect(agentActivity).toHaveBeenCalledTimes(1);
  });

  it("ignores an in-flight settlement from the previous account", async () => {
    const { bridge, emit, snapshot } = preload();
    const oldOperation = {
      operationId: "old", principal: "agent-a", phase: "verified", resource: "queue",
      verifiedItems: 1, totalItems: 1, canCancel: false,
    } as const;
    let complete!: (result: AgentActivitySnapshot) => void;
    const oldRead = new Promise<AgentActivitySnapshot>((resolve) => { complete = resolve; });
    const agentActivity = vi.fn().mockReturnValueOnce(oldRead).mockResolvedValue({
      operations: [{ ...oldOperation, operationId: "new", principal: "agent-b", resource: "interruption" }],
    });
    const client = createUiClient({ preloadBridge: { ...bridge, agentActivity } });
    render(App, { props: { client } });
    await waitFor(() => expect(client.getState().snapshot).not.toBeNull());
    emit({ deltaType: "controlOperationChanged", revision: snapshot.revision + 1, operation_id: "old", principal: "agent-a" });
    await waitFor(() => expect(agentActivity).toHaveBeenCalledTimes(1));
    emit({
      deltaType: "authChanged", revision: snapshot.revision + 2,
      auth_state: { ...snapshot.authState, accountEpoch: "new-epoch", accountScope: "new-scope" },
    });
    await tick();
    complete({ operations: [oldOperation] });
    await tick();
    expect(screen.queryByTestId("toast-agent-changed")).toBeNull();
    emit({ deltaType: "controlOperationChanged", revision: snapshot.revision + 3, operation_id: "new", principal: "agent-b" });
    const toast = await screen.findByTestId("toast-agent-changed");
    expect(toast.textContent).toContain(en.toast.agentChanged.interruption);
    expect(toast.querySelector(".toast-text")?.getAttribute("title")).toContain("agent-b");
  });

  it("gives Settings the same page title as the other pages", async () => {
    renderMock("settings");
    const title = await screen.findByRole("heading", { level: 1, name: "Settings" });
    expect(title.classList.contains("browse-title")).toBe(true);
    expect(title.closest("header")?.classList.contains("browse-head")).toBe(true);
  });

  it("lets the ambient light breathe only while a track is playing, not while it buffers", async () => {
    const { bridge, emit, snapshot } = preload({ snapshot: (s) => ({ ...s, player: { ...s.player, playback: "buffering" } }) });
    renderPreload(bridge, "home");
    const light = await screen.findByTestId("ambient-light");
    await waitFor(() => expect(light.getAttribute("data-motion")).toBe("still"));
    emit({
      deltaType: "playerChanged",
      revision: snapshot.revision + 1,
      player: { ...snapshot.player, playback: "playing" },
      pending: snapshot.pending,
    });
    await waitFor(() => expect(light.getAttribute("data-motion")).toBe("breathing"));
  });

  it("states the wide column and the context rail in the design's own pixels, and keeps the scrollbar's room", () => {
    const css = (path: string) => readFileSync(resolve(import.meta.dirname, path), "utf8");
    const base = css("../../styles/base.css");
    expect(base).toMatch(/--page-max-wide:\s*1664px/);
    expect(base).toMatch(/--context-rail:\s*352px/);
    expect(base).not.toContain("--hero-wash-opacity");
    const shell = css("../../styles/shell.css");
    expect(shell).toMatch(/\.app-scroll\s*\{[^}]*scrollbar-gutter:\s*stable/);
    expect(shell).toMatch(/\.app-page:has\(\.np-stage\)\s*\{\s*--page-cap:\s*calc\(var\(--page-max-wide\) \+ var\(--space-48\) \+ var\(--context-rail\)\)/);
  });
});
