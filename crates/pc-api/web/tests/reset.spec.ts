import { test, expect } from "@playwright/test";
import { existsSync } from "node:fs";
// A reset of the index deletes nothing from the thumbnail cache: it starts a
// new cache folder and says where the old one is and how big, for the user
// to delete by hand (el-5x1uh, review el-19kbm).
test("a reset keeps the old thumbnail cache and says so", async ({
  page,
  request,
}) => {
  // The first reset makes a generation; the second keeps it as old cache.
  const first = await (
    await request.post("/api/reset", { data: { confirmation: "RESET" } })
  ).json();
  expect(first.thumbs_removed).toBe(0);
  await page.goto("/#settings");
  await page.getByPlaceholder("RESET").fill("RESET");
  const reply = page.waitForResponse("**/api/reset");
  await page.getByRole("button", { name: "Reset the index" }).click();
  const body = await (await reply).json();
  expect(body.thumbs_removed).toBe(0);
  expect(body.thumbs_kept).toBeGreaterThanOrEqual(1);
  // The generation the first reset made is still on disk, named as kept.
  expect(existsSync(first.thumbs_generation)).toBe(true);
  const status = page.getByText(/Old thumbnail cache kept, nothing deleted/);
  await expect(status).toBeVisible();
  await expect(status).toContainText(body.thumbs_generation);
  await expect(status).toContainText("Delete it by hand");
});
