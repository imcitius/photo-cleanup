import { test, expect } from "@playwright/test";
import { mkdirSync, writeFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
// The archive tree, driven the way somebody with an array drives it: two
// filesystems, one folder structure laid across them, and the answer given
// once for both. Nothing here is mocked — a real server, a real index, and
// real files, half of which are byte-for-byte copies of the other half.
test("one mark on the merged tree clears the copies on every disk", async ({
  page,
  request,
}, info) => {
  test.setTimeout(90000);
  await request.post("/api/reset", { data: { confirmation: "RESET" } });
  // A reset forgets the index, not the rules built on top of it, and not the
  // quarantine on disk. Both outlive this test otherwise: the next attempt
  // finds its destinations occupied and a mark it never made.
  for (const mark of (await (await request.get("/api/tree")).json()).marks) {
    await request.post("/api/originals", {
      data: { path: mark.path, scope: mark.scope, marked: false },
    });
  }
  const settings = await (await request.get("/api/settings")).json();
  // A fresh archive per attempt, so a repeated run is a run, not a retry on
  // somebody else's leftovers.
  const base = join(
    dirname(settings.db_path),
    `tree-${info.project.name}-${info.repeatEachIndex}-${info.retry}`,
  );
  const disks = ["disk1", "disk2"].map((d) => join(base, d));
  const good = "D/разобрано/даня/театр";
  for (const disk of disks) {
    mkdirSync(join(disk, good), { recursive: true });
    mkdirSync(join(disk, "D/свалка"), { recursive: true });
  }
  await page.goto("/#tree");
  // One frame per disk, so each disk is a group of its own and the numbers
  // below say how many disks the single mark actually reached.
  // Two unrelated photographs, not one in two tints: frames that merely
  // differ in colour are one shot as far as the archive is concerned, they
  // land in a single group, and then the count below would be measuring the
  // grouping rather than the mark.
  const shots = await page.evaluate(() =>
    [
      [11, 5, 7, 17, 19, 3],
      [37, 2, 3, 43, 29, 53],
      [71, 13, 23, 97, 41, 61],
    ].map(([a, b, c2, d, e, f]) => {
      const c = document.createElement("canvas");
      c.width = 480;
      c.height = 320;
      const ctx = c.getContext("2d")!;
      for (let y = 0; y < c.height; y += 4)
        for (let x = 0; x < c.width; x += 4) {
          ctx.fillStyle = `rgb(${(x * a + y * b) % 256},${(x * c2 + y * d) % 256},${(x * e + y * f) % 256})`;
          ctx.fillRect(x, y, 4, 4);
        }
      return c.toDataURL("image/jpeg", 0.94).split(",")[1];
    }),
  );
  const copies = disks.map((d) => join(d, "D/свалка/IMG.JPG"));
  disks.forEach((disk, n) => {
    const bytes = Buffer.from(shots[n], "base64");
    writeFileSync(join(disk, good, "IMG.JPG"), bytes);
    writeFileSync(copies[n], bytes);
  });

  // An unmarked group belongs to automatic suggestions, but not this handoff.
  const unrelated = join(disks[0], "Other");
  mkdirSync(unrelated, { recursive: true });
  for (const name of ["one.jpg", "two.jpg"])
    writeFileSync(join(unrelated, name), Buffer.from(shots[2], "base64"));

  await request.put("/api/settings", { data: { roots: disks } });
  await request.post("/api/jobs", {
    data: { kind: "all", params: { roots: disks, min_size: 0 } },
  });
  await expect
    .poll(
      async () => (await (await request.get("/api/jobs")).json())[0]?.state,
      { timeout: 60000 },
    )
    .toBe("done");

  // The tab has been open since before the first file existed, so it is
  // holding the answer it was given then.
  await page.reload();
  await expect(page.getByText("No folder is marked yet")).toBeVisible();
  await expect(
    page.getByRole("button", {
      name: "Review originals’ copy plan",
      exact: true,
    }),
  ).toBeDisabled();
  await expect(page.locator(".plan-explorer")).toHaveCount(0);
  // Two roots, one structure: the tree says so, and `D` is one node.
  await expect(page.getByText("The archive’s disks")).toBeVisible();
  await expect(page.locator('[data-path="D"]')).toHaveCount(1);

  for (const path of ["D", "D/разобрано", "D/разобрано/даня"]) {
    await page.locator(`[data-path="${path}"] .tree-twist`).click();
  }
  await page.locator(`[data-path="${good}"] .tree-row`).click();
  await page
    .getByRole("button", { name: "The originals are here", exact: true })
    .click();
  // The numbers are the answer to "did that do anything?", and both disks
  // have a group with a file in the marked tree.
  await expect(page.getByText("Groups holding a file here: 2")).toBeVisible();

  for (const copy of copies) expect(existsSync(copy)).toBe(true);

  // The step that matters most, and the one the first version of this test
  // did not take: rebuild the groups and ask again. A mark is a rule, and a
  // rule that only holds until the next rebuild is worth nothing — the plan
  // went silently empty, with the kept file labelled a copy of the one it
  // had replaced.
  await request.post("/api/jobs", { data: { kind: "families", params: {} } });
  await expect
    .poll(async () => (await (await request.get("/api/jobs")).json())[0]?.state)
    .toBe("done");
  const after = await (
    await request.post("/api/preview", {
      data: {
        kind: "plan-apply",
        params: { roles: ["copy"], originals: true },
      },
    })
  ).json();
  expect(after.total_files).toBe(2);
  await page.goto("/#plan");
  await page
    .getByRole("combobox", { name: "Plan scope", exact: true })
    .selectOption("reviewed");
  await page
    .getByText("Saved decisions and folder rules", { exact: true })
    .click();
  await expect(page.locator(".plan-row")).toHaveCount(2);
  await expect(
    page.getByRole("region", { name: "Your saved decisions" }),
  ).toContainText(good);
  await page.goto("/#tree");
  await page.reload();
  await expect(page.locator(".plan-explorer")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Move to quarantine", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText("Marked folders: 1.", { exact: false }),
  ).toBeVisible();
  for (const width of [320, 768, 1440]) {
    await page.setViewportSize({ width, height: 1000 });
    const step = page.getByRole("region", { name: "Next: review the copies" });
    expect(await step.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(
      true,
    );
  }
  await page.screenshot({
    path: `test-results/tree-handoff-${info.project.name}.png`,
    fullPage: true,
  });
  const preview = page.waitForRequest(
    (r) => r.url().endsWith("/api/preview") && r.method() === "POST",
  );
  await page
    .getByRole("button", { name: "Review originals’ copy plan", exact: true })
    .click();
  expect((await preview).postDataJSON().params).toMatchObject({
    originals: true,
    reviewed_only: false,
    roles: ["copy"],
    allow_lightroom: false,
  });
  await expect(page).toHaveURL(/#plan$/);
  const scope = page.getByRole("combobox", { name: "Plan scope", exact: true });
  await expect(scope).toHaveValue("originals");
  await expect(page.locator(".plan-row")).toHaveCount(2);
  await page.reload();
  await expect(scope).toHaveValue("originals");
  await expect(page.locator(".plan-row")).toHaveCount(2);
  await page
    .getByRole("button", { name: "Show the combined plan", exact: true })
    .click();
  await expect(scope).toHaveValue("all");
  await expect(page.locator(".plan-row")).toHaveCount(3);
  await scope.selectOption("originals");
  await expect(page.locator(".plan-row")).toHaveCount(2);
  await page.screenshot({
    path: `test-results/originals-plan-${info.project.name}.png`,
    fullPage: true,
  });
  await page
    .getByRole("button", { name: "Move to quarantine", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Run the plan", exact: true })
    .click();
  for (const copy of copies)
    await expect.poll(() => existsSync(copy)).toBe(false);
  for (const disk of disks) {
    expect(existsSync(join(disk, good, "IMG.JPG"))).toBe(true);
  }

  expect(existsSync(join(unrelated, "one.jpg"))).toBe(true);
  expect(existsSync(join(unrelated, "two.jpg"))).toBe(true);
  await page
    .getByRole("button", { name: "Choose originals folders", exact: true })
    .click();
  await expect(page).toHaveURL(/#tree$/);
  // One mark for the pair of them, and it survives the page: it is a rule
  // about the archive rather than a press that has been spent.
  await page.reload();
  // Scoped to the list of marks: the same row style carries the per-disk
  // breakdown of whichever folder is open, and that is not a mark.
  await expect(page.locator(".mark-list .mark-row")).toHaveCount(1);
  // The badge, not the sentence above the folder list that also contains
  // these words: which of the two is on screen depends on what the tree has
  // finished loading, and the test would flake on that alone.
  await expect(
    page.locator(".mark-list .badge", { hasText: "on every disk" }),
  ).toHaveCount(1);
  await page
    .getByRole("button", { name: "Take the mark back" })
    .first()
    .click();
  await expect(page.getByText("No folder is marked yet")).toBeVisible();
});
