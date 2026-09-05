import { defineConfig } from "vitest/config";
import { fileURLToPath } from "node:url";

export default defineConfig({
  base: "./",
  build: {
    target: "es2022",
    sourcemap: true,
  },
  server: {
    fs: {
      // Let the dev server serve ../core/pkg (the wasm-pack output).
      allow: [fileURLToPath(new URL("..", import.meta.url))],
    },
  },
  test: {
    include: ["tests/**/*.test.ts"],
    environment: "node",
  },
});
