import assert from "node:assert/strict";
import { test } from "node:test";
import { STATUS } from "../lib/evidence.mjs";
import { NULL_SINK } from "../lib/sys.mjs";
import { screenLockCheck } from "./g3.mjs";

const lockMs = Date.parse("2026-01-01T00:00:00Z");
const unlockMs = lockMs + 30_000;
const events = [
  { name: "screenLocked", at: new Date(lockMs).toISOString(), ok: true, lockedHint: "yes" },
  { name: "screenUnlocked", at: new Date(unlockMs).toISOString(), lockedHint: "no" },
];
const page = { t: 42, paused: false, readyState: 4 };
const sample = { phase: "locked", atMs: lockMs + 10_000, page, audio: [{ sink: NULL_SINK, corked: false }] };
const evaluate = (samples, options = {}, lockEvents = events) => screenLockCheck(
  { events: lockEvents, samples }, { lockScreen: true, ...options },
);

test("live check requires the page projection to preserve media presence", () => {
  const result = evaluate([{ ...sample, page: { ...page, present: true } }]);
  assert.equal(result.status, STATUS.PASS_TARGET);
  assert.equal(result.measured.lockedPlaybackSamples, 1);
  assert.equal(evaluate([sample]).status, STATUS.FAIL);
});

test("replay recognizes the old positive page projection from recorded media fields", () => {
  const result = evaluate([sample], { legacyProjection: true });
  assert.equal(result.status, STATUS.PASS_TARGET);
  assert.equal(result.measured.lockedPlaybackSamples, 1);
});

test("replay excludes samples without observed, unpaused media or an uncorked null-sink stream", () => {
  const result = evaluate([
    { ...sample, page: null },
    { ...sample, page: { ...page, present: false } },
    { ...sample, page: { ...page, t: null } },
    { ...sample, page: { ...page, readyState: undefined } },
    { ...sample, page: { ...page, paused: true } },
    { ...sample, audio: [{ sink: NULL_SINK, corked: true }] },
    { ...sample, audio: [{ sink: "another_sink", corked: false }] },
  ], { legacyProjection: true });
  assert.equal(result.status, STATUS.FAIL);
  assert.equal(result.measured.lockedPlaybackSamples, 0);
});

test("replay requires samples during a measured logind lock and unlock", () => {
  const options = { legacyProjection: true };
  assert.equal(evaluate([
    { ...sample, atMs: lockMs - 1 },
    { ...sample, atMs: unlockMs },
    { ...sample, phase: "post-lock" },
  ], options).status, STATUS.FAIL);
  assert.equal(evaluate([sample], options, events.slice(0, 1)).status, STATUS.FAIL);
  assert.equal(evaluate([sample], options, [{ ...events[0], lockedHint: "no" }, events[1]]).status, STATUS.FAIL);
});
