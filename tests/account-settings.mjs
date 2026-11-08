// Browser regression: run against `cd web && vp dev`.
// node tests/account-settings.mjs http://127.0.0.1:5173
// PLAYWRIGHT and CHROMIUM optionally select the installed browser tooling.
import assert from "node:assert/strict";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;
const browser = await chromium.launch({
  executablePath: process.env.CHROMIUM,
  args: ["--no-sandbox"],
});
try {
  const page = await browser.newPage({ viewport: { width: 390, height: 844 } });
  await page.route("**/games", (route) => route.fulfill({ json: [] }));
  await page.route("**/api/**", (route) => route.fulfill({ json: [] }));
  const sent = [];
  page.on("request", (request) => sent.push(request.url() + (request.postData() ?? "")));
  await page.goto(new URL("play.html", process.argv[2] ?? "http://127.0.0.1:5173").href);
  const open = async () => {
    await page.getByRole("button", { name: "設定", exact: true }).click();
    await page.waitForFunction(() => document.querySelector("fieldset")?.disabled === false);
  };
  const email = page.getByLabel("メールアドレス", { exact: true });
  const password = page.getByLabel("パスワード", { exact: true });
  const remember = page.getByLabel("このブラウザにパスワードを保存", { exact: true });
  const save = async () => {
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "設定を保存しました" }).waitFor();
  };
  await open();
  assert.equal(await remember.isChecked(), false);
  await email.fill("fixture@example.invalid");
  await password.fill("fixture-only-secret");
  await remember.check();
  await save();
  await page.reload();
  await open();
  assert.equal(await email.inputValue(), "fixture@example.invalid");
  assert.equal(await password.inputValue(), "fixture-only-secret");
  assert.equal(await password.getAttribute("type"), "password");
  const encrypted = await page.evaluate(async () => {
    const db = await new Promise((resolve, reject) => {
      const request = indexedDB.open("ippai-account", 1);
      request.onsuccess = () => resolve(request.result);
      request.onerror = reject;
    });
    const record = await new Promise((resolve, reject) => {
      const request = db.transaction("settings").objectStore("settings").get("dmm");
      request.onsuccess = () => resolve(request.result);
      request.onerror = reject;
    });
    db.close();
    return !record.password && !record.key.extractable && record.key.algorithm.name === "AES-GCM"
      && !new TextDecoder().decode(record.ciphertext).includes("fixture-only-secret");
  });
  assert.equal(encrypted, true);
  await remember.uncheck();
  await save();
  await page.reload();
  await open();
  assert.equal(await password.inputValue(), "");
  assert.equal(await remember.isChecked(), false);
  await page.getByRole("button", { name: "保存情報を削除", exact: true }).click();
  await page.getByRole("status").filter({ hasText: "削除しました" }).waitFor();
  await page.reload();
  await open();
  assert.equal(await email.inputValue(), "");
  await page.keyboard.press("Escape");
  await page.getByRole("dialog").waitFor({ state: "hidden" });
  await page.waitForFunction(() => document.activeElement?.getAttribute("aria-label") === "設定");
  assert.equal(sent.some((request) => request.includes("fixture-only-secret") || request.includes("fixture@example.invalid")), false);
  console.log("account settings: restore, encryption, opt-out, deletion, keyboard focus and no network disclosure passed");
} finally {
  await browser.close();
}
