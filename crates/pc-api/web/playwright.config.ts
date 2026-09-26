import { defineConfig, devices } from "@playwright/test";
// Two shared servers, one per browser. PC_E2E_PORT moves the pair (it and the
// next port) so that suites in parallel checkouts do not collide.
const first = Number(process.env.PC_E2E_PORT || 18086);
const ports = [first, first + 1];
export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  workers: 1,
  timeout: 45000,
  expect: { timeout: 10000 },
  use: { baseURL: `http://127.0.0.1:${ports[0]}`, trace: "retain-on-failure" },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    {
      name: "webkit",
      use: {
        ...devices["Desktop Safari"],
        baseURL: `http://127.0.0.1:${ports[1]}`,
      },
    },
  ],
  webServer: ports.map((port) => ({
    command: "node tests/server.mjs",
    env: { PC_TEST_PORT: String(port) },
    url: `http://127.0.0.1:${port}/api/status`,
    reuseExistingServer: false,
    timeout: 30000,
  })),
});
