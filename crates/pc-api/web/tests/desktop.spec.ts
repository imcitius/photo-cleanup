// The desktop adapter: the same interface, with native folder dialogs and a
// data-folder screen when — and only when — the window's IPC is present.
//
// The IPC here is a stand-in injected before the page loads; it records
// every call and answers from a script. What it proves is the interface's
// side of the contract: which commands are called with what, that cancel
// changes nothing, and that nothing is moved without the explicit button.
// The window's side (capabilities, the copy itself) is tested in Rust and in
// the native smoke checks.
import { test, expect, type Page } from "@playwright/test";

type Call = { command: string; args: Record<string, unknown> };

const info = {
  source: "system",
  can_change: true,
  layout: {
    dir: "/Users/me/Library/Application Support/io.github.imcitius.photo-cleanup/data",
    db: "/Users/me/Library/Application Support/io.github.imcitius.photo-cleanup/data/photo-cleanup.db",
    thumbs:
      "/Users/me/Library/Application Support/io.github.imcitius.photo-cleanup/data/thumbs",
  },
  size: { db_bytes: 5 * 1024 * 1024, thumbs_bytes: 3 * 1024, thumbs_files: 2 },
  available: 10 * 1024 * 1024 * 1024,
  system_dir:
    "/Users/me/Library/Application Support/io.github.imcitius.photo-cleanup/data",
  legacy_dir: null,
};

/** Install the stand-in IPC. `answers` maps a command to a list of replies,
 *  used in order; `{ error }` rejects. */
async function desktop(
  page: Page,
  answers: Record<string, unknown[]>,
): Promise<() => Promise<Call[]>> {
  await page.addInitScript(
    ([answers, info]) => {
      const calls: Call[] = [];
      const queue = answers as Record<string, unknown[]>;
      (window as unknown as { __calls: Call[] }).__calls = calls;
      (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
        invoke(command: string, args: Record<string, unknown> = {}) {
          calls.push({ command, args });
          if (command === "desktop_info") return Promise.resolve(info);
          const reply = (queue[command] || []).shift();
          if (reply && typeof reply === "object" && "error" in reply)
            return Promise.reject((reply as { error: string }).error);
          return Promise.resolve(reply ?? null);
        },
      };
    },
    [answers, info] as const,
  );
  return () =>
    page.evaluate(() => (window as unknown as { __calls: Call[] }).__calls);
}

async function open(page: Page, tab: string) {
  await page.goto("/");
  await page
    .getByRole("navigation")
    .getByRole("link", { name: tab, exact: true })
    .click();
  await expect(page.locator("h1")).toHaveText(tab);
}

