import Ajv from "ajv";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const readJson = (path: string) =>
  JSON.parse(readFileSync(new URL(path, import.meta.url), "utf8"));
const schema = readJson("../schema/control.v1.schema.json");
const ajv = new Ajv({ strict: false });
ajv.addSchema(schema, "control-v1");
const validateRequest = ajv.compile({ $ref: "control-v1#/definitions/RequestFrame" });
const validateResponse = ajv.compile({ $ref: "control-v1#/definitions/ServerFrame" });

describe("Control v1 contract", () => {
  it("shares the Rust tool request acceptance fixtures", () => {
    const fixtures = readJson("../../../fixtures/control-v1/request-cases.json");
    for (const request of fixtures.valid) expect(validateRequest(request), JSON.stringify(validateRequest.errors)).toBe(true);
    for (const request of fixtures.invalid) expect(validateRequest(request), JSON.stringify(request)).toBe(false);
  });

  it("shares received operation and playlist fixtures without exposing host data", () => {
    for (const response of readJson("../../../fixtures/control-v1/response-cases.json")) {
      expect(validateResponse(response), JSON.stringify(validateResponse.errors)).toBe(true);
    }
    expect(JSON.stringify(schema)).not.toMatch(/cookiePairs|SessionMaterialPayload|credentialRevision|confirmedByUser/);
  });
});
