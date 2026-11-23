import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { legacyInstanceRunning, moveLegacyDirectories, xdgBases } from "../legacy-dirs.js";

describe("moving an install from before the rename", () => {
  const roots: string[] = [];
  const root = () => {
    const dir = mkdtempSync(join(tmpdir(), "ytmcp-legacy-"));
    roots.push(dir);
    return dir;
  };
  afterEach(() => {
    for (const dir of roots.splice(0)) rmSync(dir, { recursive: true, force: true });
  });

  it("moves config, data (with the browser profile) and cache to the new name", () => {
    const bases = xdgBases({}, root());
    for (const base of bases) mkdirSync(join(base, "ytm-glass"), { recursive: true });
    writeFileSync(join(bases[0]!, "ytm-glass", "settings.json"), "{\"theme\":\"dark\"}");
    mkdirSync(join(bases[1]!, "ytm-glass", "electron-profile"));
    writeFileSync(join(bases[1]!, "ytm-glass", "electron-profile", "Cookies"), "signed in");

    const moved = moveLegacyDirectories(bases, "ytmcp");

    expect(moved).toEqual(bases.map((base) => join(base, "ytmcp")));
    expect(readFileSync(join(bases[0]!, "ytmcp", "settings.json"), "utf8")).toContain("dark");
    expect(readFileSync(join(bases[1]!, "ytmcp", "electron-profile", "Cookies"), "utf8")).toBe("signed in");
    for (const base of bases) expect(existsSync(join(base, "ytm-glass"))).toBe(false);
  });

  it("merges into a directory something else created first, never overwriting, so nothing is stranded", () => {
    const [config] = xdgBases({}, root());
    // A registration made before the first start created the new config directory.
    mkdirSync(join(config!, "ytmcp", "control-credentials"), { recursive: true });
    writeFileSync(join(config!, "ytmcp", "control-credentials", "new.json"), "new");
    writeFileSync(join(config!, "ytmcp", "settings.json"), "fresh default");
    mkdirSync(join(config!, "ytm-glass", "control-credentials"), { recursive: true });
    writeFileSync(join(config!, "ytm-glass", "control-credentials", "old.json"), "old");
    writeFileSync(join(config!, "ytm-glass", "settings.json"), "the user's settings");
    writeFileSync(join(config!, "ytm-glass", "control-clients.json"), "clients");

    moveLegacyDirectories([config!], "ytmcp");

    expect(readFileSync(join(config!, "ytmcp", "control-credentials", "old.json"), "utf8")).toBe("old");
    expect(readFileSync(join(config!, "ytmcp", "control-credentials", "new.json"), "utf8")).toBe("new");
    expect(readFileSync(join(config!, "ytmcp", "control-clients.json"), "utf8")).toBe("clients");
    // An existing file is never overwritten; the old one stays where it was.
    expect(readFileSync(join(config!, "ytmcp", "settings.json"), "utf8")).toBe("fresh default");
    expect(readFileSync(join(config!, "ytm-glass", "settings.json"), "utf8")).toBe("the user's settings");
    // Directories emptied by the merge are gone; a second run changes nothing.
    expect(existsSync(join(config!, "ytm-glass", "control-credentials"))).toBe(false);
    expect(moveLegacyDirectories([config!], "ytmcp")).toEqual([]);
  });

  it("follows the XDG variables", () => {
    expect(xdgBases({ XDG_CONFIG_HOME: "/c", XDG_DATA_HOME: "/d", XDG_CACHE_HOME: "/k" }, "/home/you")).toEqual(["/c", "/d", "/k"]);
    expect(xdgBases({}, "/home/you")).toEqual(["/home/you/.config", "/home/you/.local/share", "/home/you/.cache"]);
  });

  it("knows when the old app is still running from its profile lock", () => {
    const data = root();
    const profile = join(data, "ytm-glass", "electron-profile");
    mkdirSync(profile, { recursive: true });
    expect(legacyInstanceRunning(data, "box", () => true)).toBe(false); // no lock at all
    symlinkSync("BOX-4242", join(profile, "SingletonLock"));
    expect(legacyInstanceRunning(data, "box", (pid) => pid === 4242)).toBe(true);
    expect(legacyInstanceRunning(data, "box", () => false)).toBe(false); // a stale lock
    expect(legacyInstanceRunning(data, "other-host", () => true)).toBe(false); // another machine's
  });
});
