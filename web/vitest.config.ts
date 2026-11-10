import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import path from "node:path";

/**
 * Console unit tests.
 *
 * Coverage policy: everything except the generated coverage directory, so a
 * future `web/*.test.tsx` is picked up automatically.
 */
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: false,
    include: ["**/*.test.{ts,tsx}"],
    exclude: ["node_modules/**", "dist/**", ".next/**"],
    restoreMocks: true,
    setupFiles: [".vitest.setup.ts"],
    coverage: {
      provider: "v8",
      reporter: ["text"],
      include: ["lib/**", "components/**", "app/**"],
      exclude: ["**/*.test.*", "app/**/layout.tsx", "components/ui/**"],
    },
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "."),
    },
  },
});
