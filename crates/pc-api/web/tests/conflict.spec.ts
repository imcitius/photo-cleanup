// An undo whose original place is taken by another file (el-14vx0): the
// dialog lists the conflict with the choices the server offers, keeps the
// file by default, and carries out the chosen one — for one file, or for
// all the remaining ones at once. Real server, real files, disposable
// archive under the test's own temporary directory.
import { test, expect, main } from "./isolated";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import type { APIRequestContext, Page } from "@playwright/test";

const NEWER = "a newer file that took the name";

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

/// A reviewed disk job, carried out as the interface would.
async function reviewed(
  request: APIRequestContext,
  kind: string,
  params: Record<string, unknown>,
) {
  const plan = await (
    await request.post("/api/preview", { data: { kind, params } })
  ).json();
  await request.post("/api/jobs", {
    data: { kind, params, plan_token: plan.token, confirmation: "DELETE" },
  });
  await expect
    .poll(async () => (await (await request.get("/api/jobs")).json())[0]?.state)
    .toBe("done");
  return plan;
}

test("a restore whose place is taken keeps the file by default and returns it as *_1 when asked", async ({
  page,
  request,
}) => {
  const archive = await archiveOf(request, "conflict");
  await page.goto("/#journal");
  const data = Buffer.from(await jpeg(page, 30), "base64");
  const copy = join(archive, "frame copy.jpg");
  writeFileSync(join(archive, "frame.jpg"), data);
  writeFileSync(copy, data);
  writeFileSync(join(archive, "frame copy.xmp"), "our edits");
  await job(request, "index", { roots: [archive], min_size: 0 });
  await job(request, "families", {});
  await reviewed(request, "plan-apply", { roles: ["copy"] });
  expect(existsSync(copy)).toBe(false);
  // Another program puts its own file where the copy belonged.
  writeFileSync(copy, NEWER);

  await page.reload();
  await main(page)
    .getByRole("button", { name: "Restore", exact: true })
    .first()
    .click();
  const dialog = page
    .getByRole("dialog")
    .filter({ has: page.getByTestId("conflicts") });
  const conflicts = dialog.getByTestId("conflicts");
  await expect(conflicts).toBeVisible();
  await expect(
    conflicts.getByRole("heading", { name: "The original place is taken" }),
  ).toBeVisible();
  await expect(conflicts).toContainText(copy);
  // "Keep" is the default — and a decision that can be confirmed too
  // (el-14vx0 round 3); here the answer is changed before confirming.
  const keep = conflicts.getByRole("radio", {
    name: "keep it in quarantine, return it by hand later",
  });
  await expect(keep).toBeChecked();
  await expect(
    dialog.getByRole("button", { name: "Restore", exact: true }),
  ).toBeEnabled();

  await conflicts
    .getByRole("radio", {
      name: "return it as *_1 and leave the existing file alone",
    })
    .check();
  await expect(
    dialog.getByRole("button", { name: "Restore", exact: true }),
  ).toBeEnabled();
  await dialog.getByRole("button", { name: "Restore", exact: true }).click();
  await page.getByRole("button", { name: "Run the plan", exact: true }).click();

  const back = join(archive, "frame copy_1.jpg");
  await expect.poll(() => existsSync(back)).toBe(true);
  expect(readFileSync(back).equals(data)).toBe(true);
  expect(readFileSync(join(archive, "frame copy_1.xmp"), "utf8")).toBe(
    "our edits",
  );
  expect(readFileSync(copy, "utf8")).toBe(NEWER);
});

