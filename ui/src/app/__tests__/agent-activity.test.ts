import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import type { AgentActivityRequest, AgentActivitySnapshot } from "@ytmcp/protocol";
import { FakeCoreClient } from "@ytmcp/protocol";
import { tick } from "svelte";
import { afterEach, expect, it, vi } from "vitest";
import AgentActivity from "../../lib/components/AgentActivity.svelte";
import { createUiClient } from "../../lib/ipc/client.js";
import App from "../App.svelte";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function currentActivity(): AgentActivitySnapshot {
  return {
    operations: [
      { principal: "agent-a", operationId: "enqueue-1", phase: "unknown", resource: "queue", verifiedItems: 1, totalItems: 3, canCancel: false },
      { principal: "agent-b", operationId: "enqueue-1", phase: "submitted", resource: "queue", verifiedItems: 0, totalItems: 3, canCancel: true },
    ],
  };
}

function view(agentActivity: (request: AgentActivityRequest) => Promise<AgentActivitySnapshot>, visible = true) {
  return render(AgentActivity, { props: { client: { agentActivity }, scopeKey: "core:account-a", revision: 1, visible } });
}

it("scopes a cancellation to the selected operation", async () => {
  const snapshot = currentActivity();
  const agentActivity = vi.fn<(request: AgentActivityRequest) => Promise<AgentActivitySnapshot>>()
    .mockResolvedValueOnce(snapshot)
    .mockResolvedValueOnce({ ...snapshot, operations: [snapshot.operations[0]!] });
  view(agentActivity);

  expect(await screen.findByText("Outcome not yet confirmed")).toBeTruthy();
  expect(screen.getByText("Verified: 1 of 3")).toBeTruthy();
  const operations = screen.getByRole("region", { name: "Operations" });
  const cancel = within(operations).getByRole("button", { name: "Cancel the remaining steps of enqueue-1 from agent-b" });
  expect(within(operations).getAllByRole("button")).toHaveLength(1);
  await fireEvent.click(cancel);
  expect(agentActivity.mock.calls.map(([request]) => request)).toEqual([
    { action: "read" },
    { action: "cancel", principal: "agent-b", operationId: "enqueue-1" },
  ]);
  await waitFor(() => expect(within(operations).queryByRole("button")).toBeNull());
  expect(screen.getByText("Outcome not yet confirmed")).toBeTruthy();
});

it("coalesces visible revisions and discards a previous account's late response", async () => {
  let resolveOld: (snapshot: AgentActivitySnapshot) => void = () => {};
  const oldResponse = new Promise<AgentActivitySnapshot>((resolve) => { resolveOld = resolve; });
  const agentActivity = vi.fn<(request: AgentActivityRequest) => Promise<AgentActivitySnapshot>>()
    .mockReturnValueOnce(oldResponse)
    .mockResolvedValue({ operations: [] });
  const activity = view(agentActivity, false);

  await activity.rerender({ revision: 2 });
  expect(agentActivity).not.toHaveBeenCalled();
  await activity.rerender({ visible: true });
  await waitFor(() => expect(agentActivity).toHaveBeenCalledTimes(1));
  await activity.rerender({ scopeKey: "core:account-b", revision: 3 });
  await activity.rerender({ revision: 4 });
  expect(agentActivity).toHaveBeenCalledTimes(1);
  resolveOld(currentActivity());
  expect(await screen.findByText("No control operations right now.")).toBeTruthy();
  expect(screen.queryByText("Outcome not yet confirmed")).toBeNull();
  expect(agentActivity).toHaveBeenCalledTimes(2);

  await activity.rerender({ visible: false, revision: 5 });
  expect(agentActivity).toHaveBeenCalledTimes(2);
  await activity.rerender({ visible: true });
  await waitFor(() => expect(agentActivity).toHaveBeenCalledTimes(3));
});

it("keeps the last receipt visible and disables stale actions after a failed refresh", async () => {
  const snapshot = currentActivity();
  const agentActivity = vi.fn<(request: AgentActivityRequest) => Promise<AgentActivitySnapshot>>()
    .mockResolvedValueOnce(snapshot)
    .mockRejectedValueOnce(new Error())
    .mockResolvedValueOnce(snapshot);
  const activity = view(agentActivity);
  expect(await screen.findByText("Outcome not yet confirmed")).toBeTruthy();
  await activity.rerender({ revision: 2 });
  expect(await screen.findByText("Could not read DJ activity.")).toBeTruthy();
  expect(screen.getByText("Outcome not yet confirmed")).toBeTruthy();
  const cancel = screen.getByRole("button", { name: "Cancel the remaining steps of enqueue-1 from agent-b" });
  expect(cancel.hasAttribute("disabled")).toBe(true);
  await fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
  await waitFor(() => expect(cancel.hasAttribute("disabled")).toBe(false));
  expect(agentActivity.mock.calls.map(([request]) => request)).toEqual([
    { action: "read" }, { action: "read" }, { action: "read" },
  ]);
});

