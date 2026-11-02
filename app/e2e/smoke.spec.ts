// End to end: the UI in WebKit against `quire serve`, typing as a user would.
import { cpSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, type Page, test } from "@playwright/test";

const regions = (page: Page) => page.locator(".region");

/** A blank document with the insertion point on the page. */
async function blank(page: Page) {
  await page.goto("/");
  await page.getByRole("button", { name: /Blank document/ }).click();
  await page.mouse.click(700, 250);
}

/** Type each line as a new region, as on a fresh page. */
async function typeLines(page: Page, ...lines: string[]) {
  for (const [i, line] of lines.entries()) {
    await page.keyboard.type(line);
    await page.keyboard.press(i < lines.length - 1 ? "Enter" : "Escape");
  }
}

test("evaluates math with units as you type", async ({ page }) => {
  await blank(page);
  await typeLines(page, "q:=12 kN/m", "L:=6 m", "M:=q*L^2/8=");
  await expect(regions(page)).toHaveCount(3);
  // the result keeps the units of the inputs: 54 kN·m, not 54000 J
  await expect(regions(page).nth(2)).toContainText(/=54\s*kN\W?m$/);
});

test("typing starts right after choosing Blank document", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: /Blank document/ }).click();
  await typeLines(page, "a:=2", "b:=a*3=");
  await expect(regions(page)).toHaveCount(2);
  await expect(regions(page).nth(1)).toContainText(/=6$/);
});

test("switching views doesn't make a document Edited", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: /Blank document/ }).click();
  await page.getByRole("button", { name: "Canvas", exact: true }).click();
  await page.getByRole("button", { name: "Pages", exact: true }).click();
  await expect(page.locator(".toolbar .title")).toHaveText("Untitled");
});

test("closing a format-bar gallery with Esc goes back to the math being typed", async ({ page }) => {
  await blank(page);
  await page.keyboard.type("a:=2");
  await page.getByRole("button", { name: /Structures/ }).click();
  await expect(page.locator(".fb-pop")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator(".fb-pop")).toHaveCount(0);
  await page.keyboard.type("*3=");
  await page.mouse.click(700, 600);
  await expect(regions(page).first()).toContainText(/=6$/);
});

test("an empty page says how to start, until there is content", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: /Blank document/ }).click();
  const hint = page.locator(".empty-hint");
  await expect(hint).toHaveText(/Click anywhere to start typing/);
  await page.emulateMedia({ media: "print" });
  await expect(hint).toBeHidden();
  await page.emulateMedia({ media: "screen" });
  await page.keyboard.type("a");
  await expect(hint).toHaveCount(0);
});

test("the insert tools stay put as the status changes", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: /Blank document/ }).click();
  const left = () => page.locator(".toolbar .tools").evaluate((el) => el.getBoundingClientRect().left);
  const before = await left();
  await typeLines(page, "q:=12 kN/m", "x:=1 m + q=");
  await expect(page.locator(".toolbar .errors")).toBeVisible();
  await page.getByRole("button", { name: "Canvas", exact: true }).click();
  expect(await left()).toBe(before);
});

test("explains a unit mismatch", async ({ page }) => {
  await blank(page);
  await typeLines(page, "x:=1 m + 2 kN=");
  await expect(regions(page).first()).toContainText(/cannot add a length \(m\) and a force/);
});

test("fits data and solves an ODE with units", async ({ page }) => {
  await blank(page);
  await typeLines(
    page,
    "x:=[1;2;3;4] m",
    "F:=[5;7;9;11] kN",
    "slope(x, F)=kN/m",
    "D(t, y):=-y/(2 s)",
    "Y:=rkfixed(10 mm, 0, 4 s, 40, D)",
    "Y[41, 2]=mm",
  );
  await expect(regions(page)).toHaveCount(6);
  await expect(regions(page).nth(2)).toContainText(/=2\s*kN\W?m$/);
  // 10 mm·e^−2
  await expect(regions(page).nth(5)).toContainText(/=1\.3534\s*mm$/);
});

test("a word followed by a space becomes text", async ({ page }) => {
  await blank(page);
  await typeLines(page, "The span is simply supported.");
  await expect(regions(page)).toHaveCount(1);
  await expect(regions(page).first()).toHaveClass(/\btext\b/);
  await expect(regions(page).first()).toContainText("The span is simply supported.");
});

test("undo and redo a new region", async ({ page }) => {
  await blank(page);
  await typeLines(page, "a:=2", "b:=a*3=");
  await expect(regions(page)).toHaveCount(2);
  await page.keyboard.press("ControlOrMeta+z");
  await expect(regions(page)).toHaveCount(1);
  await page.keyboard.press("ControlOrMeta+Shift+z");
  await expect(regions(page)).toHaveCount(2);
  await expect(regions(page).nth(1)).toContainText(/=6$/);
});

test("clicking a resize handle without dragging changes nothing, and keeps Redo", async ({ page }) => {
  await blank(page);
  await page.keyboard.press("ControlOrMeta+Alt+p");
  const plot = page.locator(".region.plot2d");
  await expect(plot).toHaveClass(/\bselected\b/);
  const left = async () => (await plot.boundingBox())!.x;
  const at = await left();
  // a step to undo and redo
  await page.keyboard.press("ArrowRight");
  await expect.poll(left).toBeGreaterThan(at);
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(left).toBe(at);
  const box = (await plot.locator(".resize-handle").boundingBox())!;
  const size = (await plot.boundingBox())!;
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  expect((await plot.boundingBox())!.width).toBe(size.width);
  await page.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(left).toBeGreaterThan(at);
});

