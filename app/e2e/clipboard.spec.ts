// End to end: copy and paste between documents, to other apps and from them.
import { readFileSync } from "node:fs";
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

/** What a copy of the selection puts on the clipboard, type by type. */
function copied(page: Page): Promise<Record<string, string>> {
  return page.evaluate(() => {
    const e = new ClipboardEvent("copy", { clipboardData: new DataTransfer(), bubbles: true, cancelable: true });
    (document.activeElement ?? document.body).dispatchEvent(e);
    return Object.fromEntries([...e.clipboardData!.types].map((t) => [t, e.clipboardData!.getData(t)]));
  });
}

/** Paste as another app would: these types on the clipboard. */
async function paste(page: Page, data: Record<string, string>) {
  await page.evaluate((data) => {
    const dt = new DataTransfer();
    for (const [type, value] of Object.entries(data)) dt.setData(type, value);
    const e = new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true });
    (document.activeElement ?? document.body).dispatchEvent(e);
  }, data);
}

const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

test("copies regions into another document", async ({ page, context }) => {
  await blank(page);
  await typeLines(page, "a:=2", "b:=a*3=");
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.press("ControlOrMeta+c");

  const other = await context.newPage();
  await blank(other);
  await other.keyboard.press("ControlOrMeta+v");
  await expect(regions(other)).toHaveCount(2);
  await expect(regions(other).nth(1)).toContainText(/=6$/);
});

test("copies math, text and tables for other apps", async ({ page }) => {
  await blank(page);
  await typeLines(page, "q:=12 kN/m", "M:=q*2 m=", "The span is simply supported.");
  await expect(regions(page).nth(1)).toContainText(/=24\s*kN$/);
  // the text as the engine rendered it: the results are those of the document as typed,
  // so the copy carries them (until then it carries the math as typed, without results)
  await expect(regions(page).nth(2).locator("p")).toHaveText("The span is simply supported.");
  await page.keyboard.press("ControlOrMeta+a");
  const data = await copied(page);
  expect(data["text/plain"]).toBe("q:=12 kN/m\nM:=q*2 m =\nThe span is simply supported.");
  const html = data["text/html"];
  expect(html).toContain("<p>The span is simply supported.</p>");
  expect(html).toContain('<math xmlns="http://www.w3.org/1998/Math/MathML" alttext="M:=q*2 m =">');
  expect(html).toContain("<mn>24</mn>");
  expect(html).not.toContain("data-v=");
  expect(JSON.parse(data["application/x-quire-regions"]).regions).toHaveLength(3);
});

test("a copy right after typing has what was typed, before the results catch up", async ({ page }) => {
  await blank(page);
  await typeLines(page, "q:=12 kN/m", "The span");
  await expect(regions(page).nth(1).locator("p")).toHaveText("The span");
  // the engine is slow to answer (a busy machine): the results on screen fall behind
  const held: (() => void)[] = [];
  await page.route("**/api/report", (route) => void held.push(() => void route.continue().catch(() => {})));
  // Enter edits the selected text again, the caret at its end
  await page.keyboard.press("Enter");
  await page.keyboard.type(" is simply supported.");
  await page.keyboard.press("Escape");
  await page.keyboard.press("ControlOrMeta+a");
  const data = await copied(page);
  expect(data["text/plain"]).toBe("q:=12 kN/m\nThe span is simply supported.");
  expect(data["text/html"]).toContain("<p>The span is simply supported.</p>");
  expect(data["text/html"]).toContain("q:=12 kN/m");
  expect(JSON.parse(data["application/x-quire-regions"]).regions[1].text).toBe("The span is simply supported.");
  for (const release of held) release();
});

test("pastes regions with their images from another document", async ({ page }) => {
  await blank(page);
  // as another Quire window writes it: only the HTML survived, the image is not in this document
  const clip = {
    quire: 1,
    regions: [
      { id: "x1", kind: "math", at: [0, 0], expr: "w:=3 m" },
      { id: "x2", kind: "image", at: [0, 36], size: [40, 40], src: "assets/00000000000000ff.png" },
    ],
    assets: { "assets/00000000000000ff.png": `data:image/png;base64,${PNG}` },
  };
  const b64 = Buffer.from(JSON.stringify(clip)).toString("base64");
  await paste(page, { "text/html": `<div data-quire-clip="${b64}"><p>w := 3 m</p></div>`, "text/plain": "w:=3 m" });
  await expect(regions(page)).toHaveCount(2);
  await expect(page.locator(".region img")).toHaveAttribute("src", /^data:image\/png;base64,/);
});

