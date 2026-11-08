import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["tests/**/*.test.ts"],
    // The end-to-end tests start a real server.
    testTimeout: 30_000,
    hookTimeout: 60_000,
  },
});
