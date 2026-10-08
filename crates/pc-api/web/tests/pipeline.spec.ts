import { test, expect, main, idle } from "./isolated";
import { mkdirSync, writeFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";

// A complete Finder .DS_Store holding no records: the buddy allocator, its
// root block with the address table and the DSDB table of contents, the
// DSDB header and an empty leaf; the same bytes as pc-core's
// derived/fixtures.rs. Well-formed, and still never moved (el-2rpxq).
function dsStore(): Buffer {
  const f = Buffer.alloc(4 + 0x1000);
  f.writeUInt32BE(1, 0);
  f.write("Bud1", 4, "latin1");
  f.writeUInt32BE(0x800, 8);
  f.writeUInt32BE(0x800, 12);
  f.writeUInt32BE(0x800, 16);
  Buffer.from([
    0, 0, 0x10, 0x0c, 0, 0, 0, 0x87, 0, 0, 0x20, 0x0b, 0, 0, 0, 0,
  ]).copy(f, 20);
  const root = 4 + 0x800;
  const addrs = [0x800 | 11, 0x20 | 5, 0x40 | 6];
  f.writeUInt32BE(addrs.length, root);
  addrs.forEach((a, i) => f.writeUInt32BE(a, root + 8 + 4 * i));
  let p = root + 8 + 256 * 4;
  f.writeUInt32BE(1, p);
  f.writeUInt8(4, p + 4);
  f.write("DSDB", p + 5, "latin1");
  f.writeUInt32BE(1, p + 9);
  [2, 0, 0, 1, 0x1000].forEach((x, i) => f.writeUInt32BE(x, 4 + 0x20 + 4 * i));
  return f;
}
test("real archive goes through scan, index, review, quarantine, undo and organization in the browser", async ({
  page,
  request,
}) => {
  test.setTimeout(90000);
  const settings = await (await request.get("/api/settings")).json();
  const base = dirname(settings.db_path);
  // The server is this test's own (see isolated.ts), so its directory is too.
  const archive = join(base, "archive"),
    out = join(base, "output");
  mkdirSync(archive);
  mkdirSync(out);
  mkdirSync(join(archive, "Backup"));
  // Lightroom's: listed with its reason, never moved (el-126jk).
  mkdirSync(join(archive, "Example Previews.lrdata"));
  writeFileSync(
    join(archive, "Example Previews.lrdata", "cache"),
    Buffer.alloc(12345),
  );
  // System junk, well-formed: "Previews and caches" moves none of it and
  // says why (el-126jk, el-2rpxq).
  mkdirSync(join(archive, "@eaDir"));
  writeFileSync(join(archive, "@eaDir", ".DS_Store"), dsStore());
  await page.goto("/#setup");
  const data = await page.evaluate(() => {
    const c = document.createElement("canvas");
    c.width = 480;
    c.height = 320;
    const ctx = c.getContext("2d")!;
    for (let y = 0; y < c.height; y += 4)
      for (let x = 0; x < c.width; x += 4) {
        ctx.fillStyle = `rgb(${(x * 13 + y * 7) % 256},${(x * 3 + y * 19) % 256},${(x * 23 + y * 5) % 256})`;
        ctx.fillRect(x, y, 4, 4);
      }
    return c.toDataURL("image/jpeg", 0.94).split(",")[1];
  });
  writeFileSync(
    join(archive, "20190714_183200.jpg"),
    Buffer.from(data, "base64"),
  );
  writeFileSync(
    join(archive, "Backup", "20190714_183200.jpg"),
    Buffer.from(data, "base64"),
  );
  writeFileSync(join(archive, "20190714_183200.xmp"), "<xmp/>");
  writeFileSync(join(archive, "Backup", "20190714_183200.xmp"), "<xmp/>");
  // Use just this project's fixture; no real archive is ever indexed by tests.
  await request.put("/api/settings", { data: { roots: [], min_size: 0 } });
  await page.reload();
  await page
    .getByRole("textbox", { name: "Archive root", exact: true })
    .fill(archive);
  await main(page).getByRole("button", { name: "Add", exact: true }).click();
  // Done means the server finished *and* the page has seen it: until the next
  // jobs poll the page keeps its actions disabled and the topbar still shows
  // a button named after the job.
  const finished = async () => {
    await expect
      .poll(
        async () => (await (await request.get("/api/jobs")).json())[0]?.state,
      )
      .toBe("done");
    await idle(page);
  };
  const run = async (label: string) => {
    await Promise.all([
      page.waitForResponse(
        (r) =>
          r.url().endsWith("/api/jobs") &&
          r.request().method() === "POST" &&
          r.status() === 202,
      ),
      main(page).getByRole("button", { name: label, exact: true }).click(),
    ]);
    await finished();
    await page.reload();
  };
  await run("Start the inventory");
  await page
    .getByRole("spinbutton", { name: "Smallest file, bytes" })
    .fill("0");
  await run("Start indexing");
  await run("Build everything");
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "Duplicates and versions", exact: true })
    .click();
  await expect(page.locator(".queue-file")).toHaveCount(2);
  await page.screenshot({
    path: test.info().outputPath("families.png"),
    fullPage: true,
  });

  await page
    .getByRole("combobox", { name: "Queue", exact: true })
    .selectOption("pending");
  await main(page)
    .getByRole("button", { name: /Defer D/ })
    .click();
  await expect(
    page.getByRole("heading", { name: "No groups in this queue" }),
  ).toBeVisible();
  await page.reload();
  await page
    .getByRole("combobox", { name: "Queue", exact: true })
    .selectOption("defer");
  await expect(page.locator(".queue-file")).toHaveCount(2);
  await main(page)
    .getByRole("button", { name: /Add copies to plan A/ })
    .click();
  await expect(
    page.getByRole("heading", { name: "No groups in this queue" }),
  ).toBeVisible();
  expect(existsSync(join(archive, "Backup", "20190714_183200.jpg"))).toBe(true);

  await page
    .getByRole("navigation")
    .getByRole("link", { name: "Plan and move", exact: true })
    .click();
  await expect(page.locator(".plan-row")).toHaveCount(1);
  expect(existsSync(join(archive, "Backup", "20190714_183200.jpg"))).toBe(true);
  await main(page)
    .getByRole("button", { name: "Move to quarantine", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Run the plan", exact: true })
    .click();
  await expect
    .poll(() => existsSync(join(archive, "Backup", "20190714_183200.jpg")))
    .toBe(false);
  await finished();
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "Quarantine", exact: true })
    .click();
  await main(page)
    .getByRole("button", { name: "Restore", exact: true })
    .first()
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Restore", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .last()
    .getByRole("button", { name: "Run the plan", exact: true })
    .click();
  await expect
    .poll(() => existsSync(join(archive, "Backup", "20190714_183200.jpg")))
    .toBe(true);
  await page.reload();
  await finished();
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "Sort by date", exact: true })
    .click();
  await page.getByPlaceholder("For example, /home/name/Pictures").fill(out);
  await expect(page.getByRole("alert")).toContainText(
    "Resolve the exact copies",
  );
  await page.getByText("Extra permission", { exact: true }).click();
  await page
    .getByRole("checkbox", {
      name: "Allow sorting with duplicates still unresolved",
    })
    .check();
  await expect(
    main(page).getByRole("button", { name: "Sort by date", exact: true }),
  ).toBeEnabled();
  await main(page)
    .getByRole("button", { name: "Sort by date", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Run the plan", exact: true })
    .click();
  await finished();
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "Journal and runs", exact: true })
    .click();
  await main(page)
    .getByRole("button", { name: "Undo the run", exact: true })
    .first()
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Restore", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .last()
    .getByRole("button", { name: "Run the plan", exact: true })
    .click();
  await expect
    .poll(() => existsSync(join(archive, "20190714_183200.jpg")))
    .toBe(true);
  await finished();
  expect(existsSync(join(archive, "20190714_183200.xmp"))).toBe(true);
  expect(existsSync(join(archive, "Backup", "20190714_183200.xmp"))).toBe(true);
  await page.reload();
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "Plan and move", exact: true })
    .click();
  const companions = page.locator(".explorer-detail summary");
  await expect(companions).toHaveText("Companions · 1");
  await companions.click();
  await expect(page.locator(".explorer-companion")).toContainText(
    "20190714_183200.xmp",
  );
  await main(page)
    .getByRole("button", { name: "Move to quarantine", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Run the plan", exact: true })
    .click();
  await finished();
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "Previews and caches", exact: true })
    .click();
  await expect(
    main(page).getByText(
      /Lightroom is never touched: Example Previews\.lrdata/,
    ),
  ).toBeVisible();
  await main(page)
    .getByRole("button", { name: "Plan preview", exact: true })
    .click();
  await expect(
    main(page).getByText(/derived clean moves no system files/).first(),
  ).toBeVisible();
  await expect(
    main(page).getByRole("button", { name: "Move to quarantine", exact: true }),
  ).toBeDisabled();
  // The purge preview is sealed by a token over its items *and* refusals.
  // An entry moved within the current second is still refused as "holding
  // period not passed"; a second later it is past.
  // Waiting only for the two files to count let the page's preview and the
  // job straddle that second — the job then refused, fail-closed, as a
  // changed plan. So wait until nothing is held back any more.
  await expect
    .poll(async () => {
      const p = await (
        await request.post("/api/preview", {
          data: { kind: "derived-purge", params: { older_than_secs: 0 } },
        })
      ).json();
      const held = p.refusals.some((r: { why: string }) =>
        r.why.includes("holding period"),
      );
      return held ? -1 : p.total_files;
    })
    // The copy and its sidecar.
    .toBe(2);
  const journal = await (await request.get("/api/journal")).json();
  const destinations = journal
    .filter((j) => j.status === "done" && j.op.startsWith("quarantine"))
    .map((j) => j.dst);
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "Quarantine", exact: true })
    .click();
  await page
    .getByRole("spinbutton", { name: "Held for at least, days" })
    .fill("0");
  await main(page)
    .getByRole("button", { name: "Check before deleting", exact: true })
    .click();
  await main(page)
    .getByRole("button", { name: "Delete for good", exact: true })
    .click();
  const purge = page.getByRole("dialog");
  await purge.getByRole("checkbox").check();
  await purge.getByRole("textbox").fill("DELETE");
  await purge
    .getByRole("button", { name: "Delete for good", exact: true })
    .click();
  await finished();
  expect(destinations.some((d) => d.endsWith("@eaDir"))).toBe(false);
  expect(existsSync(join(archive, "@eaDir", ".DS_Store"))).toBe(true);
  for (const dst of destinations) {
    expect(existsSync(dst)).toBe(false);
    if (dst.endsWith(".jpg"))
      expect(existsSync(dst.replace(/\.jpg$/, ".xmp"))).toBe(false);
  }
  expect(existsSync(join(archive, "Example Previews.lrdata", "cache"))).toBe(
    true,
  );
  expect(existsSync(join(archive, "20190714_183200.jpg"))).toBe(true);
  expect(existsSync(join(archive, "20190714_183200.xmp"))).toBe(true);
});
