import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { app } from "electron";

const entryModuleUrl = process.env.YTM_GLASS_M0_ENTRY_MODULE
  ? pathToFileURL(process.env.YTM_GLASS_M0_ENTRY_MODULE).href
  : new URL("../../../../apps/desktop/dist/main.mjs", import.meta.url).href;

const bootstrapModuleUrl = process.env.YTM_GLASS_M0_MAIN_MODULE
  ? pathToFileURL(process.env.YTM_GLASS_M0_MAIN_MODULE).href
  : new URL("../../../../apps/desktop/dist/main.mjs", import.meta.url).href;

const { startDesktopShell } = await import(bootstrapModuleUrl);
const { createIntegratedDesktopHooks } = await import(entryModuleUrl);

async function run() {
  const reportPath = process.env.YTM_GLASS_M0_REPORT_PATH
    ? resolve(process.env.YTM_GLASS_M0_REPORT_PATH)
    : null;

  const integrated = createIntegratedDesktopHooks({
    daemonBin: process.env.YTM_GLASS_DAEMON_BIN,
  });

  const controller = await startDesktopShell(integrated.hooks);
  if (!controller) {
    throw new Error("Failed to acquire single-instance lock for M0 integrated shell");
  }

  app.on("will-quit", () => {
    void integrated.shutdown();
  });

  await integrated.attachController(controller);

  if (reportPath) {
    const report = {
      probe: "M0-UI",
      timestamp: new Date().toISOString(),
      daemonStatus: integrated.rustHost.getStatus(),
      coreEpoch: integrated.rustHost.getCoreEpoch(),
      identity: {
        appId: controller.identity.appId,
        displayName: controller.identity.displayName,
        version: controller.identity.version,
      },
      mockMode: controller.mockMode,
      safeStorage: controller.safeStorageStatus,
    };
    mkdirSync(dirname(reportPath), { recursive: true });
    writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`, "utf8");
  }

  if (process.env.YTM_GLASS_M0_PROBE_EXIT === "1") {
    await integrated.shutdown();
    controller.shutdown();
    app.exit(0);
  }
}

run().catch((err) => {
  console.error("M0 integrated UI probe failed:", err);
  app.exit(1);
});
