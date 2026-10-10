import { test, expect } from "@playwright/test";
// A reset of the index does not touch the thumbnail cache at all, and says
// so plainly instead of counting removed or kept thumbnails (el-5x1uh, user
// decision 2026-10-10). That the cache's files stay byte for byte is checked
// on the real server in pc-desktop's storage tests.
test("a reset says the thumbnail cache was not touched", async ({ page }) => {
  await page.goto("/#settings");
  await expect(
    page.getByText(/The thumbnail cache is not touched: nothing in it is deleted/),
  ).toBeVisible();
  await page.getByPlaceholder("RESET").fill("RESET");
  const reply = page.waitForResponse("**/api/reset");
  await page.getByRole("button", { name: "Reset the index" }).click();
  const body = await (await reply).json();
  expect(body).toEqual({ ok: true, thumbnail_cache: "untouched" });
  await expect(
    page.getByText("Index cleared; the thumbnail cache was not touched."),
  ).toBeVisible();
});