it("requires the activity route before requesting data and supports manual refresh", async () => {
  const client = createUiClient({ fakeClient: new FakeCoreClient("normal") });
  const agentActivity = vi.spyOn(client, "agentActivity").mockResolvedValue(currentActivity());
  render(App, { props: { client } });
  await waitFor(() => expect(client.getState().snapshot).not.toBeNull());
  expect(agentActivity).not.toHaveBeenCalled();
  const nav = screen.getByTestId("nav-agent-activity");
  await fireEvent.click(nav);
  expect(await screen.findByRole("heading", { name: "DJ activity" })).toBeTruthy();
  expect(nav.getAttribute("aria-current")).toBe("page");
  expect(await screen.findByText("Outcome not yet confirmed")).toBeTruthy();
  await fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
  await waitFor(() => expect(agentActivity).toHaveBeenCalledTimes(2));
  await fireEvent.click(screen.getByTestId("nav-home"));
  await tick();
  expect(screen.queryByTestId("view-agent-activity")).toBeNull();
  expect(agentActivity).toHaveBeenCalledTimes(2);
});

function withPreferences(): AgentActivitySnapshot {
  return {
    ...currentActivity(),
    preferences: [
      { preferenceId: "pref-artist", subjectKind: "artist", subjectId: "art-1", label: "Synthetic Artist", stance: "avoid", recordedBy: "claude-desktop" },
      { preferenceId: "pref-video", subjectKind: "video", subjectId: "vid-1", label: null, stance: "prefer", recordedBy: "agent-a" },
    ],
    preferencesTotal: 2,
  };
}

it("lists DJ preferences with their stance, and deletes one entry from the returned snapshot", async () => {
  const snapshot = withPreferences();
  const agentActivity = vi.fn<(request: AgentActivityRequest) => Promise<AgentActivitySnapshot>>()
    .mockResolvedValueOnce(snapshot)
    .mockResolvedValueOnce({ ...snapshot, preferences: [snapshot.preferences![1]!] });
  view(agentActivity);

  const section = await screen.findByRole("region", { name: "DJ preferences" });
  const rows = await within(section).findAllByTestId("agent-preference");
  expect(rows).toHaveLength(2);
  expect(within(rows[0]!).getByText("Synthetic Artist")).toBeTruthy();
  expect(within(rows[0]!).getByTestId("agent-preference-stance").textContent?.trim()).toBe("Avoid");
  expect(within(rows[0]!).getByText("Recorded by claude-desktop")).toBeTruthy();
  // A subject the player did not know when it was recorded is shown by its own ID.
  expect(within(rows[1]!).getByText("vid-1")).toBeTruthy();
  expect(within(section).queryByText("pref-video")).toBeNull();
  expect(within(section).queryByTestId("agent-preferences-truncated")).toBeNull();
  expect(within(rows[1]!).getByTestId("agent-preference-stance").textContent?.trim()).toBe("Prefer");

  await fireEvent.click(within(rows[0]!).getByRole("button", { name: "Delete preference “Synthetic Artist”" }));
  expect(agentActivity.mock.calls.map(([request]) => request)).toEqual([
    { action: "read" },
    { action: "clearDjPreference", preferenceId: "pref-artist" },
  ]);
  await waitFor(() => expect(within(section).getAllByTestId("agent-preference")).toHaveLength(1));
  expect(within(section).queryByText("Synthetic Artist")).toBeNull();
});

it("says when only the newest preferences of a longer list are shown", async () => {
  view(vi.fn(async () => ({ ...withPreferences(), preferencesTotal: 120 })));
  const section = await screen.findByRole("region", { name: "DJ preferences" });
  expect((await within(section).findByTestId("agent-preferences-truncated")).textContent?.trim())
    .toBe("Showing the latest 2 of 120.");
});

it("shows the empty preference state and reports a failed deletion through the activity error", async () => {
  const snapshot = withPreferences();
  const agentActivity = vi.fn<(request: AgentActivityRequest) => Promise<AgentActivitySnapshot>>()
    .mockResolvedValueOnce(snapshot)
    .mockRejectedValueOnce(Object.assign(new Error(), { code: "STALE_CONTEXT" }))
    .mockResolvedValueOnce({ ...snapshot, preferences: [] });
  view(agentActivity);
  await fireEvent.click(await screen.findByRole("button", { name: "Delete preference “Synthetic Artist”" }));
  expect(await screen.findByText("No DJ preferences recorded.")).toBeTruthy();
  expect(screen.getByText("The operation has changed. Check the refreshed list.")).toBeTruthy();
});

