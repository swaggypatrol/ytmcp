import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, it, vi } from "vitest";
import { resolveDaemonBinaryPath } from "../client.js";

it("finds a bundled sidecar without a wrapper and honors explicit overrides", () => {
  const resourcesPath = mkdtempSync(join(tmpdir(), "ytm-daemon-resources-"));
  const descriptor = Object.getOwnPropertyDescriptor(process, "resourcesPath");
  Object.defineProperty(process, "resourcesPath", { configurable: true, value: resourcesPath });
  vi.stubEnv("YTM_GLASS_DAEMON_BIN", undefined);
  try {
    const bundled = join(resourcesPath, "bin", "ytm-daemon");
    const override = join(resourcesPath, "custom-daemon");
    mkdirSync(join(resourcesPath, "bin"));
    writeFileSync(bundled, "", { mode: 0o755 });
    writeFileSync(override, "", { mode: 0o755 });
    expect(resolveDaemonBinaryPath()).toBe(bundled);

    vi.stubEnv("YTM_GLASS_DAEMON_BIN", override);
    expect(resolveDaemonBinaryPath()).toBe(override);
    expect(resolveDaemonBinaryPath(bundled)).toBe(bundled);
  } finally {
    if (descriptor) Object.defineProperty(process, "resourcesPath", descriptor);
    else Reflect.deleteProperty(process, "resourcesPath");
    vi.unstubAllEnvs();
    rmSync(resourcesPath, { recursive: true, force: true });
  }
});