test("pastes lines of text as math and text regions", async ({ page }) => {
  await blank(page);
  await paste(page, { "text/plain": "Loads on the beam\nq:=12 kN/m\nL:=6 m\nM:=q*L^2/8=\n" });
  await expect(regions(page)).toHaveCount(4);
  await expect(regions(page).first()).toHaveClass(/\btext\b/);
  await expect(regions(page).nth(3)).toContainText(/=54\s*kN\W?m$/);
  // one below the other, none covering the next
  const boxes = await regions(page).evaluateAll((els) =>
    els.map((e) => e.getBoundingClientRect()).map((r) => [r.top, r.bottom]),
  );
  for (let i = 1; i < boxes.length; i++) expect(boxes[i][0]).toBeGreaterThanOrEqual(boxes[i - 1][1]);
});

test("pastes LaTeX as linear math", async ({ page }) => {
  await blank(page);
  await typeLines(page, "q:=12 kN/m", "L:=6 m");
  await page.mouse.click(700, 400);
  await paste(page, { "text/plain": "$M = \\frac{q L^{2}}{8}$" });
  await expect(regions(page)).toHaveCount(3);
  await regions(page).nth(2).dblclick();
  await expect(page.locator("input.math-input")).toHaveValue("M := q*L^2/8");
  // into the math being edited, too
  await page
    .locator("input.math-input")
    .evaluate((i: HTMLInputElement) => i.setSelectionRange(i.value.length, i.value.length));
  await page.keyboard.type(" + ");
  await paste(page, { "text/plain": "\\sqrt{q^{2}} \\cdot L^2" });
  await expect(page.locator("input.math-input")).toHaveValue("M := q*L^2/8 + sqrt(q^2)*L^2");
});

test("pastes an HTML table as a spreadsheet table", async ({ page }) => {
  await blank(page);
  await paste(page, {
    "text/html":
      "<table><tr><th>Load</th><th>q</th></tr><tr><td>Dead</td><td>4.5 kN/m</td></tr><tr><td>Live<br>roof</td><td>1.5 kN/m</td></tr></table>",
    "text/plain": "Load q\nDead 4.5 kN/m\nLive roof 1.5 kN/m",
  });
  await expect(regions(page)).toHaveCount(1);
  await expect(regions(page).first()).toHaveClass(/\btable\b/);
  await expect(regions(page).first()).toContainText("Dead");
  await expect(regions(page).first()).toContainText("4.5 kN/m");
});

test("the context menu pastes", async ({ page }) => {
  await blank(page);
  await typeLines(page, "c:=5");
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.press("ControlOrMeta+c");
  await page.keyboard.press("Escape");
  await page.mouse.click(700, 500, { button: "right" });
  const item = page.getByText("Paste", { exact: true });
  await expect(item).toBeEnabled();
  await item.click();
  await expect(regions(page)).toHaveCount(2);
});

/**
 * Serve the page with the desktop app's content security policy, read from tauri.conf.json so
 * the two can't drift apart (the dev server sends none).
 */
async function withAppCsp(page: Page) {
  const conf = JSON.parse(readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"));
  const csp: string = conf.app.security.csp;
  await page.route(
    (url) => url.pathname === "/",
    async (route) => {
      const response = await route.fetch();
      await route.fulfill({ response, headers: { ...response.headers(), "content-security-policy": csp } });
    },
  );
}

test("copies a plot as an image under the desktop app's content security policy", async ({ page }) => {
  await withAppCsp(page);
  // what reaches the clipboard: the PNG's first bytes
  await page.addInitScript(() => {
    Object.defineProperty(navigator.clipboard, "write", {
      value: async (items: ClipboardItem[]) => {
        const png = await items[0].getType("image/png");
        (window as unknown as { copied: number[] }).copied = [...new Uint8Array(await png.slice(0, 4).arrayBuffer())];
      },
    });
  });
  await page.goto("/");
  await page.getByRole("button", { name: /Simply supported beam/ }).click();
  const plot = page.locator(".region.plot2d");
  await expect(plot.locator(".plot svg")).toBeVisible();
  // the policy is in force: an image from a blob: URL can't load
  const blobLoads = await page.evaluate(async () => {
    const img = new Image();
    img.src = URL.createObjectURL(new Blob(["<svg xmlns='http://www.w3.org/2000/svg'/>"], { type: "image/svg+xml" }));
    return img.decode().then(
      () => true,
      () => false,
    );
  });
  expect(blobLoads).toBe(false);

  await plot.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Copy as Image" }).click();
  await expect(page.locator(".toast")).toContainText("Copied the plot as an image");
  // ‰PNG
  expect(await page.evaluate(() => (window as unknown as { copied?: number[] }).copied)).toEqual([
    0x89, 0x50, 0x4e, 0x47,
  ]);
});