it("prompts for an acknowledged account handoff and opens the player only on click", async () => {
  const onOpenOfficialPlayer = vi.fn();
  const snapshot: AgentActivitySnapshot = {
    ...currentActivity(),
    operations: [
      { principal: "mcp-a", operationId: "handoff-1", phase: "acknowledged", resource: "accountHandoff", verifiedItems: 0, totalItems: 0, canCancel: false },
      { principal: "mcp-a", operationId: "handoff-0", phase: "verified", resource: "accountHandoff", verifiedItems: 0, totalItems: 0, canCancel: false },
      { principal: "mcp-a", operationId: "interrupt-1", phase: "acknowledged", resource: "interruption", verifiedItems: 0, totalItems: 0, canCancel: false },
      { principal: "mcp-a", operationId: "pref-1", phase: "verified", resource: "djPreference", verifiedItems: 0, totalItems: 0, canCancel: false },
    ],
  };
  render(AgentActivity, {
    props: { client: { agentActivity: vi.fn(async () => snapshot) }, scopeKey: "core:account-a", revision: 1, visible: true, onOpenOfficialPlayer },
  });
  const prompts = await screen.findAllByTestId("agent-account-handoff");
  expect(prompts).toHaveLength(1);
  expect(prompts[0]!.textContent).toContain("An AI agent is asking you to sign in or switch accounts in the player");
  const sources = screen.getAllByText((_, el) => el?.tagName === "P" && el.textContent?.startsWith("mcp-a") === true);
  expect(sources.map((el) => el.textContent?.replace(/\s+/g, "").trim())).toEqual([
    "mcp-a·Sign-inrequest", "mcp-a·Sign-inrequest", "mcp-a·Interruption", "mcp-a·DJpreference",
  ]);
  expect(onOpenOfficialPlayer).not.toHaveBeenCalled();
  await fireEvent.click(within(prompts[0]!).getByRole("button", { name: "Open the official player" }));
  expect(onOpenOfficialPlayer).toHaveBeenCalledTimes(1);
});

it("displays listening intents, operation summaries, preference notes, and paginates taste notes", async () => {
  const initialSnapshot: AgentActivitySnapshot = {
    operations: [
      {
        principal: "agent-a",
        operationId: "op-summary-1",
        phase: "verified",
        resource: "queue",
        action: "enqueue",
        verifiedItems: 2,
        totalItems: 2,
        canCancel: false,
      },
    ],
    listeningIntents: [
      {
        intentId: "intent-1",
        intentKey: "vocals",
        kind: "vocals",
        value: "preferInstrumental",
        basis: "ownerInput",
        scopeKind: "today",
        expiresAtMs: Date.now() + 60_000,
        reportedBy: "owner",
      },
    ],
    preferences: [
      {
        preferenceId: "pref-1",
        subjectKind: "artist",
        subjectId: "ar-1",
        label: "Artist One",
        stance: "prefer",
        note: "Great late-night instrumental vibe",
        recordedBy: "agent-a",
      },
    ],
    preferencesTotal: 25,
  };
  const agentActivity = vi.fn<(request: AgentActivityRequest) => Promise<AgentActivitySnapshot>>()
    .mockResolvedValueOnce(initialSnapshot)
    .mockResolvedValueOnce({ ...initialSnapshot, listeningIntents: [] })
    .mockResolvedValueOnce({
      ...initialSnapshot,
      listeningIntents: [],
      preferences: [
        ...initialSnapshot.preferences!,
        {
          preferenceId: "pref-2",
          subjectKind: "video",
          subjectId: "vid-2",
          label: "Second Track",
          stance: "avoid",
          note: "Too loud for focus",
          recordedBy: "agent-a",
        },
      ],
    });
  view(agentActivity);

  expect(await screen.findByText("Added to the queue")).toBeTruthy();
  expect(screen.getByText("Fewer vocals")).toBeTruthy();
  expect(screen.getByTestId("agent-intent-ends").textContent).toMatch(/^until \d/);
  expect(screen.getByTestId("agent-preference-note").textContent).toBe("Great late-night instrumental vibe");

  await fireEvent.click(screen.getByRole("button", { name: "Clear intent “Fewer vocals”" }));
  await waitFor(() => expect(screen.getByText("No active listening intents.")).toBeTruthy());

  await fireEvent.click(screen.getByTestId("agent-preferences-load-more"));
  await waitFor(() => expect(screen.getByText("Second Track")).toBeTruthy());
  expect(agentActivity.mock.calls.map(([req]) => req)).toEqual([
    { action: "read" },
    { action: "clearListeningIntent", intentKey: "vocals" },
    { action: "readWithOptions", preferencesLimit: 100 },
  ]);
});
