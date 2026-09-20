import { defineConfig, devices } from "@playwright/test";
export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  workers: 1,
  timeout: 45000,
  expect: { timeout: 10000 },
  use: { baseURL: "http://127.0.0.1:18086", trace: "retain-on-failure" },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    {
      name: "webkit",
      use: { ...devices["Desktop Safari"], baseURL: "http://127.0.0.1:18087" },
    },
  ],
  webServer: [18086, 18087].map((port) => ({
    command: "node tests/server.mjs",
    env: { PC_TEST_PORT: String(port) },
    url: `http://127.0.0.1:${port}/api/status`,
    reuseExistingServer: false,
    timeout: 30000,
  })),
});
