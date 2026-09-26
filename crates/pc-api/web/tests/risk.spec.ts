// The promises that cost a photograph when they fail, checked in a browser
// against a real server and real files on disk.
import { test, expect, main } from "./isolated";
import { mkdirSync, writeFileSync, existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import type { APIRequestContext, Page } from "@playwright/test";

const QUARANTINE = ".photo-cleanup-quarantine";

// A picture that compresses to something a decoder will accept, drawn in the
// page so the test needs no binary fixture.
async function jpeg(page: Page, tint: number) {
  return await page.evaluate((t) => {
    const c = document.createElement("canvas");
    c.width = 320;
    c.height = 240;
    const ctx = c.getContext("2d")!;
    for (let y = 0; y < c.height; y += 4)
      for (let x = 0; x < c.width; x += 4) {
        ctx.fillStyle = `rgb(${(x * 7 + t) % 256},${(y * 11 + t) % 256},${(x * 3 + y * 5) % 256})`;
        ctx.fillRect(x, y, 4, 4);
      }
    return c.toDataURL("image/jpeg", 0.95).split(",")[1];
  }, tint);
}

async function archiveOf(request: APIRequestContext, name: string) {
  const settings = await (await request.get("/api/settings")).json();
  const archive = join(dirname(settings.db_path), `${name}-${Date.now()}`);
  mkdirSync(archive, { recursive: true });
  return archive;
}

// Every test here moves, restores or deletes, and `/api/reset` keeps the
// journal: a reset between tests would still leave the previous test's
// quarantine restorable and purgeable. Each test gets its own server instead.

async function job(
  request: APIRequestContext,
  kind: string,
  params: Record<string, unknown>,
) {
  await request.post("/api/jobs", { data: { kind, params } });
  await expect
    .poll(async () => (await (await request.get("/api/jobs")).json())[0]?.state)
    .toBe("done");
}

test("a plan that changed under the reviewer is refused, not carried out", async ({
  page,
  request,
}) => {
  const archive = await archiveOf(request, "stale");
  await page.goto("/#plan");
  const data = await jpeg(page, 10);
  const copy = join(archive, "frame copy.jpg");
  writeFileSync(join(archive, "frame.jpg"), Buffer.from(data, "base64"));
  writeFileSync(copy, Buffer.from(data, "base64"));
  await job(request, "index", { roots: [archive], min_size: 0 });
  await job(request, "families", {});

  const plan = await (
    await request.post("/api/preview", {
      data: { kind: "plan-apply", params: { roles: ["copy"] } },
    })
  ).json();
  expect(plan.total_files).toBe(1);

  // Between the review and the press, the archive gains another copy: what
  // the reviewer approved is no longer what would happen.
  writeFileSync(join(archive, "frame copy 2.jpg"), Buffer.from(data, "base64"));
  await job(request, "index", { roots: [archive], min_size: 0 });
  await job(request, "families", {});

  const refused = await request.post("/api/jobs", {
    data: {
      kind: "plan-apply",
      params: { roles: ["copy"] },
      plan_token: plan.token,
      confirmation: "DELETE",
    },
  });
  // The job is accepted for running and refuses itself at the plan check,
  // which is where the token is compared.
  const id = (await refused.json()).job_id;
  await expect
    .poll(
      async () => (await (await request.get(`/api/jobs/${id}`)).json()).state,
    )
    .toBe("failed");
  const failed = await (await request.get(`/api/jobs/${id}`)).json();
  expect(failed.error).toContain("plan has changed");
  expect(existsSync(copy)).toBe(true);
});

test("quarantine left by an older database comes home to where it came from", async ({
  page,
  request,
}) => {
  const archive = await archiveOf(request, "orphans");
  // A directory that was quarantined whole by a database that is now gone,
  // with its own tree inside it.
  const nested = join(archive, QUARANTINE, "Old Previews.lrdata", "sub");
  mkdirSync(nested, { recursive: true });
  writeFileSync(join(nested, "cache"), "previews from another database");
  await job(request, "scan", { roots: [archive] });

  await page.goto("/#quarantine");
  await page.reload();
  // The orphan panel, not the journal-backed quarantine list below it: each
  // of those rows has a "Restore" button of its own.
  const orphans = main(page)
    .locator("section")
    .filter({
      has: page.getByRole("heading", { name: "Quarantine with no journal" }),
    });
  await expect(orphans).toBeVisible();
  await orphans
    .getByRole("button", { name: "Put everything back", exact: true })
    .click();

  const home = join(archive, "Old Previews.lrdata", "sub", "cache");
  await expect(orphans.locator(".plan-row")).toHaveCount(1);
  await expect(orphans.locator(".plan-row")).toContainText("cache");
  // Out of quarantine, not into it: the button says Restore.
  await orphans.getByRole("button", { name: "Restore", exact: true }).click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Run the plan", exact: true })
    .click();

  await expect.poll(() => existsSync(home)).toBe(true);
  expect(readFileSync(home, "utf8")).toBe("previews from another database");
  expect(existsSync(join(nested, "cache"))).toBe(false);
  // And it is a move like any other: the journal can take it back.
  const journal = await (await request.get("/api/journal")).json();
  expect(journal.some((j: { op: string }) => j.op === "adopt")).toBe(true);
});

test("deleting for good takes the bytes, and only after the word", async ({
  page,
  request,
}) => {
  // The other purge test answers a made-up server. This one watches the file.
  const archive = await archiveOf(request, "purge");
  await page.goto("/#quarantine");
  const data = await jpeg(page, 60);
  writeFileSync(join(archive, "frame.jpg"), Buffer.from(data, "base64"));
  writeFileSync(join(archive, "frame copy.jpg"), Buffer.from(data, "base64"));
  await job(request, "index", { roots: [archive], min_size: 0 });
  await job(request, "families", {});

  const plan = await (
    await request.post("/api/preview", {
      data: { kind: "plan-apply", params: { roles: ["copy"] } },
    })
  ).json();
  expect(plan.total_files).toBe(1);
  const quarantined = plan.items[0].dst;
  await request.post("/api/jobs", {
    data: {
      kind: "plan-apply",
      params: { roles: ["copy"] },
      plan_token: plan.token,
      confirmation: "DELETE",
    },
  });
  await expect.poll(() => existsSync(quarantined)).toBe(true);

  // The holding period is counted in whole seconds, and nothing is offered
  // for deletion on the same second it arrived.
  await expect
    .poll(
      async () =>
        (
          await (
            await request.post("/api/preview", {
              data: { kind: "derived-purge", params: { older_than_secs: 0 } },
            })
          ).json()
        ).total_files,
      { timeout: 5000 },
    )
    .toBe(1);
  await page.reload();
  await page
    .getByRole("spinbutton", { name: "Held for at least, days" })
    .fill("0");
  await main(page)
    .getByRole("button", { name: "Check before deleting", exact: true })
    .click();
  await main(page)
    .getByRole("button", { name: "Delete for good", exact: true })
    .click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("checkbox").check();
  // The wrong word is not the word.
  await dialog.getByRole("textbox").fill("delete");
  await expect(
    dialog.getByRole("button", { name: "Delete for good", exact: true }),
  ).toBeDisabled();
  expect(existsSync(quarantined)).toBe(true);

  await dialog.getByRole("textbox").fill("DELETE");
  await dialog
    .getByRole("button", { name: "Delete for good", exact: true })
    .click();
  await expect.poll(() => existsSync(quarantined)).toBe(false);
  // What was kept is untouched, and the journal says the entry is purged.
  expect(existsSync(join(archive, "frame.jpg"))).toBe(true);
  const journal = await (await request.get("/api/journal")).json();
  expect(
    journal.some(
      (j: { status: string; dst: string | null }) =>
        j.dst === quarantined && j.status === "purged",
    ),
  ).toBe(true);
});
