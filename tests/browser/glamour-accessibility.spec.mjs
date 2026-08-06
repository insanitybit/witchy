import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";

const here = fileURLToPath(new URL(".", import.meta.url));
const repo = resolve(here, "../..");
const binary = process.env.WITCHY_BIN || resolve(repo, "target/debug/witchy");
const project = "projects/glamour/examples/accessibility";
// `witchy run` drives the app through Exec, whose contract concatenates the
// child's stdout with its stderr (rfcs/0004). When platform confinement is
// enforced (Linux: Landlock + seccomp), the app's `confinement: layer=...`
// diagnostic lines (on stderr) fold into that combined output. They are
// operational noise, not rendered HTML — strip them before injecting into the
// DOM, matching the same filter used by scripts/e2e-full.sh and release-smoke.sh.
const html = execFileSync(binary, ["run", project], {
  cwd: repo,
  encoding: "utf8",
})
  .split("\n")
  .filter((line) => !line.startsWith("confinement: layer="))
  .join("\n")
  .trim();

test("Glamour accessibility primitives retain native browser semantics", async ({ page, browserName }) => {
  await page.setContent(`<!doctype html><html lang="en"><body>${html}</body></html>`);

  const open = page.getByRole("button", { name: "Open panel" });
  const image = page.getByRole("img", { name: "Witchy sigil" });
  const name = page.getByLabel("Display name");
  const preferences = page.getByRole("group", { name: "Preferences" });
  const updates = page.getByRole("checkbox", { name: "Email updates" });
  const theme = page.getByRole("combobox", { name: "Theme" });
  const notes = page.getByRole("textbox", { name: "Notes" });
  const disclosure = page.getByText("Advanced settings", { exact: true });
  const save = page.getByRole("button", { name: "Save profile" });
  await expect(open).toHaveAttribute("type", "button");
  await expect(image).toHaveAttribute("alt", "Witchy sigil");
  await expect(name).toHaveAttribute("id", "display-name");
  await expect(name).toHaveValue("Ada");
  await expect(preferences).toBeVisible();
  await expect(updates).toBeChecked();
  await expect(theme).toHaveValue("system");
  await expect(notes).toHaveValue("Ready");
  await expect(disclosure).toBeVisible();
  await expect(save).toHaveAttribute("type", "submit");

  const nextFocus = browserName === "webkit" ? "Alt+Tab" : "Tab";
  await page.keyboard.press(nextFocus);
  await expect(open).toBeFocused();
  await page.keyboard.press(nextFocus);
  await expect(name).toBeFocused();
  await page.keyboard.press(nextFocus);
  await expect(updates).toBeFocused();
  await page.keyboard.press(nextFocus);
  await expect(theme).toBeFocused();
  await page.keyboard.press(nextFocus);
  await expect(notes).toBeFocused();
  await page.keyboard.press(nextFocus);
  await expect(disclosure).toBeFocused();
  await page.keyboard.press(nextFocus);
  await expect(save).toBeFocused();

  await updates.click();
  await expect(updates).not.toBeChecked();
  await theme.selectOption("dark");
  await expect(theme).toHaveValue("dark");
  await notes.fill("Browser-owned input");
  await expect(notes).toHaveValue("Browser-owned input");
  await disclosure.click();
  await expect(disclosure.locator("xpath=..")).toHaveAttribute("open", "");

  await page.emulateMedia({ colorScheme: "dark", reducedMotion: "reduce", forcedColors: "active" });
  await expect(open).toBeVisible();
  await expect(name).toBeEditable();
  await expect(save).toBeVisible();

  for (const zoom of [2, 4]) {
    await page.evaluate((value) => { document.documentElement.style.zoom = String(value); }, zoom);
    await expect(open).toBeVisible();
    await expect(name).toBeVisible();
    await expect(preferences).toBeVisible();
    await expect(updates).toBeVisible();
    await expect(theme).toBeVisible();
    await expect(notes).toBeVisible();
    await expect(disclosure).toBeVisible();
    await expect(save).toBeVisible();
  }
});
