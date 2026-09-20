import { test, expect } from "@playwright/test";
import { mkdirSync, writeFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
test("real archive goes through scan, index, review, quarantine, undo and organization in the browser", async ({
  page,
  request,
}, info) => {
  test.setTimeout(90000);
  const settings = await (await request.get("/api/settings")).json();
  const base = dirname(settings.db_path);
  const archive = join(base, `archive-${info.project.name}`),
    out = join(base, `output-${info.project.name}`);
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
    .getByRole("textbox", { name: "Корень архива", exact: true })
    .fill(archive);
  await page.getByRole("button", { name: "Добавить", exact: true }).click();
  const run = async (label: string) => {
    await Promise.all([
      page.waitForResponse(
        (r) =>
          r.url().endsWith("/api/jobs") &&
          r.request().method() === "POST" &&
          r.status() === 202,
      ),
      page.getByRole("button", { name: label, exact: true }).click(),
    ]);
    await expect
      .poll(async () => {
        const jobs = await (await request.get("/api/jobs")).json();
        return jobs[0]?.state;
      })
      .toBe("done");
    await page.reload();
  };
  await run("Начать опись");
  await page
    .getByRole("spinbutton", { name: "Минимальный размер, байт" })
    .fill("0");
  await run("Начать индексацию");
  await run("Построить всё");
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "Дубликаты и версии", exact: true })
    .click();
  await expect(page.locator(".member")).toHaveCount(2);
  await page.screenshot({
    path: `test-results/families-${info.project.name}.png`,
    fullPage: true,
  });
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "План и перенос", exact: true })
    .click();
  await expect(page.locator(".plan-row")).toHaveCount(1);
  expect(existsSync(join(archive, "Backup", "20190714_183200.jpg"))).toBe(true);
  await page
    .getByRole("button", { name: "Перенести в карантин", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Выполнить план", exact: true })
    .click();
  await expect
    .poll(() => existsSync(join(archive, "Backup", "20190714_183200.jpg")))
    .toBe(false);
  await expect
    .poll(async () => {
      const jobs = await (await request.get("/api/jobs")).json();
      return jobs[0]?.state;
    })
    .toBe("done");
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "Карантин", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Восстановить", exact: true })
    .first()
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Восстановить", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .last()
    .getByRole("button", { name: "Выполнить план", exact: true })
    .click();
  await expect
    .poll(() => existsSync(join(archive, "Backup", "20190714_183200.jpg")))
    .toBe(true);
  await page.reload();
  await expect
    .poll(async () => {
      const jobs = await (await request.get("/api/jobs")).json();
      return jobs[0]?.state;
    })
    .toBe("done");
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "Раскладка по датам", exact: true })
    .click();
  await page.getByPlaceholder("Например, /home/имя/Фотографии").fill(out);
  await expect(page.getByRole("alert")).toContainText(
    "Сначала разберите точные копии",
  );
  await page.getByText("Дополнительное разрешение", { exact: true }).click();
  await page
    .getByRole("checkbox", {
      name: "Разрешить раскладку неразобранных дубликатов",
    })
    .check();
  await expect(
    page.getByRole("button", { name: "Разложить по датам", exact: true }),
  ).toBeEnabled();
  await page
    .getByRole("button", { name: "Разложить по датам", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Выполнить план", exact: true })
    .click();
  await expect
    .poll(async () => {
      const jobs = await (await request.get("/api/jobs")).json();
      return jobs[0]?.state;
    })
    .toBe("done");
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "Журнал и прогоны", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Откатить прогон", exact: true })
    .first()
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Восстановить", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .last()
    .getByRole("button", { name: "Выполнить план", exact: true })
    .click();
  await expect
    .poll(() => existsSync(join(archive, "20190714_183200.jpg")))
    .toBe(true);
  await expect
    .poll(async () => (await (await request.get("/api/jobs")).json())[0]?.state)
    .toBe("done");
  expect(existsSync(join(archive, "20190714_183200.xmp"))).toBe(true);
  expect(existsSync(join(archive, "Backup", "20190714_183200.xmp"))).toBe(true);
  await page.reload();
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "План и перенос", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Спутники · 1", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Перенести в карантин", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Выполнить план", exact: true })
    .click();
  await expect
    .poll(async () => (await (await request.get("/api/jobs")).json())[0]?.state)
    .toBe("done");
  await page
    .getByRole("navigation")
    .getByRole("link", { name: "Превью и кэши", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Предпросмотр плана", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Перенести в карантин", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Выполнить план", exact: true })
    .click();
  await expect
    .poll(async () => (await (await request.get("/api/jobs")).json())[0]?.state)
    .toBe("done");
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
    .getByRole("link", { name: "Карантин", exact: true })
    .click();
  await page
    .getByRole("spinbutton", { name: "Хранятся не менее, дней" })
    .fill("0");
  await page
    .getByRole("button", { name: "Проверить перед удалением", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Удалить навсегда", exact: true })
    .click();
  const purge = page.getByRole("dialog");
  await purge.getByRole("checkbox").check();
  await purge.getByRole("textbox").fill("УДАЛИТЬ");
  await purge
    .getByRole("button", { name: "Удалить навсегда", exact: true })
    .click();
  await expect
    .poll(async () => (await (await request.get("/api/jobs")).json())[0]?.state)
    .toBe("done");
  for (const dst of destinations) {
    expect(existsSync(dst)).toBe(false);
    if (dst.endsWith(".jpg"))
      expect(existsSync(dst.replace(/\.jpg$/, ".xmp"))).toBe(false);
  }
  expect(existsSync(join(archive, "20190714_183200.jpg"))).toBe(true);
  expect(existsSync(join(archive, "20190714_183200.xmp"))).toBe(true);
});
