import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const items = Array.from({ length: 10532 }, (_, i) => ({
  file_id: i + 1,
  path: `/archive/Family/Данька/DSC${String(i).padStart(5, "0")}.ARW`,
  dst: `/archive/.quarantine/DSC${String(i).padStart(5, "0")}.ARW`,
  size: 7832914,
  file_count: 1,
  keeper_id: i + 20000,
  keeper_path: `/originals/Family/DSC${String(i).padStart(5, "0")}.ARW`,
  reason: "Exact copy of the kept file",
  role: "copy",
  thumb: null,
  keeper_thumb: null,
}));
const refusals = Array.from({ length: 381 }, (_, i) => ({
  path: `/archive/Concerts/DSC${i + 30000}.JPG`,
  why: `Lightroom protects this file ${i}`,
}));
test.beforeEach(async ({ page }) => {
  await page.route("**/api/preview", (r) =>
    r.fulfill({
      json: {
        kind: "plan-apply",
        params: { roles: ["copy"] },
        token: "test-plan",
        items,
        refusals,
        total_files: items.length,
        total_bytes: 82509230648,
      },
    }),
  );
  await page.route("**/api/jobs", (r) => r.fulfill({ json: [] }));
});
test("large plans have searchable outcomes, folder navigation and bounded refusal lists", async ({
  page,
}) => {
  await page.goto("/#plan");
  const explorer = page.getByRole("region", { name: "Explore the plan" });
  await expect(explorer.locator(".explorer-row")).toHaveCount(40);
  expect(
    (await explorer.locator(".explorer-row").first().boundingBox())!.height,
  ).toBeLessThan(160);
  const before = await explorer
    .locator(".explorer-row strong")
    .allTextContents();
  await explorer.locator(".explorer-row").nth(2).click();
  expect(
    await explorer.locator(".explorer-row strong").allTextContents(),
  ).toEqual(before);
  await expect(
    explorer.getByRole("region", { name: "What happens to this file" }),
  ).toContainText("DSC00002.ARW");
  await explorer
    .getByRole("searchbox", { name: "Find a photo or folder" })
    .fill("DSC10531");
  await expect(explorer.locator(".explorer-row").first()).toContainText(
    "DSC10531",
  );
  await explorer.getByRole("searchbox").fill("Данка DSC10531");
  await expect(explorer.locator(".explorer-row").first()).toContainText(
    "DSC10531",
  );
  await explorer.getByRole("searchbox").fill("");
  await explorer
    .getByRole("button", { name: /Refused · stays/ })
    .first()
    .click();
  await expect(explorer.locator(".explorer-row")).toHaveCount(40);
  await explorer.locator(".explorer-row").first().click();
  await expect(explorer.locator(".explorer-detail")).toContainText(
    "Lightroom protects this file",
  );
  await explorer
    .getByRole("button", { name: "Next page", exact: true })
    .click();
  await expect(explorer.locator(".explorer-row").first()).toContainText(
    "DSC30040",
  );
  await explorer.getByRole("button", { name: /All results/ }).click();
  await explorer
    .locator(".folder-children")
    .getByRole("button", { name: /archive/ })
    .click();
  await explorer
    .locator(".folder-children")
    .getByRole("button", { name: /Concerts/ })
    .click();
  await expect(
    explorer.getByText("Results: 381", { exact: true }),
  ).toBeVisible();
  await expect(explorer.locator(".explorer-row")).toHaveCount(40);
  await explorer.getByRole("searchbox").fill("not-in-this-plan");
  await expect(
    explorer.getByRole("heading", { name: "Nothing found" }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Move to quarantine", exact: true })
    .click();
  // Searching never narrows the approved operation implicitly.
  await expect(page.getByRole("dialog")).toContainText("10532");
});
test("plan browser stays within the viewport and accessible in both themes", async ({
  page,
}) => {
  test.setTimeout(120000);
  await page.goto("/#plan");
  await expect(page.locator(".explorer-row").first()).toBeVisible();
  for (const width of [320, 768, 1024, 1440]) {
    await page.setViewportSize({ width, height: 1000 });
    for (const scope of ["originals", "reviewed", "all"]) {
      await page
        .getByRole("combobox", { name: "Plan scope", exact: true })
        .selectOption(scope);
      const layout = await page.evaluate(
        (viewport) => ({
          inner: innerWidth,
          width: document.documentElement.scrollWidth,
          overflow: [...document.querySelectorAll("body *")]
            .filter((el) => {
              const rect = el.getBoundingClientRect();
              return (
                rect.width > 0 &&
                (rect.right > viewport || el.scrollWidth > el.clientWidth)
              );
            })
            .slice(0, 8)
            .map((el) => ({
              tag: el.tagName,
              className: el.className,
              width: el.getBoundingClientRect().width,
            })),
        }),
        width,
      );
      expect(layout.width, JSON.stringify(layout)).toBeLessThanOrEqual(width);
    }
  }
  await page
    .locator(".plan-explorer")
    .screenshot({ path: `test-results/plan-explorer.png` });
  for (const theme of ["light", "dark"]) {
    await page.evaluate(
      (t) => (document.documentElement.dataset.theme = t),
      theme,
    );
    const result = await new AxeBuilder({ page })
      .include(".plan-explorer")
      .include(".plan-scope-panel")
      .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
      .analyze();
    expect(
      result.violations.map((v) => ({
        id: v.id,
        nodes: v.nodes.map((n) => n.target),
      })),
    ).toEqual([]);
  }
});

test("sidecars can be found separately and selecting a photo only loads its pair", async ({
  page,
}) => {
  await page.route("**/api/preview", (r) =>
    r.fulfill({
      json: {
        kind: "plan-apply",
        params: { roles: ["copy"] },
        token: "sidecars",
        items: [
          {
            ...items[0],
            companions: [
              {
                path: "/archive/Family/Данька/DSC00000.xmp",
                dst: "/archive/.quarantine/DSC00000.xmp",
                size: 1873,
              },
            ],
          },
        ],
        refusals: [],
        total_files: 2,
        total_bytes: 7834787,
      },
    }),
  );
  const images: string[] = [];
  await page.route("**/api/file/*/preview", (r) => {
    images.push(r.request().url());
    return r.fulfill({ status: 404, body: "Missing preview" });
  });
  await page.goto("/#plan");
  await expect(page.locator(".review-photo-error")).toHaveCount(2);
  expect(images).toHaveLength(2);
  await page
    .getByRole("searchbox", { name: "Find a photo or folder" })
    .fill("DSC00000.xmp");
  await expect(page.locator(".explorer-row")).toHaveCount(1);
  await expect(page.locator(".explorer-detail")).toContainText(
    "/archive/.quarantine/DSC00000.xmp",
  );
  await expect(page.locator(".explorer-detail")).toContainText(
    "Moves with the photo DSC00000.ARW",
  );
});

test("the combined plan is the default and queue navigation clears a limited scope", async ({
  page,
}, info) => {
  await page.addInitScript(() =>
    sessionStorage.setItem("pc-reviewed-plan", "true"),
  );
  const first = page.waitForRequest((r) => r.url().endsWith("/api/preview"));
  await page.goto("/#plan");
  expect((await first).postDataJSON().params).toMatchObject({
    reviewed_only: false,
    originals: false,
    roles: ["copy"],
    allow_lightroom: false,
  });
  const scope = page.getByRole("combobox", { name: "Plan scope", exact: true });
  await expect(scope).toHaveValue("all");
  await expect(
    page.getByRole("heading", { name: "Combined cleanup plan", exact: true }),
  ).toBeVisible();
  await scope.selectOption("reviewed");
  await expect(
    page.getByRole("heading", { name: "Only my decisions", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Show the combined plan", exact: true })
    .click();
  await expect(scope).toHaveValue("all");
  await scope.selectOption("originals");
  await page.reload();
  await expect(scope).toHaveValue("originals");
  await page.goto("/#families");
  await page.getByRole("button", { name: /Open combined plan/ }).click();
  await expect(scope).toHaveValue("all");
  await expect(page.locator(".explorer-row").first()).toBeVisible();
  await page.screenshot({
    path: `test-results/combined-plan-${info.project.name}.png`,
    fullPage: true,
  });
});
