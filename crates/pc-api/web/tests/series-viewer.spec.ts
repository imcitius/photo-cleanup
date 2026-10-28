import { test, expect, type Page } from "@playwright/test";

// A burst read in the viewer is where the best frame is found. Closing the
// viewer to look for that frame again by name in the strip below was the
// complaint; the marks are set from inside the viewer, and it stays open.
const frame = (file_id: number, rank: number, taken_at: number) => ({
  file_id,
  name: `DSC0${file_id}.ARW`,
  dir: "/archive/2023/burst",
  rank,
  score: 50 - rank,
  breakdown: "",
  sharpness: 12.3,
  thumb: null,
  taken_at,
  is_best: false,
  is_rejected: false,
  family_id: null,
  family_size: 1,
  is_family_keeper: false,
});

// Playwright's keyboard has no Cyrillic keys; what the page reads is the
// `key` a Russian layout reports for the same physical key.
const russian = (page: Page, key: string) =>
  page.evaluate(
    (key) =>
      document.activeElement?.dispatchEvent(
        new KeyboardEvent("keydown", { key, bubbles: true }),
      ),
    key,
  );

async function burst(page: Page) {
  const members = [
    frame(4171, 2, 1_700_000_000),
    frame(4172, 0, 1_700_000_001),
    frame(4173, 1, 1_700_000_002),
  ];
  const posts: { path: string; body: unknown }[] = [];
  await page.route("**/api/jobs", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/file/**", (r) => r.fulfill({ status: 404 }));
  await page.route("**/api/thumb/**", (r) => r.fulfill({ status: 404 }));
  await page.route("**/api/series?*", (r) =>
    r.fulfill({
      json: {
        total: 1,
        series: [
          {
            id: 37,
            kind: "burst",
            label: "Burst",
            because: "frames a second apart",
            started_at: 1_700_000_000,
            camera: "Sony ILCE-7M3",
            protected: false,
            members,
          },
        ],
      },
    }),
  );
  // The server's answers, kept just far enough to show what the page reads
  // back after each decision.
  await page.route("**/api/series/37/best", async (r) => {
    const body = r.request().postDataJSON();
    posts.push({ path: "best", body });
    for (const m of members)
      if (body.best === false) {
        if (m.file_id === body.file_id) m.is_best = false;
      } else m.is_best = m.file_id === body.file_id;
    await r.fulfill({ json: { ok: true } });
  });
  await page.route(/\/api\/files\/\d+\/reject$/, async (r) => {
    const id = Number(
      r
        .request()
        .url()
        .match(/files\/(\d+)\//)![1],
    );
    const body = r.request().postDataJSON();
    posts.push({ path: `reject ${id}`, body });
    members.find((m) => m.file_id === id)!.is_rejected = body.rejected;
    await r.fulfill({ json: { ok: true } });
  });
  return { members, posts };
}

test("B, X and U mark the frame on screen and the viewer stays on it", async ({
  page,
}) => {
  const { members, posts } = await burst(page);
  await page.goto("/#series");
  await page
    .getByRole("button", { name: /DSC04171\.ARW/ })
    .first()
    .click();
  const viewer = page.locator("dialog[open]");
  await expect(viewer.getByText("1 / 3")).toBeVisible();

  // Second frame: the best one.
  await page.keyboard.press("ArrowRight");
  await expect(viewer.getByText("2 / 3")).toBeVisible();
  await page.keyboard.press("b");
  await expect(viewer.locator(".viewer-state")).toHaveText("★ best");
  expect(members[1].is_best).toBe(true);
  await expect(viewer.getByText("2 / 3")).toBeVisible();
  await expect(
    viewer.getByRole("button", { name: /Make it the best/ }),
  ).toBeDisabled();

  // Third: to quarantine, from the Russian layout's Ч.
  await page.keyboard.press("ArrowRight");
  await russian(page, "ч");
  await expect(viewer.locator(".viewer-state")).toHaveText(
    "marked for quarantine",
  );
  expect(members[2].is_rejected).toBe(true);
  await expect(viewer.getByText("3 / 3")).toBeVisible();

  // The strip under the viewer followed.
  await expect(page.locator("article.shot.best")).toHaveCount(1);
  await expect(page.locator("article.shot.rejected")).toHaveCount(1);

  // U on the best frame takes the star off that frame only.
  await page.keyboard.press("ArrowLeft");
  await viewer.getByRole("button", { name: /Clear marks/ }).click();
  await expect(viewer.locator(".viewer-state")).toHaveText("");
  expect(posts.at(-1)).toEqual({
    path: "best",
    body: { file_id: 4172, best: false },
  });
  expect(members[2].is_rejected).toBe(true);

  // Г is U on the Russian layout; on the rejected frame it clears the mark.
  await page.keyboard.press("ArrowRight");
  await russian(page, "г");
  await expect(viewer.locator(".viewer-state")).toHaveText("");
  expect(posts.at(-1)).toEqual({
    path: "reject 4173",
    body: { rejected: false },
  });

  // Nothing to clear: U is a no-op, not a request.
  const sent = posts.length;
  await page.keyboard.press("u");
  await expect(
    viewer.getByRole("button", { name: /Clear marks/ }),
  ).toBeDisabled();
  expect(posts.length).toBe(sent);
  await expect(viewer).toBeVisible();
});

test("a refused mark is said in the page and the viewer stays open", async ({
  page,
}) => {
  await burst(page);
  await page.route(/\/api\/files\/\d+\/reject$/, (r) =>
    r.fulfill({
      status: 400,
      json: {
        error:
          "The file is no longer in the archive: it is in quarantine or deleted. It cannot be marked (#4171)",
      },
    }),
  );
  await page.goto("/#series");
  await page
    .getByRole("button", { name: /DSC04171\.ARW/ })
    .first()
    .click();
  const viewer = page.locator("dialog[open]");
  await page.keyboard.press("x");
  await expect(viewer.getByText(/no longer in the archive/)).toBeVisible();
  await expect(viewer.locator(".viewer-state")).toHaveText("");
  await expect(viewer.getByText("1 / 3")).toBeVisible();
});

test("in Duplicates and versions, B keeps the version on screen", async ({
  page,
}) => {
  const member = (id: number, keeper: boolean) => ({
    file_id: id,
    name: `IMG_${id}.JPG`,
    dir: keeper ? "/archive/2019" : "/archive/Backup/2019",
    role: keeper ? "original" : "copy",
    role_label: "",
    size: 4_183_552,
    width: 6000,
    height: 4000,
    quality: 87,
    breakdown: "",
    evidence: null,
    thumb: null,
    is_keeper: keeper,
  });
  const kept: number[] = [];
  await page.route("**/api/jobs", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/file/**", (r) => r.fulfill({ status: 404 }));
  await page.route("**/api/families?*", (r) =>
    r.fulfill({
      json: {
        total: 1,
        families: [
          {
            id: 913,
            taken_at: 1563129000,
            camera: "Sony A7 III",
            total_size: 8_367_104,
            removable_bytes: 4_183_552,
            members: [member(5531, true), member(5532, false)],
          },
        ],
      },
    }),
  );
  await page.route("**/api/families/913/keeper", async (r) => {
    kept.push(r.request().postDataJSON().file_id);
    await r.fulfill({ json: { ok: true } });
  });
  await page.goto("/#families");
  await page
    .getByRole("button", { name: "Detailed review", exact: true })
    .click();
  await page
    .getByRole("button", { name: /IMG_5532\.JPG/ })
    .first()
    .click();
  const viewer = page.locator("dialog[open]");
  // Two versions open side by side, the second one current.
  await expect(viewer.locator(".viewer-state")).toHaveText("");
  await page.keyboard.press("b");
  await expect(viewer.locator(".viewer-state")).toHaveText("★ kept");
  expect(kept).toEqual([5532]);
  await expect(viewer).toBeVisible();
  // This page has no quarantine mark of its own for one version.
  await expect(viewer.getByRole("button", { name: /Clear marks/ })).toHaveCount(
    0,
  );
});
