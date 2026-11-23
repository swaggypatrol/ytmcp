import { expect, it } from "vitest";
import { validateRendererIpc } from "../ipc-guard.js";

it("rejects nested renderer objects before serialization and still accepts normal frames", () => {
  const context = {
    role: "mainUi",
    allowedOrigins: new Set(["app://ui"]),
    senderMatchesExpected: true,
    senderFrame: { isMainFrame: true, origin: "app://ui" },
  } as const;
  const frame = {
    protocolVersion: 1,
    kind: "ui.subscribeSnapshot",
    seq: 1,
    payload: {},
  };
  let nested: unknown = null;
  for (let level = 0; level < 128; level++) nested = { next: nested };

  expect(validateRendererIpc(context, { ...frame, payload: nested })).toMatchObject({
    ok: false,
    message: "IPC payload is not JSON-serializable",
  });
  expect(validateRendererIpc(context, frame)).toMatchObject({ ok: true });
});
