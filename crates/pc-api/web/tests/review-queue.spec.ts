import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

async function fixture(page: Page) {
  const decisions = new Map<number, string>();
  const operations: [number, string | undefined][][] = [];
  const writes: number[] = [];
  let fail = false;
  const member = (id: number, keeper: boolean) => ({
    file_id: id * 2 + (keeper ? 0 : 1),
    name: `DSC${String(id).padStart(5, "0")}.jpg`,
    path: `/archive/${keeper ? "originals" : "copies"}/DSC${id}.jpg`,
    dir: `/archive/${keeper ? "originals" : "copies"}`,
    size: 7832914,
    width: 4928,
    height: 3264,
    role: keeper ? "original" : "copy",
    is_keeper: keeper,
    is_rejected: false,
    thumb: null,
  });
  await page.route("**/api/review**", async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() === "GET") {
      const queue = url.searchParams.get("queue") || "pending",
        search = url.searchParams.get("search") || "";
      const ids = Array.from({ length: 10000 }, (_, i) => i + 1).filter(
        (id) =>
          (queue === "all" || (decisions.get(id) || "pending") === queue) &&
          (!search || `DSC${String(id).padStart(5, "0")}.jpg`.includes(search)),
      );
      const offset = Math.min(
        Number(url.searchParams.get("offset")),
        Math.max(0, ids.length - 1),
      );
      const counts = {
        pending: 10000 - decisions.size,
        plan: 0,
        keep: 0,
        defer: 0,
      };
      for (const state of decisions.values())
        counts[state as "plan" | "keep" | "defer"]++;
      return route.fulfill({
        json: {
          total: ids.length,
          offset,
          counts,
          groups: ids.slice(offset, offset + 50).map((id) => ({
            id,
            review_state: decisions.get(id) || "pending",
            review_token: String(id),
            exact: id !== 9999,
            can_plan: id !== 9999,
            review_reasons: [],
            members: [member(id, true), member(id, false)],
          })),
        },
      });
    }
    if (fail)
      return route.fulfill({
        status: 409,
        json: { error: "The group changed; refresh the queue" },
      });
    const body = route.request().postDataJSON();
    if (url.pathname.endsWith("batch-preview"))
      return route.fulfill({
        json: { token: "batch", groups: 2, files: 2, bytes: 15665828 },
      });
    if (url.pathname.endsWith("undo")) {
      for (const [id, previous] of operations[body.operation - 1]) {
        if (previous) decisions.set(id, previous);
        else decisions.delete(id);
      }
      return route.fulfill({ json: { ok: true } });
    }
    const ids = url.pathname.endsWith("batch")
      ? [100, 101]
      : [Number(url.pathname.split("/").pop())];
    operations.push(ids.map((id) => [id, decisions.get(id)]));
    for (const id of ids) {
      writes.push(id);
      decisions.set(id, body.state || "plan");
    }
    return route.fulfill({ json: { operation: operations.length } });
  });
  await page.route("**/api/jobs", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/file/*/preview", (r) =>
    r.fulfill({ status: 404, body: "No preview" }),
  );
  await page.goto("/#families");
  await expect(page.locator(".queue-group")).toHaveCount(3);
  return {
    writes,
    decisions,
    setFail: (value: boolean) => {
      fail = value;
    },
  };
}

test("10000 groups keep card order and keyboard navigation crosses page boundaries", async ({
  page,
}) => {
  await fixture(page);
  const cards = page.locator(".queue-group strong"),
    before = await cards.allTextContents();
  await page.locator(".queue-group").nth(2).click();
  expect(await cards.allTextContents()).toEqual(before);
  await expect(page.locator(".queue-photo-header h2")).toHaveText(
    "DSC00003.jpg",
  );
  await page.keyboard.press("j");
  await expect(page.locator(".queue-photo-header h2")).toHaveText(
    "DSC00004.jpg",
  );
  await page.getByRole("spinbutton", { name: "Group position" }).fill("50");
  await expect(page.locator(".queue-photo-header h2")).toHaveText(
    "DSC00050.jpg",
  );
  await page.getByRole("spinbutton").blur();
  await page.keyboard.press("j");
  await expect(page.locator(".queue-photo-header h2")).toHaveText(
    "DSC00051.jpg",
  );
  await page.keyboard.press("k");
  await expect(page.locator(".queue-photo-header h2")).toHaveText(
    "DSC00050.jpg",
  );
  await page.getByRole("spinbutton").fill("9999");
  await expect(page.locator(".queue-photo-header h2")).toHaveText(
    "DSC09999.jpg",
  );
  await expect(
    page.getByRole("button", { name: /Add copies to plan A/ }),
  ).toBeDisabled();
});

test("choices and undo preserve selection; typing, errors and dialogs cannot trigger decisions", async ({
  page,
}) => {
  const f = await fixture(page);
  await page.locator(".queue-group").nth(2).click();
  const before = await page.locator(".queue-group strong").allTextContents();
  await page.keyboard.press("s");
  await expect(page.locator(".queue-photo-header h2")).toHaveText(
    "DSC00004.jpg",
  );
  expect(f.writes).toEqual([3]);
  await page.keyboard.press("ControlOrMeta+z");
  await expect(page.locator(".queue-photo-header h2")).toHaveText(
    "DSC00003.jpg",
  );
  expect(await page.locator(".queue-group strong").allTextContents()).toEqual(
    before,
  );
  const search = page.getByRole("searchbox");
  await search.fill("DSC00003");
  await search.press("a");
  expect(f.writes).toEqual([3]);
  await search.fill("");
  await search.blur();
  await expect(page.locator(".queue-group")).toHaveCount(3);
  await page.evaluate(() =>
    document.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "s",
        code: "KeyS",
        repeat: true,
        bubbles: true,
      }),
    ),
  );
  expect(f.writes).toEqual([3]);
  f.setFail(true);
  await page.keyboard.press("s");
  await expect(page.getByRole("alert")).toContainText("The group changed");
  await expect(page.locator(".queue-photo-header h2")).toHaveText(
    "DSC00001.jpg",
  );
  f.setFail(false);
  await page
    .getByRole("button", { name: "Batch exact copies", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("s");
  expect(f.writes).toEqual([3]);
  await page
    .getByRole("button", { name: "Add batch to plan", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: /Undo decision/ }),
  ).toBeEnabled();
  expect(f.decisions.size).toBe(2);
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => f.decisions.size).toBe(0);
});

test("review comparison is accessible and fits mobile, tablet and desktop", async ({
  page,
}, info) => {
  test.setTimeout(120000);
  await fixture(page);
  for (const width of [320, 768, 1024, 1440]) {
    await page.setViewportSize({ width, height: 1000 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
  }
  await page.keyboard.press("z");
  await expect(page.locator(".queue-photos")).toHaveClass(/zoom/);
  for (const theme of ["light", "dark"]) {
    await page.evaluate((theme) => {
      document.documentElement.dataset.theme = theme;
    }, theme);
    const result = await new AxeBuilder({ page })
      .include(".review-queue")
      .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
      .analyze();
    expect(
      result.violations.map((v) => ({
        id: v.id,
        nodes: v.nodes.map((n) => n.target),
      })),
    ).toEqual([]);
    await page.screenshot({
      path: `test-results/queue-${theme}-${info.project.name}.png`,
      fullPage: true,
    });
  }
});
