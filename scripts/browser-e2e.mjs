// Browser walk of the first sign-in: set a password, add the first place,
// land on Today. Run by .github/workflows/browser.yml.
//
//   HOMESTEAD_URL   http://127.0.0.1:8088
//   SETUP_PATH      /setup/<token>

import { chromium } from "playwright";

const base = process.env.HOMESTEAD_URL;
const setup = process.env.SETUP_PATH;
if (!base || !setup) {
  console.error("HOMESTEAD_URL and SETUP_PATH are required");
  process.exit(1);
}

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 390, height: 844 } });
const failures = [];
page.on("pageerror", (err) => failures.push(String(err)));

await page.goto(base + setup);
await page.getByLabel("New password (12 characters or more; a few words work well)").fill("homestead-e2e");
await page.getByLabel("Same again").fill("homestead-e2e");
await page.getByRole("button", { name: "Set password and sign in" }).click();
await page.getByRole("heading", { name: "Welcome" }).waitFor();
await page.getByLabel("Place name").fill("E2E bed");
await page.getByRole("button", { name: "Add the place" }).click();
await page.getByText("Places are already recorded.").waitFor();
await page.getByRole("button", { name: "Go to Today" }).click();
await page.getByRole("heading", { name: "Today" }).waitFor();
const place = page.locator('select option', { hasText: "E2E bed" });
if ((await place.count()) !== 1) throw new Error("Today did not offer E2E bed");

const manifest = await page.request.get(base + "/manifest.webmanifest");
if (!manifest.ok()) throw new Error("manifest " + manifest.status());
const body = await manifest.json();
if (body.start_url !== "/") throw new Error("start_url " + body.start_url);

const worker = await page.request.get(base + "/sw.js");
const script = await worker.text();
if (!script.includes("Authenticated pages stay on the network")) {
  throw new Error("service worker caches more than the shell");
}

if (failures.length) throw new Error(failures.join("\n"));
await browser.close();
console.log("browser workflow ok");
