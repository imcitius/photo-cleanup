import { test, expect } from "@playwright/test";
import { mkdirSync, writeFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
// The archive tree, driven the way somebody with a NAS full of photographs
// drives it: open the folder the originals are in, say so once, and let the
// copies elsewhere go. Nothing here is mocked — a real server, a real index
// and two real files, one of which is a byte-for-byte copy of the other.
test("marking a folder as the originals clears its copies from everywhere else", async ({
  page,
  request,
}, info) => {
  test.setTimeout(90000);
  await request.post("/api/reset", { data: { confirmation: "RESET" } });
  const settings = await (await request.get("/api/settings")).json();
  const archive = join(dirname(settings.db_path), `tree-${info.project.name}`);
  const shots = join(archive, "shots", "2014");
  const mirror = join(archive, "mirror");
  mkdirSync(shots, { recursive: true });
  mkdirSync(mirror, { recursive: true });
  await page.goto("/#tree");
  const data = await page.evaluate(() => {
    const c = document.createElement("canvas");
    c.width = 480;
    c.height = 320;
    const ctx = c.getContext("2d")!;
    for (let y = 0; y < c.height; y += 4)
      for (let x = 0; x < c.width; x += 4) {
        ctx.fillStyle = `rgb(${(x * 11 + y * 5) % 256},${(x * 7 + y * 17) % 256},${(x * 19 + y * 3) % 256})`;
        ctx.fillRect(x, y, 4, 4);
      }
    return c.toDataURL("image/jpeg", 0.94).split(",")[1];
  });
  const copy = join(mirror, "20190714_183200.jpg");
  writeFileSync(
    join(shots, "20190714_183200.jpg"),
    Buffer.from(data, "base64"),
  );
  writeFileSync(copy, Buffer.from(data, "base64"));
  await request.put("/api/settings", { data: { roots: [archive] } });
  await request.post("/api/jobs", {
    data: { kind: "all", params: { roots: [archive], min_size: 0 } },
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
  // Down to the archive: a chain of folders holding nothing but one another
  // is one entry, so this is a single press however deep the mount point is.
  await page.locator(".tree-open").first().click();
  const row = page.locator(".tree-row", { hasText: "shots" });
  await expect(row).toHaveCount(1);
  await row.getByRole("button", { name: "The originals are here" }).click();
  // The numbers are the answer to "did that do anything?", and there is one
  // group in this archive with a file in the marked tree.
  await expect(page.getByText("Groups holding a file here: 1")).toBeVisible();

  expect(existsSync(copy)).toBe(true);
  await page
    .getByRole("button", { name: "Move to quarantine", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Run the plan", exact: true })
    .click();
  await expect.poll(() => existsSync(copy)).toBe(false);
  expect(existsSync(join(shots, "20190714_183200.jpg"))).toBe(true);

  // And the mark is still there afterwards, because it is a rule about the
  // archive rather than a press that has been spent.
  await page.reload();
  await expect(page.locator(".mark-row")).toHaveCount(1);
  await page
    .getByRole("button", { name: "Take the mark back" })
    .first()
    .click();
  await expect(page.getByText("No folder is marked yet")).toBeVisible();
});
