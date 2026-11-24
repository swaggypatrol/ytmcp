import {
  chmodSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  readlinkSync,
  renameSync,
  rmdirSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { homedir, hostname } from "node:os";
import { join } from "node:path";

/** XDG directory name of installs from before the project was renamed from YTM Glass to YTMcp. */
export const LEGACY_XDG_DIR_NAME = "ytm-glass";
const CLIENTS_FILE = "control-clients.json";
const CREDENTIALS_DIR = "control-credentials";
const MAX_GRANTS = 16;

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

function mergeInto(
  from: string,
  to: string,
  moved: string[],
  skipCredentials: ReadonlySet<string> = new Set()
): void {
  if (!existsSync(to)) {
    renameSync(from, to);
    moved.push(to);
    return;
  }
  if (!isDirectory(to)) return; // a file stands in the way: leave both alone

  // Merge control-clients.json structurally before credentials so token hashes and credential
  // files always stay paired for colliding or capacity-capped principals.
  const skippedCredentials = new Set(skipCredentials);
  const fromClients = join(from, CLIENTS_FILE);
  const toClients = join(to, CLIENTS_FILE);
  if (existsSync(fromClients) && existsSync(toClients)) {
    for (const p of mergeControlClients(fromClients, toClients, moved)) {
      skippedCredentials.add(p);
    }
  }

  for (const name of readdirSync(from)) {
    if (name === CLIENTS_FILE && existsSync(toClients)) {
      continue;
    }
    const source = join(from, name);
    const target = join(to, name);
    if (!existsSync(target)) {
      if (name === CREDENTIALS_DIR && isDirectory(source) && skippedCredentials.size > 0) {
        mergeCredentialsDir(source, target, skippedCredentials, moved);
      } else {
        renameSync(source, target);
        moved.push(target);
      }
    } else if (isDirectory(source)) {
      if (name === CREDENTIALS_DIR) {
        mergeCredentialsDir(source, target, skippedCredentials, moved);
      } else {
        mergeInto(source, target, moved, skippedCredentials);
      }
    }
  }
  if (readdirSync(from).length === 0) rmdirSync(from);
}

interface RegistrationEntry {
  principal: string;
  [key: string]: unknown;
}

function parseRegistrationList(path: string): RegistrationEntry[] | null {
  try {
    const parsed: unknown = JSON.parse(readFileSync(path, "utf8"));
    if (!Array.isArray(parsed)) return null;
    for (const item of parsed) {
      if (
        item === null ||
        typeof item !== "object" ||
        typeof (item as { principal?: unknown }).principal !== "string" ||
        (item as { principal: string }).principal.length === 0
      ) {
        return null;
      }
    }
    return parsed as RegistrationEntry[];
  } catch {
    return null;
  }
}

function writePrivateAtomicJson(target: string, value: unknown): void {
  const tmp = `${target}.tmp.${process.pid}`;
  writeFileSync(tmp, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
  chmodSync(tmp, 0o600);
  renameSync(tmp, target);
  chmodSync(target, 0o600);
}

/**
 * Merges `fromClients` into `toClients` by `principal` (keeping `to` entries on collision, up to
 * `MAX_GRANTS`). Returns the set of principals from `from` that were NOT merged (colliding or
 * capped), whose `control-credentials/<p>.json` files must stay paired with `from`.
 */
function mergeControlClients(fromClients: string, toClients: string, moved: string[]): Set<string> {
  const fromList = parseRegistrationList(fromClients);
  const toList = parseRegistrationList(toClients);
  if (!fromList || !toList) {
    // Malformed file: do not overwrite or risk stranding credentials.
    return new Set();
  }
  const merged = [...toList];
  const seen = new Set(toList.map((e) => e.principal));
  const leftBehind: RegistrationEntry[] = [];
  let adopted = false;

  for (const entry of fromList) {
    if (!seen.has(entry.principal) && merged.length < MAX_GRANTS) {
      merged.push(entry);
      seen.add(entry.principal);
      adopted = true;
    } else {
      leftBehind.push(entry);
    }
  }

  if (adopted) {
    writePrivateAtomicJson(toClients, merged);
    moved.push(toClients);
  }
  if (leftBehind.length === 0) {
    unlinkSync(fromClients);
  } else {
    writePrivateAtomicJson(fromClients, leftBehind);
  }
  return new Set(leftBehind.map((e) => e.principal));
}

/** Moves credential files over one by one, leaving those of `skipPrincipals` where they are. */
function mergeCredentialsDir(
  fromDir: string,
  toDir: string,
  skipPrincipals: ReadonlySet<string>,
  moved: string[]
): void {
  if (!existsSync(toDir)) mkdirSync(toDir, { mode: 0o700 });
  for (const entry of readdirSync(fromDir)) {
    const principal = entry.endsWith(".json") ? entry.slice(0, -".json".length) : entry;
    if (skipPrincipals.has(principal)) continue;
    const target = join(toDir, entry);
    if (!existsSync(target)) {
      renameSync(join(fromDir, entry), target);
      moved.push(target);
    }
  }
  if (readdirSync(fromDir).length === 0) rmdirSync(fromDir);
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
