// A browser IPC stand-in connected to the real Rust relocation API on tempfiles.
// This covers the shared UI and backend contract, not native Tauri/restart smoke.
import { test, expect, type Page } from "@playwright/test";
import { execFileSync, spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { resolve } from "node:path";
import type { MovePreview } from "../src/desktop";

const workspace = resolve("../../..");
test.beforeAll(() => {
  execFileSync(
    "cargo",
    [
      "build",
      "-p",
      "pc-desktop",
      "--example",
      "data_folder_fixture",
      "--locked",
    ],
    {
      cwd: workspace,
      timeout: 120000,
    },
  );
});

async function fixture(scenario: string, suffix?: string) {
  const child = spawn(
    resolve(workspace, "target/debug/examples/data_folder_fixture"),
    [scenario, ...(suffix ? [suffix] : [])],
  );
  let stderr = "";
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  const exited = new Promise<void>((accept, reject) => {
    child.on("error", reject);
    child.on("exit", (code) =>
      code === 0
        ? accept()
        : reject(new Error(`fixture exited ${code}: ${stderr}`)),
    );
  });
  // Attach a handler immediately; failures are rethrown by reads/close below.
  void exited.catch(() => {});
  const lines = createInterface({ input: child.stdout })[
    Symbol.asyncIterator
  ]();
  const read = async () => {
    const line = await lines.next();
    if (line.done) {
      await exited;
      throw new Error("fixture closed without a response");
    }
    return JSON.parse(line.value);
  };
  return {
    preview: (await read()) as MovePreview,
    async command(command: string) {
      child.stdin.write(command + "\n");
      return read();
    },
    async close() {
      child.stdin.end();
      await exited;
    },
  };
}

async function open(page: Page, state: Awaited<ReturnType<typeof fixture>>) {
  let switched = false;
  await page.exposeFunction(
    "storageSwitch",
    async (args: { target: string; action: string }) => {
      expect(args).toEqual({
        target: state.preview.to,
        action: "use_existing",
      });
      const result = await state.command("switch");
      expect(result.switched).toBe(true);
      switched = true;
    },
  );
  await page.addInitScript((p) => {
    (window as any).__TAURI_INTERNALS__ = {
      invoke(command: string, args: unknown) {
        if (command === "desktop_info")
          return Promise.resolve({
            source: "system",
            can_change: true,
            layout: {
              dir: p.from,
              db: p.from + "/photo-cleanup.db",
              thumbs: p.from + "/thumbs",
            },
            size: p.size,
            available: p.available,
            system_dir: p.from,
            legacy_dir: null,
          });
        if (command === "pick_folder") return Promise.resolve(p.to);
        if (command === "preview_data_dir_change") return Promise.resolve(p);
        if (command === "change_data_dir")
          return (window as any).storageSwitch(args);
        return Promise.reject(new Error("Unexpected command: " + command));
      },
    };
  }, state.preview);
  await page.goto("/");
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "Settings", exact: true })
    .click();
  await page.getByRole("button", { name: "Change folder…" }).click();
  const dialog = page.getByRole("dialog", { name: "Change the data folder" });
  await expect(dialog).toBeVisible();
  return { dialog, switched: () => switched };
}

for (const scenario of ["recovery", "wal"]) {
  test(`use-existing switches the real ${scenario} database with sidecars`, async ({
    page,
  }) => {
    const state = await fixture(scenario);
    try {
      expect(state.preview.existing_database).toBe(true);
      expect(
        state.preview.blockers.filter((b) => b.kind === "sidecar_exists"),
      ).toHaveLength(scenario === "recovery" ? 3 : 2);
      const { dialog, switched } = await open(page, state);
      await expect(
        dialog.getByRole("button", { name: "Copy and restart" }),
      ).toHaveCount(0);
      const useExisting = dialog.getByRole("button", {
        name: "Switch to the database in this folder without copying",
      });
      await expect(useExisting).toBeVisible();
      expect(switched()).toBe(false);
      await useExisting.click();
      await expect.poll(switched).toBe(true);
      await expect(dialog.getByRole("alert")).toHaveCount(0);
    } finally {
      await state.close();
    }
  });
}

const negativeCases = [
  ...["sidecar", "sidecar-link", "sidecar-dangling"].flatMap((scenario) =>
    ["-wal", "-shm", "-journal"].map((suffix) => ({ scenario, suffix })),
  ),
  ...["database-link", "database-dangling", "partial"].map((scenario) => ({
    scenario,
    suffix: undefined,
  })),
];
for (const { scenario, suffix } of negativeCases) {
  test(`refuses ${scenario} ${suffix ?? ""} without changing temporary data`, async ({
    page,
  }) => {
    test.skip(
      process.platform === "win32" &&
        (scenario.includes("link") || scenario.includes("dangling")),
      "symlinks require Windows privileges; not a Windows acceptance check",
    );
    const state = await fixture(scenario, suffix);
    try {
      expect(state.preview.existing_database).toBe(scenario === "partial");
      const { dialog, switched } = await open(page, state);
      await expect(
        dialog.getByRole("button", {
          name: "Switch to the database in this folder without copying",
        }),
      ).toHaveCount(0);
      const copy = dialog.getByRole("button", { name: "Copy and restart" });
      if (scenario === "partial") await expect(copy).toHaveCount(0);
      else await expect(copy).toBeDisabled();
      await dialog.getByRole("button", { name: "Cancel" }).click();
      expect(switched()).toBe(false);
      expect(await state.command("check")).toEqual({ unchanged: true });
    } finally {
      await state.close();
    }
  });
}
