#!/usr/bin/env node
// Focused rerun against the same release build as the formal M0 evidence.
// YTMCP_ALLOW_LIVE=1 CARGO_TARGET_DIR=<formal M0 target dir> node tools/qa/recheck-g1-origin.mjs
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { AppInstance, DAEMON, ELECTRON, PROD_MAIN, REPO, waitAppReady } from "./lib/app.mjs";
import { check, gateDoc, writeEvidence } from "./lib/evidence.mjs";
import { ensureNullSink, fileSha256, NULL_SINK, unloadNullSink } from "./lib/sys.mjs";
import { collectEnvironment } from "./gates/g0.mjs";
import { nonMusicOriginCheck } from "./gates/g1.mjs";
import { quitViaMpris } from "./gates/g2.mjs";

if (process.env.YTMCP_ALLOW_LIVE !== "1") {
  console.error("Refusing live G1 origin recheck without YTMCP_ALLOW_LIVE=1");
  process.exit(2);
}

const M0_DIR = join(REPO, "artifacts", "m0");
const CHECK_IDS = ["G1.authDomainNoBridge", "G1.awayNotReady", "G1.bridgeReturns"];
const stamp = new Date().toISOString().replace(/[:.]/g, "-");
const output = join(M0_DIR, `g1-origin-recheck-${stamp}.json`);
const checks = [];
const evidence = {};
const meta = { harness: "tools/qa/recheck-g1-origin.mjs", nullSink: NULL_SINK };
let app;
let sink;
let step = "formal run provenance";

try {
  const g0Path = join(M0_DIR, "g0.json");
  const g1Path = join(M0_DIR, "g1.json");
  const g0 = JSON.parse(readFileSync(g0Path, "utf8"));
  const g1 = JSON.parse(readFileSync(g1Path, "utf8"));
  const originalChecks = g1.checks.filter(({ id }) => CHECK_IDS.includes(id)).map(({ id, status }) => ({ id, status }));
  const originalCommit = g0.meta?.git?.commit;
  meta.formalM0 = {
    commit: originalCommit,
    files: {
      "artifacts/m0/g0.json": fileSha256(g0Path),
      "artifacts/m0/g1.json": fileSha256(g1Path),
    },
    generatedAt: g1.generatedAt,
    g1Result: g1.result,
    originChecks: originalChecks,
  };
  const originalsMatch = g0.run === "FINAL" && g1.run === "FINAL" &&
    g0.result === "PASS_TARGET" && Boolean(originalCommit) && originalCommit === g1.meta?.git?.commit &&
    CHECK_IDS.every((id) => originalChecks.some((c) => c.id === id));
  checks.push(check("G1.recheck.formalProvenance", "original formal G0 and G1 identify one candidate and contain the origin checks", {
    pass: originalsMatch, blocked: originalsMatch ? null : "original formal G0 or G1 evidence is missing or mismatched",
  }));

  step = "candidate and device identity";
  const files = {
    "ytm-daemon": DAEMON,
    "apps/desktop/dist/main.mjs": PROD_MAIN,
    "apps/desktop/dist/ui-preload.cjs": join(REPO, "apps/desktop/dist/ui-preload.cjs"),
    "apps/desktop/dist/player-preload.cjs": join(REPO, "apps/desktop/dist/player-preload.cjs"),
    "bridge/dist/bridge-probe.mjs": join(REPO, "bridge/dist/bridge-probe.mjs"),
    "ui/dist/index.html": join(REPO, "ui/dist/index.html"),
  };
  const artifactHashes = Object.fromEntries(Object.entries(files).map(([name, path]) => [name, fileSha256(path)]));
  const formalHashes = g0.evidence?.build?.artifacts ?? {};
  const mismatchedArtifacts = Object.keys(files).filter((name) => !artifactHashes[name] || artifactHashes[name] !== formalHashes[name]);
  const environment = await collectEnvironment();
  const sameEnvironment = ["kernel", "kwin", "plasma", "gpu", "electronBinary"].every((key) => environment[key] && environment[key] === g0.meta?.[key]) &&
    environment.session.XDG_SESSION_TYPE === "wayland" && g0.meta?.session?.XDG_SESSION_TYPE === "wayland" &&
    Object.entries(environment.lockfiles).every(([name, hash]) => Boolean(hash) && hash === g0.meta?.lockfiles?.[name]);
  const runtimeSha256 = fileSha256(ELECTRON);
  meta.environment = environment;
  meta.build = { artifactHashes, runtimeSha256, probeSha256: fileSha256(join(REPO, "tools/qa/gates/g1.mjs")) };
  const compatible = !mismatchedArtifacts.length && sameEnvironment && Boolean(runtimeSha256);
  checks.push(check("G1.recheck.sameBuild", "production bundles, lockfiles, runtime version and target device match formal M0", {
    measured: { mismatchedArtifacts, sameEnvironment, runtimePresent: Boolean(runtimeSha256) },
    pass: compatible,
    blocked: compatible ? null : "select the formal M0 build with CARGO_TARGET_DIR and use the same target environment",
  }));

  if (originalsMatch && compatible) {
    step = "authenticated launch";
    sink = await ensureNullSink();
    const scratch = process.env.YTMCP_QA_WORKDIR ?? join(process.env.XDG_RUNTIME_DIR ?? "/tmp", "ytmcp-qa");
    app = await AppInstance.launch({ label: "g1-origin", workDir: join(scratch, `g1-origin-${stamp}`) });
    const ready = await waitAppReady(app, { timeoutMs: 90_000 });
    checks.push(check("G1.recheck.ready", "player starts authenticated with the app-owned session and its bridge ready", {
      measured: { ready: ready.ok, waitedMs: ready.waitedMs }, pass: ready.ok,
      blocked: ready.ok ? null : "authenticated player did not reach readiness",
    }));
    if (ready.ok) {
      step = "non-music origin checks";
      const result = await nonMusicOriginCheck(app);
      checks.push(...result.checks);
      evidence.nonMusicOrigin = result.measured;
    }
  }
} catch {
  checks.push(check("G1.recheck.harness", "focused recheck completed its active step", { measured: { step }, pass: false }));
} finally {
  if (app) {
    try {
      const quit = await quitViaMpris(app);
      checks.push(check("G1.recheck.quit", "quit exits cleanly without leaving app processes", {
        measured: { exited: quit.exited, leftoverPids: quit.leftoverPids.length },
        pass: quit.mprisQuitCall && quit.exited && quit.leftoverPids.length === 0,
      }));
    } catch {
      checks.push(check("G1.recheck.quit", "quit exits cleanly without leaving app processes", { pass: false }));
    } finally {
      if (!app.exit || app.pids().length) await app.kill();
    }
  }
  if (sink?.created) await unloadNullSink(sink.moduleId);
}

const doc = gateDoc({ gate: "G1", name: "Non-music origin isolation and recovery recheck", run: "RECHECK", meta, checks, evidence });
console.log(`G1 origin recheck ${doc.result} -> ${writeEvidence(output, doc)}`);
if (doc.result !== "PASS_TARGET") process.exitCode = 1;
