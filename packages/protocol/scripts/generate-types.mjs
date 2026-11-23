import { readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { compile } from "json-schema-to-typescript";

const __dirname = dirname(fileURLToPath(import.meta.url));
const pkgRoot = resolve(__dirname, "..");
const control = process.argv.includes("--control");
const schemaPath = resolve(pkgRoot, control ? "schema/control.v1.schema.json" : "schema/protocol.v1.schema.json");
const outPath = resolve(pkgRoot, control ? "src/generated/control.ts" : "src/generated/types.ts");

const schema = JSON.parse(readFileSync(schemaPath, "utf8"));

const ts = await compile(schema, control ? "ControlV1" : "ProtocolSchemaRoot", {
  bannerComment:
    `/* eslint-disable */\n/**\n * GENERATED FILE — DO NOT EDIT MANUALLY.\n * Regenerate via: just ${control ? "gen-control" : "gen-protocol"}\n */`,
  additionalProperties: false,
  unreachableDefinitions: true,
  strictIndexSignatures: true,
  style: {
    singleQuote: false,
  },
});

mkdirSync(dirname(outPath), { recursive: true });
writeFileSync(outPath, ts, "utf8");
console.error(`Generated TypeScript definitions at ${outPath}`);
