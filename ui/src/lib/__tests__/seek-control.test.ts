import type { CommandReceipt } from "@ytmcp/protocol";
import { describe, expect, it, vi } from "vitest";
import { SeekControl } from "../seek-control.svelte.js";

const accepted = (commandId: string): CommandReceipt => ({
  commandId,
  op: "seek",
  status: "accepted",
  acceptedAtMs: 0,
  observedAtMs: null,
  deduplicated: false,
  error: null,
});

describe("SeekControl", () => {
  it("never seeks the next track when the track changes during a drag", async () => {
    const control = new SeekControl();
    const seek = vi.fn(async () => accepted("s1"));
    control.drag(30_000, 200_000, "track-a");
    expect(control.preview?.positionMs).toBe(30_000);
    control.reconcile("track-b", null);
    control.drag(40_000, 200_000, "track-b");
    expect(control.preview).toBeNull();
    await control.commit(40_000, 200_000, "track-b", seek);
    expect(seek).not.toHaveBeenCalled();
    expect(control.preview).toBeNull();
  });

  it("holds the released position until the seek settles and drops a drag that sent nothing", async () => {
    vi.useFakeTimers();
    try {
      const control = new SeekControl();
      await control.commit(50_000, 200_000, "track-a", async () => accepted("s1"));
      expect(control.preview).toMatchObject({ positionMs: 50_000, commandId: "s1" });
      control.reconcile("track-a", { ...accepted("s1"), status: "observed" });
      expect(control.preview).toBeNull();

      control.drag(70_000, 200_000, "track-a");
      control.release();
      vi.runAllTimers();
      expect(control.preview).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});
