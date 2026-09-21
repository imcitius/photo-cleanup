import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
const tabs = [
  "Archive overview",
  "Inventory and index",
  "Archive tree",
  "Duplicates and versions",
  "Bursts",
  "Kinds",
  "Plan and move",
  "Sort by date",
  "Previews and caches",
  "Quarantine",
  "Journal and runs",
  "Settings",
];
const empty = {
  files: 0,
  images: 0,
  skipped: 0,
  families: 0,
  families_multi: 0,
  roles: [],
  derived_removable_bytes: 0,
  derived_blocked: 0,
  quarantined_bytes: 0,
  mislabelled: 0,
};
async function emptyState(page: Page) {
  await page.route("**/api/status", (r) => r.fulfill({ json: empty }));
  await page.route("**/api/jobs", (r) => r.fulfill({ json: [] }));
}
test("production page renders and every screen is clickable without an invisible overlay", async ({
  page,
}) => {
  await emptyState(page);
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "Let's start with your archive" }),
  ).toBeVisible();
  for (const name of tabs) {
    await page
      .getByRole("navigation")
      .getByRole("link", { name, exact: true })
      .click();
    await expect(page.locator("h1")).toHaveText(name);
    await expect(page.locator("main")).toBeVisible();
    await expect(page.locator("dialog[open]")).toHaveCount(0);
  }
  expect(
    await page
      .locator("#root")
      .evaluate((el) => el.getBoundingClientRect().height),
  ).toBeGreaterThan(600);
  expect(errors).toEqual([]);
});
test("keyboard modal restores focus; themes and tablet layouts stay accessible", async ({
  page,
}) => {
  await emptyState(page);
  await page.setViewportSize({ width: 1024, height: 900 });
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "Let's start with your archive" }),
  ).toBeVisible();
  const help = page.getByRole("button", { name: "Keys", exact: true });
  await help.click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(help).toBeFocused();
  for (const theme of ["light", "dark"]) {
    await page.evaluate(
      (t) => (document.documentElement.dataset.theme = t),
      theme,
    );
    const a = await new AxeBuilder({ page })
      .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
      .analyze();
    expect(
      a.violations.map((v) => ({
        id: v.id,
        nodes: v.nodes.map((n) => n.target),
      })),
    ).toEqual([]);
  }
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: "test-results/overview-dark.png",
    fullPage: true,
  });
});
test("server errors are readable and retryable", async ({ page }) => {
  await emptyState(page);
  let bad = true;
  await page.route("**/api/status", (r) =>
    r.fulfill({
      status: bad ? 500 : 200,
      json: bad ? { error: "/mnt/disk3/foto: permission denied" } : empty,
    }),
  );
  await page.goto("/");
  await expect(page.getByRole("alert")).toContainText("/mnt/disk3/foto");
  bad = false;
  await page.getByRole("button", { name: "Try again", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Let's start with your archive" }),
  ).toBeVisible();
});
test("a running job survives tab reload and requests cooperative cancellation", async ({
  page,
}) => {
  let done = 24,
    cancelled = false;
  const job = () => ({
    id: 99,
    kind: "index",
    state: cancelled ? "cancelled" : "running",
    params: {},
    started_at: 1700000000,
    finished_at: null,
    error: null,
    progress: {
      phase: "Reading images",
      done,
      total: 100,
      current: "/mnt/disk3/foto/DSC001.ARW",
      bytes_done: 102400,
      bytes_total: 500000,
      per_disk: [{ disk: "disk3", done, total: 100 }],
    },
  });
  await page.route("**/api/jobs", (r) => r.fulfill({ json: [job()] }));
  await page.route("**/api/jobs/99", (r) => r.fulfill({ json: job() }));
  await page.route("**/api/jobs/99/events", (r) =>
    r.fulfill({
      contentType: "text/event-stream",
      body: `data: ${JSON.stringify(job())}\n\n`,
    }),
  );
  await page.route("**/api/jobs/99/cancel", (r) => {
    cancelled = true;
    return r.fulfill({ json: { ok: true } });
  });
  await page.goto("/");
  await expect(
    page.getByRole("progressbar", { name: "Indexing", exact: true }),
  ).toHaveAttribute("aria-valuenow", "24");
  done = 42;
  await page.reload();
  await expect(
    page.getByRole("progressbar", { name: "Indexing", exact: true }),
  ).toHaveAttribute("aria-valuenow", "42");
  await page.getByRole("button", { name: "Stop", exact: true }).click();
  await expect(
    page.getByRole("progressbar", { name: "Indexing", exact: true }),
  ).toHaveCount(0);
  expect(cancelled).toBe(true);
});
test("interrupted work stays visible and leads to the journal", async ({
  page,
}) => {
  await emptyState(page);
  await page.route("**/api/jobs", (r) =>
    r.fulfill({
      json: [
        {
          id: 8,
          kind: "organize-apply",
          state: "interrupted",
          progress: {},
          started_at: 1700000000,
          error: "The server restarted",
        },
      ],
    }),
  );
  await page.goto("/");
  await expect(page.getByText("There is unfinished work")).toBeVisible();
  await page.getByRole("link", { name: "Check the journal →" }).click();
  await expect(page.locator("h1")).toHaveText("Journal and runs");
});
test("50,000 families use a bounded DOM, filters and optimistic rollback", async ({
  page,
}) => {
  const member = (id: number, keeper: boolean) => ({
    file_id: id,
    name: `DSC${id}.JPG`,
    dir: "/mnt/disk3/foto/2019",
    role: keeper ? "original" : "copy",
    role_label: "",
    size: 102400,
    width: 6000,
    height: 4000,
    quality: 87,
    breakdown: "sharpness +20; detail +15",
    evidence: null,
    thumb: null,
    is_keeper: keeper,
  });
  await page.route("**/api/families?*", (r) => {
    const url = new URL(r.request().url());
    const offset = +(url.searchParams.get("offset") || 0),
      search = url.searchParams.get("search");
    return r.fulfill({
      json: {
        total: search ? 0 : 50000,
        families: search
          ? []
          : Array.from({ length: 100 }, (_, i) => ({
              id: offset + i,
              taken_at: 1563129000,
              camera: "Sony A7 III",
              total_size: 204800,
              removable_bytes: 102400,
              members: [
                member((offset + i) * 2, true),
                member((offset + i) * 2 + 1, false),
              ],
            })),
      },
    });
  });
  await page.route("**/api/families/*/keeper", (r) =>
    r.fulfill({ status: 500, json: { error: "/mnt/disk3: unreachable" } }),
  );
  await page.goto("/#families");
  await expect(page.getByText("50,000 groups")).toBeVisible();
  expect(await page.locator(".family-list-item").count()).toBeLessThan(20);
  await page.locator(".family-list").evaluate((el) => (el.scrollTop = 92000));
  await expect(page.locator(".family-list-item").first()).toBeVisible();
  expect(await page.locator(".family-list-item").count()).toBeLessThan(20);
  await page
    .getByRole("button", { name: "Keep this one", exact: true })
    .last()
    .click();
  await expect(page.getByRole("alert")).toContainText("/mnt/disk3");
  await expect(page.locator(".keeper")).toHaveCount(1);
  await page
    .getByRole("textbox", { name: "Search by name or path" })
    .fill("no such frame");
  await expect(
    page.getByRole("heading", { name: "Nothing found" }),
  ).toBeVisible();
});
test("purge needs a reviewed plan, a checkbox and the exact confirmation word", async ({
  page,
}) => {
  await page.route("**/api/quarantine", (r) =>
    r.fulfill({
      json: [
        {
          journal_id: 1,
          file_id: null,
          src: "/mnt/disk3/Previews.lrdata",
          dst: "/mnt/disk3/.pc-quarantine/Previews.lrdata",
          name: "Previews.lrdata",
          size: 100,
          file_count: 3,
          applied_at: 1,
          kind: "derived data",
          thumb: null,
        },
      ],
    }),
  );
  await page.route("**/api/preview", (r) =>
    r.fulfill({
      json: {
        kind: "derived-purge",
        params: { older_than_secs: 604800 },
        token: "reviewed",
        items: [
          {
            path: "/mnt/disk3/.pc-quarantine/Previews.lrdata",
            dst: "Permanent deletion",
            size: 100,
            file_count: 3,
          },
        ],
        refusals: [],
        total_files: 3,
        total_bytes: 100,
      },
    }),
  );
  let submitted = false;
  await page.route("**/api/jobs", (r) => {
    if (r.request().method() === "POST") {
      const body = r.request().postDataJSON();
      expect(body.confirmation).toBe("DELETE");
      expect(body.plan_token).toBe("reviewed");
      submitted = true;
      return r.fulfill({ json: { job_id: 9 } });
    }
    return r.fulfill({ json: [] });
  });
  await page.goto("/#quarantine");
  await page.getByRole("button", { name: "Check before deleting" }).click();
  await page
    .getByRole("button", { name: "Delete for good", exact: true })
    .click();
  const dialog = page.getByRole("dialog");
  await expect(
    dialog.getByRole("button", { name: "Delete for good", exact: true }),
  ).toBeDisabled();
  await dialog.getByRole("checkbox").check();
  await dialog.getByRole("textbox").fill("DELETE");
  await dialog
    .getByRole("button", { name: "Delete for good", exact: true })
    .click();
  expect(submitted).toBe(true);
});
