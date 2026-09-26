// A private server per test, for the specs that move, restore or purge.
//
// The shared servers started by playwright.config.ts live for the whole run,
// and `/api/reset` deliberately keeps the journal — so anything one test put
// in quarantine is still there, restorable and purgeable, when the next test
// asks how much a purge would take. The destructive specs therefore get their
// own database, quarantine and archive under a fresh temporary directory,
// torn down (process and files) when the test ends. Nothing here ever points
// at a real archive.
import { test as base, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { spawn } from "node:child_process";
import { mkdtempSync, mkdirSync, realpathSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

async function freePort() {
  return await new Promise<number>((ok, fail) => {
    const s = createServer();
    s.unref();
    s.on("error", fail);
    s.listen(0, "127.0.0.1", () => {
      const address = s.address();
      s.close(() =>
        typeof address === "object" && address
          ? ok(address.port)
          : fail(new Error("no port")),
      );
    });
  });
}

export const test = base.extend<{ baseURL: string }>({
  baseURL: async ({}, use) => {
    const root = realpathSync(mkdtempSync(join(tmpdir(), "pc-web-e2e-own-")));
    mkdirSync(join(root, "quarantine"));
    const port = await freePort();
    const url = `http://127.0.0.1:${port}`;
    const child = spawn(
      resolve("../../../target/debug/photo-cleanup"),
      [
        "--db",
        join(root, "test.db"),
        "serve",
        "--bind",
        `127.0.0.1:${port}`,
        "--quarantine",
        join(root, "quarantine"),
      ],
      { stdio: ["ignore", "ignore", "inherit"] },
    );
    const exited = new Promise<void>((done) =>
      child.once("exit", () => done()),
    );
    try {
      await expect
        .poll(
          async () => {
            if (child.exitCode !== null) return "exited";
            try {
              return (await fetch(`${url}/api/status`)).ok ? "up" : "down";
            } catch {
              return "down";
            }
          },
          { timeout: 30000, intervals: [100, 250, 500] },
        )
        .toBe("up");
      await use(url);
    } finally {
      if (child.exitCode === null) {
        child.kill("SIGTERM");
        await Promise.race([
          exited,
          new Promise((r) => setTimeout(r, 5000)).then(() =>
            child.kill("SIGKILL"),
          ),
        ]);
        await exited;
      }
      rmSync(root, { recursive: true, force: true });
    }
  },
});
export { expect };

/// The page's own controls, never the topbar: the activity indicator there is
/// a button named after the running job ("Move to quarantine", "Delete for
/// good", "Sort by date"), and it lingers until the next jobs poll.
export const main = (page: Page) => page.getByRole("main");

/// Waits until the interface itself has seen the last job finish — the
/// server saying "done" is not enough, the UI polls on its own schedule and
/// keeps actions disabled until it catches up.
export async function idle(page: Page) {
  await expect(
    page.getByRole("banner").getByText("Nothing running", { exact: true }),
  ).toBeVisible();
}
