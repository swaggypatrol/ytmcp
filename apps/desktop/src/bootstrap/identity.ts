import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

export interface AppIdentity {
  readonly schemaVersion: number;
  readonly displayName: string;
  readonly codename: string;
  readonly appId: string;
  readonly desktopEntry: string;
  readonly mprisBusName: string;
  readonly mprisIdentity: string;
  readonly xdgDirName: string;
  readonly playerSessionPartition: string;
  readonly uiSessionPartition: string;
  readonly appScheme: string;
  readonly protocolVersion: number;
  readonly version: string;
  readonly license: string;
}

const REQUIRED_STRING_FIELDS: ReadonlyArray<keyof AppIdentity> = [
  "displayName",
  "codename",
  "appId",
  "desktopEntry",
  "mprisBusName",
  "mprisIdentity",
  "xdgDirName",
  "playerSessionPartition",
  "uiSessionPartition",
  "appScheme",
  "version",
  "license",
];

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/**
 * Resolves the workspace root by walking upwards from a starting directory until
 * `config/app-identity.json` is found.
 */
export function resolveIdentityFilePath(explicitPath?: string): string {
  const envOverride = explicitPath ?? process.env.YTM_GLASS_IDENTITY_PATH;
  if (envOverride) {
    return resolve(envOverride);
  }

  const searchStarts = [
    import.meta.dirname,
    process.cwd(),
    ...(typeof process.resourcesPath === "string" ? [process.resourcesPath] : []),
  ];

  for (const start of searchStarts) {
    let current = resolve(start);
    for (let depth = 0; depth < 8; depth++) {
      const candidate = resolve(current, "config/app-identity.json");
      if (existsSync(candidate)) {
        return candidate;
      }
      const packagedCandidate = resolve(current, "app-identity.json");
      if (existsSync(packagedCandidate)) {
        return packagedCandidate;
      }
      const parent = dirname(current);
      if (parent === current) {
        break;
      }
      current = parent;
    }
  }

  throw new Error("Unable to locate config/app-identity.json");
}

/**
 * Validates and freezes a parsed app-identity JSON object.
 */
export function parseAppIdentity(raw: unknown): AppIdentity {
  if (!isRecord(raw)) {
    throw new Error("app-identity.json must be a JSON object");
  }

  if (typeof raw.schemaVersion !== "number" || !Number.isInteger(raw.schemaVersion) || raw.schemaVersion < 1) {
    throw new Error("app-identity.json schemaVersion must be a positive integer");
  }
  if (typeof raw.protocolVersion !== "number" || !Number.isInteger(raw.protocolVersion) || raw.protocolVersion < 1) {
    throw new Error("app-identity.json protocolVersion must be a positive integer");
  }

  for (const key of REQUIRED_STRING_FIELDS) {
    const val = raw[key];
    if (typeof val !== "string" || val.trim().length === 0) {
      throw new Error(`app-identity.json missing non-empty string field: ${String(key)}`);
    }
  }

  const appScheme = raw.appScheme as string;
  if (!/^[a-z][a-z0-9+.-]*$/.test(appScheme)) {
    throw new Error(`app-identity.json invalid appScheme: ${appScheme}`);
  }

  const playerPartition = raw.playerSessionPartition as string;
  if (!playerPartition.startsWith("persist:")) {
    throw new Error("app-identity.json playerSessionPartition must be persistent (persist:...)");
  }

  if (playerPartition === raw.uiSessionPartition) {
    throw new Error("app-identity.json playerSessionPartition and uiSessionPartition must differ");
  }

  return Object.freeze({
    schemaVersion: raw.schemaVersion,
    displayName: raw.displayName as string,
    codename: raw.codename as string,
    appId: raw.appId as string,
    desktopEntry: raw.desktopEntry as string,
    mprisBusName: raw.mprisBusName as string,
    mprisIdentity: raw.mprisIdentity as string,
    xdgDirName: raw.xdgDirName as string,
    playerSessionPartition: playerPartition,
    uiSessionPartition: raw.uiSessionPartition as string,
    appScheme,
    protocolVersion: raw.protocolVersion,
    version: raw.version as string,
    license: raw.license as string,
  });
}

let cachedIdentity: AppIdentity | null = null;

/**
 * Loads and caches `config/app-identity.json`.
 */
export function loadAppIdentity(explicitPath?: string): AppIdentity {
  if (!explicitPath && cachedIdentity) {
    return cachedIdentity;
  }
  const filePath = resolveIdentityFilePath(explicitPath);
  const rawText = readFileSync(filePath, "utf8");
  const parsed: unknown = JSON.parse(rawText);
  const identity = parseAppIdentity(parsed);
  if (!explicitPath) {
    cachedIdentity = identity;
  }
  return identity;
}

export function resetCachedAppIdentityForTest(): void {
  cachedIdentity = null;
}