test("a change upstream updates results below", async ({ page }) => {
  await blank(page);
  await typeLines(page, "L:=2 m", "A:=L*3=");
  await expect(regions(page).nth(1)).toContainText(/=6\s*m$/);
  await regions(page).first().dblclick();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.type("L:=5 m");
  await page.keyboard.press("Escape");
  await expect(regions(page).nth(1)).toContainText(/=15\s*m$/);
});

test("imports a CSV file as a table and exports its values", async ({ page }) => {
  await blank(page);
  await page.keyboard.press("Escape");
  await page.mouse.click(700, 250, { button: "right" });
  const chooser = page.waitForEvent("filechooser");
  await page.getByText("Insert Table from CSV…").click();
  await (
    await chooser
  ).setFiles({
    name: "wind loads.csv",
    mimeType: "text/csv",
    buffer: Buffer.from('Load,q,twice\nDead,4.5 kN/m,=B2*2\n"Live, roof",1.5 kN/m,=B3*2\n'),
  });
  const table = regions(page).first();
  await expect(table).toContainText("Live, roof");
  await expect(page.locator(".flash, .message, body")).toContainText("as the table wind_loads");

  await page.keyboard.press("Escape");
  await table.click({ button: "right" });
  const download = page.waitForEvent("download");
  await page.getByText("Export as CSV…").click();
  const file = await download;
  expect(file.suggestedFilename()).toBe("wind_loads.csv");
  const csv = await (await file.createReadStream()).toArray().then((c) => Buffer.concat(c).toString("utf8"));
  const lines = csv.replace(/^﻿/, "").trim().split("\r\n");
  expect(lines[0]).toBe("Load,q,twice");
  expect(lines[1]).toMatch(/^Dead,4\.5 kN\/m,9\b/);
  expect(lines[2]).toMatch(/^"Live, roof",1\.5 kN\/m,3\b/);
});

test("the Insert menu adds tables, plots and contents, from the mouse or the keyboard", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: /Blank document/ }).click();
  await page.getByRole("button", { name: "Insert" }).click();
  await page
    .getByRole("menuitem", { name: /^Table/ })
    .first()
    .click();
  await expect(page.locator(".region.table")).toHaveCount(1);
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Insert" }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("menuitem").first()).toBeFocused();
  await page.getByRole("menuitem", { name: "Contents" }).click();
  await expect(page.locator(".region.toc")).toHaveCount(1);
});

test("contents and checks lists keep their rows through an edit elsewhere", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: /Simply supported beam/ }).click();
  for (const item of ["Contents", "Summary of Checks"]) {
    await page.getByRole("button", { name: "Insert" }).click();
    await page.getByRole("menuitem", { name: item }).click();
  }
  const rows = page.locator(".region.toc .contents-row");
  await expect(rows.filter({ hasText: "Calculations" })).toHaveCount(1);
  await expect(rows.filter({ hasText: /[✓✗]/ }).first()).toBeVisible();
  const count = await rows.count();
  // count every row the lists take out of the page from here on
  await page.evaluate(() => {
    const w = window as unknown as { replacedRows: number };
    w.replacedRows = 0;
    new MutationObserver((records) => {
      for (const r of records)
        for (const n of r.removedNodes)
          if (n instanceof Element && (n.matches(".contents-row") || n.querySelector(".contents-row")))
            w.replacedRows++;
    }).observe(document.body, { childList: true, subtree: true });
  });
  // typing a paragraph elsewhere changes the document, its layout and the report many times over
  const paragraph = page.locator('[data-region="b01"]');
  await paragraph.dblclick();
  await expect(paragraph.locator(".ProseMirror")).toBeFocused();
  await page.keyboard.press("End");
  await page.keyboard.type(" It carries a uniform load along its whole length, from end to end.");
  await page.keyboard.press("Escape");
  await expect(paragraph).toContainText("from end to end.");
  await expect(rows).toHaveCount(count);
  expect(await page.evaluate(() => (window as unknown as { replacedRows: number }).replacedRows)).toBe(0);
});

test("a region this version can't read shows as a placeholder and is saved back as it was", async ({ page }) => {
  // a fixture shared with the engine's tests (crates/quire-cli/tests/check.rs), copied so saving can't change it
  const fixture = fileURLToPath(
    new URL("../../crates/quire-cli/tests/fixtures/lenient-unreadable.quire", import.meta.url),
  );
  const dir = mkdtempSync(join(tmpdir(), "quire-unreadable-e2e-"));
  const bundle = join(dir, "Odd.quire");
  cpSync(fixture, bundle, { recursive: true });
  const original = readFileSync(join(bundle, "document.json"), "utf8");
  try {
    await page.goto(`/?open=${encodeURIComponent(bundle)}`);
    await expect(page.locator(".toast")).toContainText("2 parts of this document aren't understood");
    const placeholders = page.locator(".region.unreadable");
    await expect(placeholders).toHaveCount(2);
    await expect(placeholders.first()).toContainText("can't read");
    // the other regions still evaluate, and the placeholder's own `a := 100` doesn't count
    await expect(regions(page).last()).toContainText(/=6$/);
    await page.keyboard.press("ControlOrMeta+s");
    await expect(page.locator(".toast")).toHaveText("Saved Odd");
    expect(readFileSync(join(bundle, "document.json"), "utf8")).toBe(original);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
