import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

// Non-round totals expose number wrapping that an empty archive cannot.
const status = {
  files: 78241,
  images: 74618,
  skipped: 3623,
  families: 51807,
  families_multi: 12936,
  series: 487,
  categorised: 74618,
  roles: [{ role: "copy", count: 14312, bytes: 29438571264 }],
  derived_removable_bytes: 73484218368,
  quarantined_bytes: 4236247040,
  version: "0.4.1",
};

test.beforeEach(async ({ page }) => {
  await page.route("**/api/status", (route) => route.fulfill({ json: status }));
  await page.route("**/api/jobs", (route) => route.fulfill({ json: [] }));
  await page.route("**/api/journal?*", (route) => route.fulfill({ json: [] }));
  await page.route("**/api/catalogs", (route) => route.fulfill({ json: [] }));
});

test("mobile navigation opens by keyboard, closes on Escape and leads to each screen", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  const menu = page.getByRole("button", { name: "Main navigation" });
  const nav = page.getByRole("navigation", { name: "Main navigation" });
  await expect(nav).toBeHidden();
  await menu.focus();
  await page.keyboard.press("Enter");
  await expect(menu).toHaveAttribute("aria-expanded", "true");
  await page.keyboard.press("Tab");
  await page.keyboard.press("Escape");
  await expect(nav).toBeHidden();
  await expect(menu).toBeFocused();
  for (const [route, title] of [
    ["setup", "Inventory and index"],
    ["tree", "Archive tree"],
    ["families", "Duplicates and versions"],
    ["series", "Bursts"],
    ["categories", "Kinds"],
    ["plan", "Plan and move"],
    ["organize", "Sort by date"],
    ["derived", "Previews and caches"],
    ["quarantine", "Quarantine"],
    ["journal", "Journal and runs"],
    ["settings", "Settings"],
    ["overview", "Archive overview"],
  ]) {
    await menu.click();
    await nav.getByRole("link", { name: title, exact: true }).click();
    await expect(page).toHaveURL(new RegExp(`#${route}$`));
    await expect(page.locator("h1")).toHaveText(title);
    await expect(nav).toBeHidden();
    await expect(page.locator("main")).toBeFocused();
  }
});

test("overview totals and controls fit narrow screens in both languages and themes", async ({
  page,
}) => {
  test.setTimeout(120000);
  for (const language of ["en", "ru"]) {
    await page.addInitScript(
      (lang) => localStorage.setItem("pc-lang", lang),
      language,
    );
    await page.goto("/");
    await expect(page.locator(".archive-metrics dd").first()).toContainText(
      /74\D?618/,
    );
    for (const width of [320, 768, 1024, 1440]) {
      await page.setViewportSize({ width, height: 1000 });
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
      for (const theme of ["light", "dark"]) {
        await page.evaluate((value) => {
          document.documentElement.dataset.theme = value;
        }, theme);
        const result = await new AxeBuilder({ page })
          .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
          .analyze();
        expect(
          result.violations.map((violation) => ({
            id: violation.id,
            targets: violation.nodes.map((node) => node.target),
          })),
        ).toEqual([]);
      }
    }
  }
});

test("theme toggle reverses the effective system theme and keeps the choice after reload", async ({
  page,
}) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.addInitScript(() => {
    if (!localStorage.getItem("pc-theme"))
      localStorage.setItem("pc-theme", "system");
  });
  await page.goto("/");
  await expect(page.locator(".archive-metrics")).toBeVisible();
  await page.getByRole("button", { name: "Switch theme" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  expect(await page.evaluate(() => localStorage.getItem("pc-theme"))).toBe(
    "light",
  );
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.getByRole("button", { name: "Switch theme" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
});