test("apply to all answers every remaining conflict of a run with the same choice", async ({
  page,
  request,
}) => {
  const archive = await archiveOf(request, "conflict-all");
  await page.goto("/#journal");
  const names = ["one.jpg", "two.jpg"];
  for (const [i, name] of names.entries())
    writeFileSync(
      join(archive, name),
      Buffer.from(await jpeg(page, 70 + i * 90), "base64"),
    );
  await job(request, "index", { roots: [archive], min_size: 0 });
  await job(request, "families", {});
  const sorted = join(archive, "sorted");
  mkdirSync(sorted);
  const plan = await reviewed(request, "organize-apply", { root: sorted });
  expect(plan.items.length).toBe(2);
  for (const name of names) {
    await expect.poll(() => existsSync(join(archive, name))).toBe(false);
    writeFileSync(join(archive, name), NEWER);
  }

  await page.reload();
  await main(page)
    .getByRole("button", { name: "Undo the run", exact: true })
    .first()
    .click();
  const dialog = page
    .getByRole("dialog")
    .filter({ has: page.getByTestId("conflicts") });
  const conflicts = dialog.getByTestId("conflicts");
  await expect(conflicts.locator(".conflict")).toHaveCount(2);
  const first = conflicts.locator(".conflict").first();
  await first
    .getByRole("checkbox", {
      name: "Apply this choice to all the remaining ones",
    })
    .check();
  await first
    .getByRole("radio", {
      name: "return it as *_1 and leave the existing file alone",
    })
    .check();
  await expect(
    conflicts.locator(".conflict").nth(1).getByRole("radio", {
      name: "return it as *_1 and leave the existing file alone",
    }),
  ).toBeChecked();
  await dialog.getByRole("button", { name: "Restore", exact: true }).click();
  await page.getByRole("button", { name: "Run the plan", exact: true }).click();

  for (const name of ["one", "two"]) {
    await expect
      .poll(() => existsSync(join(archive, `${name}_1.jpg`)))
      .toBe(true);
    expect(readFileSync(join(archive, `${name}.jpg`), "utf8")).toBe(NEWER);
  }
});

// el-14vx0 round 3 (rejection el-zvg9s, R2-B4): "keep" is a decision too.
// A batch whose every choice is "keep" can be confirmed; the job writes the
// decision down and moves nothing.
test("an explicit keep-only decision can be confirmed, is recorded and moves nothing", async ({
  page,
  request,
}) => {
  const archive = await archiveOf(request, "conflict-keep");
  await page.goto("/#journal");
  const data = Buffer.from(await jpeg(page, 30), "base64");
  const copy = join(archive, "frame copy.jpg");
  writeFileSync(join(archive, "frame.jpg"), data);
  writeFileSync(copy, data);
  writeFileSync(join(archive, "frame copy.xmp"), "our edits");
  await job(request, "index", { roots: [archive], min_size: 0 });
  await job(request, "families", {});
  await reviewed(request, "plan-apply", { roles: ["copy"] });
  expect(existsSync(copy)).toBe(false);
  writeFileSync(copy, NEWER);

  await page.reload();
  await main(page)
    .getByRole("button", { name: "Restore", exact: true })
    .first()
    .click();
  const dialog = page
    .getByRole("dialog")
    .filter({ has: page.getByTestId("conflicts") });
  const conflicts = dialog.getByTestId("conflicts");
  await expect(conflicts).toBeVisible();
  await conflicts
    .getByRole("radio", {
      name: "return it as *_1 and leave the existing file alone",
    })
    .check();
  const keep = conflicts.getByRole("radio", {
    name: "keep it in quarantine, return it by hand later",
  });
  await keep.check();
  await expect(keep).toBeChecked();
  const restore = dialog.getByRole("button", { name: "Restore", exact: true });
  await expect(restore).toBeEnabled();
  await restore.click();
  await page.getByRole("button", { name: "Run the plan", exact: true }).click();

  await expect
    .poll(async () => (await (await request.get("/api/jobs")).json())[0]?.state)
    .toBe("done");
  const jobs = JSON.stringify((await (await request.get("/api/jobs")).json())[0]);
  expect(jobs).toContain("kept in quarantine");
  expect(readFileSync(copy, "utf8")).toBe(NEWER);
  expect(existsSync(join(archive, "frame copy_1.jpg"))).toBe(false);
  expect(existsSync(join(archive, "frame copy.xmp"))).toBe(false);
});
