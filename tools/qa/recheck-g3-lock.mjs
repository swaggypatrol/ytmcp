#!/usr/bin/env node
// Replay the screen-lock check from saved formal M0 evidence without starting the app.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { screenLockCheck } from "./gates/g3.mjs";
import { check, gateDoc, STATUS, writeEvidence } from "./lib/evidence.mjs";
import { NULL_SINK } from "./lib/sys.mjs";

const dir = join(import.meta.dirname, "../../artifacts/m0");
const sourceBytes = readFileSync(join(dir, "g3.json"));
const source = JSON.parse(sourceBytes);
const originalCheck = source.checks?.find(({ id }) => id === "G3.screenLock");
const continuousAudio = source.checks?.find(({ id }) => id === "G3.soakAudioContinuous");
const soak = source.evidence?.soak;
const formalM0 = {
  file: "artifacts/m0/g3.json",
  sha256: createHash("sha256").update(sourceBytes).digest("hex"),
  commit: source.meta?.git?.commit ?? null,
  generatedAt: source.generatedAt,
  gateResult: source.result,
  lockStatus: originalCheck?.status ?? null,
  lockedPlaybackSamples: originalCheck?.measured?.lockedPlaybackSamples ?? null,
  continuousAudioStatus: continuousAudio?.status ?? null,
};

const canReplay = source.gate === "G3" && source.run === "FINAL" && source.countsForM0 === true
  && source.meta?.session?.XDG_SESSION_TYPE === "wayland" && source.meta?.nullSink === NULL_SINK
  && Boolean(formalM0.commit && originalCheck) && continuousAudio?.status === STATUS.PASS_TARGET
  && Array.isArray(soak?.samples) && Array.isArray(soak?.events);
const legacyProjection = formalM0.lockStatus === STATUS.FAIL && formalM0.lockedPlaybackSamples === 0;
const checks = [check("G3.recheck.formalProvenance", "saved formal Wayland run includes the lock observations and passed continuous audio check", {
  measured: formalM0,
  pass: canReplay,
})];
if (canReplay) checks.push(screenLockCheck(soak, { lockScreen: true, legacyProjection }));

const doc = gateDoc({
  gate: "G3",
  name: "Playback evidence during screen lock, recheck",
  run: "RECHECK",
  meta: { harness: "tools/qa/recheck-g3-lock.mjs", mode: "offline evidence replay", legacyProjection },
  checks,
});
const output = writeEvidence(join(dir, "g3-lock-recheck.json"), doc);
console.log(`G3 lock evidence replay ${doc.result} -> ${output}`);
if (doc.result !== STATUS.PASS_TARGET) process.exitCode = 1;
