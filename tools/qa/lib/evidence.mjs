// Gate verdicts computed from measured fields, plus the redaction pass applied to every
// evidence file before it is written.
import { mkdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { sha256Short } from "./sys.mjs";

export const STATUS = Object.freeze({
  PASS_TARGET: "PASS_TARGET",
  FAIL: "FAIL",
  BLOCKED: "BLOCKED",
  NOT_RUN: "NOT_RUN",
});

/**
 * One check. `pass` must be derived from `measured`; `blocked` / `notRun` are reasons
 * (strings) that override it. `support: true` marks supporting evidence that cannot
 * by itself decide the gate (e.g. process tests, unit tests).
 */
export function check(id, what, { measured, pass, blocked = null, notRun = null, support = false, note = null }) {
  let status;
  if (notRun) status = STATUS.NOT_RUN;
  else if (blocked) status = STATUS.BLOCKED;
  else status = pass === true ? STATUS.PASS_TARGET : STATUS.FAIL;
  return { id, what, status, support, measured, reason: notRun ?? blocked ?? null, note };
}

/** Gate verdict: any FAIL -> FAIL; else any BLOCKED -> BLOCKED; else any NOT_RUN -> NOT_RUN; else PASS_TARGET. */
export function verdict(checks) {
  const decisive = checks.filter((c) => !c.support);
  if (!decisive.length) return STATUS.NOT_RUN;
  if (decisive.some((c) => c.status === STATUS.FAIL)) return STATUS.FAIL;
  if (decisive.some((c) => c.status === STATUS.BLOCKED)) return STATUS.BLOCKED;
  if (decisive.some((c) => c.status === STATUS.NOT_RUN)) return STATUS.NOT_RUN;
  return STATUS.PASS_TARGET;
}

const HASH_KEYS = new Set(["accountScope", "accountEpoch", "accountHint", "credentialRevision", "datasyncId"]);
const DROP_KEYS = new Set(["channelLabel", "title", "author", "description", "subtitle", "cookie", "cookies", "headers", "mediaUrl", "src"]);
const SECRET_RE = /(SAPISID|__Secure-[0-9A-Za-z]+|\bHSID\b|\bSSID\b|\bAPISID\b|LOGIN_INFO|DATASYNC_ID|Cookie:\s|Authorization:\s|SIDCC)/i;
const HOME = homedir();

export function redact(value, key = null) {
  if (value == null) return value;
  if (Array.isArray(value)) return value.map((v) => redact(v, key));
  if (typeof value === "object") {
    const out = {};
    for (const [k, v] of Object.entries(value)) {
      if (DROP_KEYS.has(k)) {
        out[k] = v == null ? v : "[redacted]";
        continue;
      }
      out[k] = HASH_KEYS.has(k) && typeof v === "string" && !v.startsWith("h:") ? sha256Short(v) : redact(v, k);
    }
    return out;
  }
  if (typeof value === "string") {
    if (SECRET_RE.test(value)) return "[REDACTED:secret-pattern]";
    // library playlist ids can be private: keep only a hash in test ids
    const tid = /^(playlist-card|pick-playlist|home-playlist|search-playlist)-(.+)$/.exec(value);
    if (tid) return `${tid[1]}-${sha256Short(tid[2])}`;
    return value.split(HOME).join("~");
  }
  return value;
}

/** Scans text for secret-looking tokens; returns counts per pattern only (never the values). */
export function scanForSecrets(text) {
  const patterns = {
    sapisid: /SAPISID/g,
    secureCookie: /__Secure-[0-9A-Za-z]+PSID/g,
    cookieHeader: /Cookie:\s/gi,
    authorizationHeader: /Authorization:\s*SAPISIDHASH/gi,
    datasync: /DATASYNC_ID/g,
    loginInfo: /LOGIN_INFO/g,
  };
  const counts = {};
  for (const [name, re] of Object.entries(patterns)) counts[name] = (text.match(re) ?? []).length;
  return counts;
}

export function writeEvidence(path, doc) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, `${JSON.stringify(redact(doc), null, 2)}\n`);
  return path;
}

export function gateDoc({ gate, name, run, meta, checks, evidence = {} }) {
  return {
    gate,
    name,
    run,
    countsForM0: run === "FINAL",
    result: verdict(checks),
    generatedAt: new Date().toISOString(),
    meta,
    checks,
    evidence,
  };
}

export function evidencePath(root, run, gate) {
  return join(root, "artifacts/m0", run === "FINAL" ? "" : run.toLowerCase(), `${gate}.json`);
}