test("in a browser the server folder browser and server paths stay", async ({
  page,
}) => {
  await open(page, "Inventory and index");
  await page.getByRole("button", { name: "Browse folders" }).click();
  await expect(
    page.getByRole("dialog", { name: "Choose a folder on the server" }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await open(page, "Settings");
  await expect(
    page.getByRole("heading", { name: "App data folder" }),
  ).toHaveCount(0);
  await expect(page.getByLabel("Database", { exact: true })).toBeVisible();
});

test("the native dialog picks archive roots, and cancel adds nothing", async ({
  page,
}) => {
  const calls = await desktop(page, {
    pick_folder: [null, "/Volumes/Архив/Фото 2015"],
  });
  await open(page, "Inventory and index");
  const browse = page.getByRole("button", { name: "Browse folders" });
  await browse.click();
  await expect.poll(async () => (await calls()).length).toBeGreaterThan(0);
  // Cancelled: no server browser, no root.
  await expect(page.locator("dialog[open]")).toHaveCount(0);
  await expect(page.getByText("/Volumes/Архив/Фото 2015")).toHaveCount(0);
  await browse.click();
  await expect(page.getByText("/Volumes/Архив/Фото 2015")).toBeVisible();
  const picks = (await calls()).filter((c) => c.command === "pick_folder");
  expect(picks).toHaveLength(2);
  expect(picks[0].args.title).toBe("Choose a folder");
});

test("the data folder is shown, and a change needs a preview and an explicit button", async ({
  page,
}) => {
  const target = "/Volumes/Big Disk/Photo Cleanup данные";
  const taken = "/Volumes/Old/photo-cleanup";
  const preview = {
    from: info.layout.dir,
    to: target,
    size: info.size,
    needed: 70 * 1024 * 1024,
    available: 900 * 1024 * 1024,
    blockers: [],
    reasons: [],
    existing_database: false,
  };
  const calls = await desktop(page, {
    pick_folder: [null, taken, target],
    preview_data_dir_change: [
      {
        ...preview,
        to: taken,
        blockers: [{ kind: "database_exists" }],
        reasons: [
          "the folder already holds photo-cleanup.db; it is not overwritten",
        ],
        existing_database: true,
      },
      preview,
    ],
    change_data_dir: [{ error: "not enough space: 70 MiB needed, 1 MiB free" }],
  });
  await open(page, "Settings");
  await expect(
    page.getByRole("heading", { name: "App data folder" }),
  ).toBeVisible();
  await expect(page.getByLabel("Folder", { exact: true })).toHaveValue(
    info.layout.dir,
  );
  await expect(page.getByTestId("data-folder-size")).toContainText("5 MiB");
  // Server-only fields give way to the panel.
  await expect(page.getByLabel("Database", { exact: true })).toHaveValue(
    info.layout.db,
  );

  const change = page.getByRole("button", { name: "Change folder…" });
  // Cancelled dialog: no preview, nothing else asked.
  await change.click();
  await expect
    .poll(async () => (await calls()).map((c) => c.command))
    .toContain("pick_folder");
  await expect(page.locator("dialog[open]")).toHaveCount(0);

  // A folder with a database: no copy over it, only the explicit switch.
  await change.click();
  const dialog = page.getByRole("dialog", { name: "Change the data folder" });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("it is not overwritten");
  await expect(
    dialog.getByRole("button", { name: "Copy and restart" }),
  ).toHaveCount(0);
  await expect(
    dialog.getByRole("button", {
      name: "Switch to the database in this folder without copying",
    }),
  ).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toHaveCount(0);

  // A free folder: the copy runs only on the button, and a failure is shown
  // with the old folder still in use.
  await change.click();
  await expect(dialog).toContainText(target);
  await expect(dialog).toContainText("nothing is deleted");
  await dialog.getByRole("button", { name: "Copy and restart" }).click();
  await expect(dialog.getByRole("alert")).toContainText(
    "The change failed. The original data has been kept: not enough space",
  );

  const commands = (await calls()).map((c) => c.command);
  expect(commands.filter((c) => c === "change_data_dir")).toHaveLength(1);
  const changed = (await calls()).find((c) => c.command === "change_data_dir");
  expect(changed?.args).toEqual({ target, action: "copy" });
  // Cancel and the refused folder never reached the change.
  expect(commands.indexOf("change_data_dir")).toBe(commands.length - 1);
});

test("a volume that cannot keep private folders forbids the copy, not the switch", async ({
  page,
}) => {
  const card = "/Volumes/CARD/photo-cleanup";
  const reason =
    "/Volumes/CARD/photo-cleanup cannot hold the move's temporary folders private to this user: " +
    "volume /Volumes/CARD (exfat) is mounted with ownership ignored (noowners): every user of " +
    "this computer has owner rights to every folder on it. Choose a folder on another volume";
  // exFAT also cannot rename without replacing (el-21zyg): a second
  // blocker of the same kind — it forbids the copy, not the switch.
  const rename =
    "/Volumes/CARD/photo-cleanup cannot receive the data: its volume cannot rename without " +
    "replacing (volume /Volumes/CARD (exfat) cannot rename without replacing what is at the " +
    "new name (RENAME_EXCL)), and without that the move can neither publish the copy nor " +
    "safely remove its own leftovers. Choose a folder on another volume";
  const refused = {
    from: info.layout.dir,
    to: card,
    size: info.size,
    needed: 70 * 1024 * 1024,
    available: 900 * 1024 * 1024,
    blockers: [
      { kind: "no_private_folders", path: card, reason: "..." },
      { kind: "no_exclusive_rename", path: card, reason: "..." },
    ],
    reasons: [reason, rename],
    existing_database: false,
  };
  const calls = await desktop(page, {
    pick_folder: [card, card],
    preview_data_dir_change: [
      refused,
      {
        ...refused,
        blockers: [...refused.blockers, { kind: "database_exists" }],
        reasons: [
          ...refused.reasons,
          "the folder already holds photo-cleanup.db; it is not overwritten",
        ],
        existing_database: true,
      },
    ],
  });
  await open(page, "Settings");
  const change = page.getByRole("button", { name: "Change folder…" });
  const dialog = page.getByRole("dialog", { name: "Change the data folder" });

  // An empty folder there: the reason, with path and volume, and no copy.
  await change.click();
  await expect(dialog).toContainText(reason);
  await expect(dialog).toContainText(rename);
  await expect(
    dialog.getByRole("button", { name: "Copy and restart" }),
  ).toBeDisabled();
  await expect(
    dialog.getByRole("button", {
      name: "Switch to the database in this folder without copying",
    }),
  ).toHaveCount(0);
  await dialog.getByRole("button", { name: "Cancel" }).click();

  // A database already there: switching to it makes no temporary folders,
  // so it stays available.
  await change.click();
  await expect(dialog).toContainText(reason);
  await expect(
    dialog.getByRole("button", {
      name: "Switch to the database in this folder without copying",
    }),
  ).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel" }).click();
  expect((await calls()).map((c) => c.command)).not.toContain(
    "change_data_dir",
  );
});

test("a path others could redirect forbids the copy, not the switch", async ({
  page,
}) => {
  const shared = "/Users/Shared/photo-cleanup";
  const reason =
    "the target folder /Users/Shared/photo-cleanup is on a path other users could redirect " +
    "or re-permission: /Users/Shared permissions 1777 let other users add, rename and " +
    "remove entries in it. Choose a folder whose whole path belongs only to you and the " +
    "system, for example inside your home folder";
  const refused = {
    from: info.layout.dir,
    to: shared,
    size: info.size,
    needed: 70 * 1024 * 1024,
    available: 900 * 1024 * 1024,
    blockers: [
      {
        kind: "unprotected_folder",
        role: "target",
        path: shared,
        component: "/Users/Shared",
        reason: "...",
      },
    ],
    reasons: [reason],
    existing_database: false,
  };
  const calls = await desktop(page, {
    pick_folder: [shared, shared],
    preview_data_dir_change: [
      refused,
      {
        ...refused,
        blockers: [...refused.blockers, { kind: "database_exists" }],
        reasons: [
          ...refused.reasons,
          "the folder already holds photo-cleanup.db; it is not overwritten",
        ],
        existing_database: true,
      },
    ],
  });
  await open(page, "Settings");
  const change = page.getByRole("button", { name: "Change folder…" });
  const dialog = page.getByRole("dialog", { name: "Change the data folder" });

  await change.click();
  await expect(dialog).toContainText(reason);
  await expect(
    dialog.getByRole("button", { name: "Copy and restart" }),
  ).toBeDisabled();
  await expect(
    dialog.getByRole("button", {
      name: "Switch to the database in this folder without copying",
    }),
  ).toHaveCount(0);
  await dialog.getByRole("button", { name: "Cancel" }).click();

  await change.click();
  await expect(
    dialog.getByRole("button", {
      name: "Switch to the database in this folder without copying",
    }),
  ).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel" }).click();
  expect((await calls()).map((c) => c.command)).not.toContain(
    "change_data_dir",
  );
});
