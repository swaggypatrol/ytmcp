#!/usr/bin/env node
// Launches the liquid-glass probe in the repo's pinned Electron with a throwaway profile.
//
//   node tools/probes/glass/run-probe.mjs behavior          Chromium behaviour matrix (synthetic page)
//   node tools/probes/glass/run-probe.mjs shots [quick|clean] real UI: screenshots and contrast, clean adds README views
//   node tools/probes/glass/run-probe.mjs acceptance [section ...] phase 2 layout, native input and frame recordings
//   node tools/probes/glass/run-probe.mjs perf [seconds]    real UI: frame pacing of automated scrolling (default 60 s per run)
//   node tools/probes/glass/run-probe.mjs perf collapse     real UI: only the sidebar collapse transition
//   node tools/probes/glass/run-probe.mjs look [dark|light] real UI: a few scenes and 2x crops, for visual iteration
//   node tools/probes/glass/run-probe.mjs flow              real UI: volume wheel, Ctrl+L return, playback indicator
//   node tools/probes/glass/run-probe.mjs tiers [dark|light] real UI: every main view at the four layout tiers
//   node tools/probes/glass/run-probe.mjs overlay           real UI: popover motion, frame intervals, input and press spring
//   node tools/probes/glass/run-probe.mjs rims              native corner crops in both themes at the selected DPR
//   node tools/probes/glass/run-probe.mjs transition        real UI: slowed page transitions captured mid-way
//
// shots, perf and look start the real application (apps/desktop/dist + ui/dist) with
// YTMCP_MODE=mock; build both first (`just dev-mock` shows how). They never contact
// music.youtube.com and never touch the real profile: every XDG home points into a scratch
// directory that is removed afterwards.
//
//   --evidence      write shots to artifacts/screenshots/glass, look to its look/ subdirectory,
//                   and perf to artifacts/perf/glass-real-ui.json
// Full shots fails for layout or contrast regressions after writing report.json. The quick
// variant keeps its shorter, advisory contrast sweep.
//   --scratch=DIR   parent directory for the scratch XDG homes (default: the OS temp dir)
//   other --flags   are passed to Electron (e.g. --force-device-scale-factor=0.5)
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";

const here = import.meta.dirname;
const repoRoot = resolve(here, "../../..");
const electronBin = resolve(repoRoot, "apps/desktop/node_modules/.bin/electron");
const mode = process.argv[2] ?? "shots";
const rest = process.argv.slice(3);
const evidence = rest.includes("--evidence");
const scratchParent = rest.find((a) => a.startsWith("--scratch="))?.slice("--scratch=".length) ?? tmpdir();
const electronFlags = rest.filter((a) => a.startsWith("--") && a !== "--evidence" && !a.startsWith("--scratch="));
const modeArgs = rest.filter((a) => !a.startsWith("--"));
const realUi = mode !== "behavior";

if (!existsSync(electronBin)) {
  console.error(`Electron binary missing at ${electronBin}`);
  process.exit(1);
}
for (const built of realUi ? ["apps/desktop/dist/main.mjs", "ui/dist/index.html"] : []) {
  if (!existsSync(resolve(repoRoot, built))) {
    console.error(`${built} is missing: build the UI and the desktop shell first`);
    process.exit(1);
  }
}

mkdirSync(scratchParent, { recursive: true });
const scratch = mkdtempSync(join(scratchParent, "ytmcp-probe-"));
const dirs = { config: join(scratch, "config"), data: join(scratch, "data"), cache: join(scratch, "cache"), state: join(scratch, "state") };
for (const d of Object.values(dirs)) mkdirSync(d, { recursive: true });

// Fonts are looked up through XDG dirs; link the user's font dir and fontconfig read-only so the
// probe renders with the same typefaces as the installed app.
const userFonts = join(homedir(), ".local/share/fonts");
const userFontconfig = join(homedir(), ".config/fontconfig");
if (existsSync(userFonts)) symlinkSync(userFonts, join(dirs.data, "fonts"));
if (existsSync(userFontconfig)) symlinkSync(userFontconfig, join(dirs.config, "fontconfig"));

const cpus = spawnSync(join(homedir(), ".local/bin/workload-cpus"), { encoding: "utf8" }).stdout?.trim();
// The real app derives its profile from XDG_DATA_HOME; the synthetic page gets an explicit one.
const electronArgs = ["--mute-audio", ...(realUi ? [] : [`--user-data-dir=${join(scratch, "profile")}`]), ...electronFlags, join(here, "electron-entry.mjs")];
const command = cpus ? "taskset" : electronBin;
const args = cpus ? ["-c", cpus, electronBin, ...electronArgs] : electronArgs;

const result = spawnSync(command, args, {
  cwd: here,
  stdio: "inherit",
  timeout: 20 * 60_000,
  env: {
    ...process.env,
    XDG_CONFIG_HOME: dirs.config,
    XDG_DATA_HOME: dirs.data,
    XDG_CACHE_HOME: dirs.cache,
    XDG_STATE_HOME: dirs.state,
    ...(realUi ? { YTMCP_MODE: "mock", YTMCP_WITH_DAEMON: "0" } : {}),
    GLASS_PROBE_MODE: mode,
    GLASS_PROBE_ARGS: JSON.stringify(modeArgs),
    GLASS_PROBE_OUT: resolve(process.env.GLASS_PROBE_OUT ?? join(here, "out")),
    ...(evidence
      ? {
          GLASS_PROBE_SHOTS_DIR: resolve(repoRoot, "artifacts/screenshots/glass"),
          GLASS_PROBE_PERF_FILE: resolve(repoRoot, "artifacts/perf/glass-real-ui.json"),
        }
      : {}),
  },
});

rmSync(scratch, { recursive: true, force: true });
process.exit(result.status ?? 1);
