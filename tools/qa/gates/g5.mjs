// G5: exactly one MPRIS service and one KDE media-key action executed exactly once.
import { check } from "../lib/evidence.mjs";
import { mprisNames, mprisProps, run, sleep } from "../lib/sys.mjs";
import { ensurePlaying, pageMedia } from "./common.mjs";

const KGLOBALACCEL = ["org.kde.kglobalaccel", "/component/mediacontrol", "org.kde.kglobalaccel.Component.invokeShortcut", "playpausemedia"];

async function pressPlayPause(app) {
  const since = (await app.state()).seq;
  const before = { page: await pageMedia(app), mpris: await mprisProps(), core: (await app.snapshot())?.player?.playback };
  const r = await run("qdbus6", KGLOBALACCEL);
  await sleep(2500);
  const after = { page: await pageMedia(app), mpris: await mprisProps(), core: (await app.snapshot())?.player?.playback };
  const ui = await app.evalMain(`({
    button: document.querySelector('[data-testid="btn-play-pause"]')?.getAttribute("aria-label") ?? null,
    status: document.querySelector('[data-testid="player-observed-status"]')?.textContent?.trim() ?? null
  })`);
  const evs = await app.events(since, ["core->bridge"]);
  const commands = evs.filter((e) => e.kind === "player.command").map((e) => e.op);
  return {
    invokeOk: r.ok,
    pagePausedBefore: before.page.paused,
    pagePausedAfter: after.page.paused,
    mprisStatus: { before: before.mpris.status, after: after.mpris.status },
    coreBefore: before.core,
    coreAfter: after.core,
    ui,
    bridgeCommands: commands,
  };
}

export async function g5Checks(app) {
  const pids = app.pids();
  const names = await mprisNames(pids);
  const players = (await run("playerctl", ["-l"])).stdout.trim().split("\n").filter(Boolean);
  const props = await mprisProps();
  const nameCheck = check("G5.singleService", "session bus has exactly one org.mpris.MediaPlayer2.ytm_glass and no Chromium MPRIS from app processes", {
    measured: { mprisNames: names.all, ytmGlass: names.ytmGlass.length, chromiumFromApp: names.chromiumFromApp, playerctlPlayers: players, props: { status: props.status, hasTitle: props.hasTitle, lengthUs: props.lengthUs } },
    pass: names.ytmGlass.length === 1 && names.chromiumFromApp.length === 0,
  });
  await ensurePlaying(app);
  const first = await pressPlayPause(app);
  const second = await pressPlayPause(app);
  const effect = (p, toPaused) =>
    p.invokeOk &&
    p.pagePausedBefore === !toPaused &&
    p.pagePausedAfter === toPaused &&
    p.mprisStatus.after === (toPaused ? "Paused" : "Playing") &&
    p.coreAfter === (toPaused ? "paused" : "playing") &&
    p.ui.button === (toPaused ? "Play" : "Pause") &&
    p.bridgeCommands.length === 1;
  return {
    measured: { names, first, second },
    checks: [
      nameCheck,
      check("G5.mediaKeyPause", "KDE media key (kglobalaccel mediacontrol playpausemedia) pauses the real page once; MPRIS, core and UI follow", {
        measured: first,
        pass: effect(first, true),
      }),
      check("G5.mediaKeyPlay", "second press resumes once; MPRIS, core and UI follow", {
        measured: second,
        pass: effect(second, false),
      }),
    ],
  };
}
