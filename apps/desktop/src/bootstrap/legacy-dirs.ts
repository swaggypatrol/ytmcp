import { existsSync, lstatSync, readdirSync, readlinkSync, renameSync, rmdirSync } from "node:fs";
import { homedir, hostname } from "node:os";
import { join } from "node:path";

/** XDG directory name of installs from before the project was renamed from YTM Glass to YTMcp. */
export const LEGACY_XDG_DIR_NAME = "ytm-glass";

/** The XDG base directories the app keeps state in: config, data (with the browser profile) and cache. */
export function xdgBases(env: NodeJS.ProcessEnv = process.env, home: string = homedir()): string[] {
  return [
    env.XDG_CONFIG_HOME?.trim() || join(home, ".config"),
    env.XDG_DATA_HOME?.trim() || join(home, ".local/share"),
    env.XDG_CACHE_HOME?.trim() || join(home, ".cache"),
  ];
}

/**
 * Moves an install from before the rename to the current directory name, base by base. When
 * `<base>/ytm-glass` exists, everything in it that `<base>/<currentName>` lacks moves over;
 * directories present on both sides merge the same way, entry by entry. Nothing is ever
 * overwritten, and directories left empty are removed. So a fresh install, a second start, or a
 * `ytmcp` directory something else created first (a registration, say) all end the same way, with
 * nothing stranded under the old name. Must run before anything opens these directories. Returns
 * the paths it moved.
 */
export function moveLegacyDirectories(bases: readonly string[], currentName: string): string[] {
  const moved: string[] = [];
  for (const base of bases) {
    const legacy = join(base, LEGACY_XDG_DIR_NAME);
    const current = join(base, currentName);
    if (legacy === current || !isDirectory(legacy)) continue;
    mergeInto(legacy, current, moved);
  }
  return moved;
}

function mergeInto(from: string, to: string, moved: string[]): void {
  if (!existsSync(to)) {
    renameSync(from, to);
    moved.push(to);
    return;
  }
  if (!isDirectory(to)) return; // a file stands in the way: leave both alone
  for (const name of readdirSync(from)) {
    const source = join(from, name);
    const target = join(to, name);
    if (!existsSync(target)) {
      renameSync(source, target);
      moved.push(target);
    } else if (isDirectory(source)) {
      mergeInto(source, target, moved);
    }
  }
  if (readdirSync(from).length === 0) rmdirSync(from);
}

function isDirectory(path: string): boolean {
  try {
    return lstatSync(path).isDirectory();
  } catch {
    return false;
  }
}

/**
 * Whether an app from before the rename is still running: its browser profile's `SingletonLock`
 * (a symlink to `HOST-PID`) names a live process on this machine. Moving its directories from
 * under it would break it and strand whatever it writes next, so the migration must wait.
 */
export function legacyInstanceRunning(
  dataBase: string,
  host: string = hostname(),
  alive: (pid: number) => boolean = processAlive
): boolean {
  let target: string;
  try {
    target = readlinkSync(join(dataBase, LEGACY_XDG_DIR_NAME, "electron-profile", "SingletonLock"));
  } catch {
    return false;
  }
  const dash = target.lastIndexOf("-");
  const lockHost = target.slice(0, dash);
  const pid = Number(target.slice(dash + 1));
  return dash > 0 && Number.isInteger(pid) && pid > 0 && lockHost.toLowerCase() === host.toLowerCase() && alive(pid);
}

function processAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "EPERM";
  }
}
