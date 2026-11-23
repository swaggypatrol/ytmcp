import { resolve } from "node:path";
import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["tests/process/**/*.test.ts"],
    testTimeout: 20_000,
    hookTimeout: 20_000,
    pool: "forks",
  },
  resolve: {
    alias: {
      "@ytmcp/protocol": resolve(import.meta.dirname, "../../packages/protocol/src/index.ts"),
    },
  },
});
