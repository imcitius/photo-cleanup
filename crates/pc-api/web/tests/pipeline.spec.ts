import { test, expect, main, idle } from "./isolated";
import { mkdirSync, writeFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
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
  mkdirSync(join(archive, "Example Previews.lrdata"));
  writeFileSync(
    join(archive, "Example Previews.lrdata", "cache"),
    Buffer.alloc(12345),
  );
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
  await main(page)
    .getByRole("button", { name: "Plan preview", exact: true })
    .click();
  await main(page)
    .getByRole("button", { name: "Move to quarantine", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Run the plan", exact: true })
    .click();
  await finished();
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
    )
    .toBe(3);
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
  for (const dst of destinations) {
    expect(existsSync(dst)).toBe(false);
    if (dst.endsWith(".jpg"))
      expect(existsSync(dst.replace(/\.jpg$/, ".xmp"))).toBe(false);
  }
  expect(existsSync(join(archive, "20190714_183200.jpg"))).toBe(true);
  expect(existsSync(join(archive, "20190714_183200.xmp"))).toBe(true);
});
