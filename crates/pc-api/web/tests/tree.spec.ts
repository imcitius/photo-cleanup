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
  const settings = await (await request.get("/api/settings")).json();
  const base = join(dirname(settings.db_path), `tree-${info.project.name}`);
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

  // One mark for the pair of them, and it survives the page: it is a rule
  // about the archive rather than a press that has been spent.
  await page.reload();
  // Scoped to the list of marks: the same row style carries the per-disk
  // breakdown of whichever folder is open, and that is not a mark.
  await expect(page.locator(".mark-list .mark-row")).toHaveCount(1);
  await expect(page.getByText("on every disk")).toBeVisible();
  await page
    .getByRole("button", { name: "Take the mark back" })
    .first()
    .click();
  await expect(page.getByText("No folder is marked yet")).toBeVisible();
});
