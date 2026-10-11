// Browser and Discord halves of scripts/onboarding-e2e.sh. Each phase is
// one command; the shell script runs them in order and checks the
// database between them.
//
//   node onboarding-e2e.mjs <phase>
//
// Environment: E2E (the target/e2e directory), CORE (the core's address),
// URL (a signing link, for sign/reject). `join` drives the /join link.
//
// Documenso draws the document on a canvas, so fields are clicked by page
// position, from legal/onboarding.fields.json (see signHere).
// Clicking a filled field clears it, so each document is signed in one pass.

import crypto from "node:crypto";
import fs from "node:fs";
import { chromium } from "playwright";

const E = process.env.E2E;
const DOCUMENSO = "http://localhost:3000";
const secret = (name) => fs.readFileSync(`${E}/${name}`, "utf8").trim();

async function browser(viewport = { width: 1280, height: 1400 }) {
  // Playwright's own browsers aren't installed here; the system Chrome is.
  // homestead-core is the core's public_url host (see the shell script).
  const args = process.env.CORE ? [`--host-resolver-rules=MAP homestead-core ${process.env.CORE}`] : [];
  const b = await chromium.launch({ channel: "chrome", args });
  const p = await b.newPage({ viewport });
  // A form submitted before hydration goes out as a GET with every field
  // in the URL; never let a password travel that way.
  await p.route(/password=/, (r) => r.abort());
  return { b, p };
}

async function signIn() {
  const { b, p } = await browser();
  await p.goto(`${DOCUMENSO}/signin`, { waitUntil: "networkidle" });
  await p.locator('input[name="email"]').fill("steward@e2e.test");
  await p.locator('input[name="password"]').fill(secret("steward.pw"));
  await p.getByRole("button", { name: "Sign In", exact: true }).click();
  await p.waitForURL((u) => u.pathname.startsWith("/t/"), { timeout: 30000 });
  const team = new URL(p.url()).pathname.split("/")[2];
  return { b, p, team };
}

async function drawIn(p, dialog) {
  const c = await dialog.locator("canvas").first().boundingBox();
  await p.mouse.move(c.x + 40, c.y + c.height / 2);
  await p.mouse.down();
  for (let i = 0; i < 14; i++) {
    await p.mouse.move(c.x + 40 + i * 22, c.y + c.height / 2 + (i % 2 ? 30 : -30), { steps: 4 });
  }
  await p.mouse.up();
}

// Fills the welcome agreement on its signing page and completes it. Each
// field is clicked where legal/onboarding.fields.json puts it (percentages
// of its page), measured against that page as Documenso draws it, so the
// document can change length without this breaking.
const LAYOUT = JSON.parse(fs.readFileSync(new URL("../../legal/onboarding.fields.json", `file://${E}/`), "utf8"));
async function at(p, kind, label) {
  const f = LAYOUT.find((f) => f.kind === kind && (!label || f.label === label));
  if (!f) throw new Error(`no ${kind} ${label ?? ""} in the layout`);
  return spot(p, f);
}
async function spot(p, f) {
  const page = p.locator(`[data-page-number="${f.page}"]`).first();
  await page.scrollIntoViewIfNeeded();
  await p.waitForTimeout(300);
  const b = await page.boundingBox();
  return [b.x + ((f.x + f.width / 2) / 100) * b.width, b.y + ((f.y + f.height / 2) / 100) * b.height];
}

async function signHere(p) {
  await p.locator('[data-page-number="1"]').first().waitFor();
  await p.waitForTimeout(1500);
  const dialog = p.locator('[role="dialog"]');
  const text = async (label, value) => {
    await p.mouse.click(...(await at(p, "TEXT", label)));
    await dialog.waitFor();
    await dialog.locator("textarea, input").first().fill(value);
    await dialog.getByRole("button", { name: "Enter" }).click();
    await dialog.waitFor({ state: "detached" });
    await p.waitForTimeout(600);
  };
  // Every release box has initials; they fill from the signer's name, or
  // ask for them.
  for (const f of LAYOUT.filter((f) => f.kind === "INITIALS")) {
    await p.mouse.click(...(await spot(p, f)));
    await p.waitForTimeout(1000);
    if (await dialog.count()) {
      await dialog.locator("input").first().fill("ST");
      await dialog.getByRole("button", { name: /enter|save|sign/i }).last().click();
      await dialog.waitFor({ state: "detached" });
    }
  }
  await text("Emergency contact (name)", "Sam Tester");
  await text("Emergency contact phone", "818-555-0142");
  await p.mouse.click(...(await at(p, "NAME"))); // filled from the recipient
  await p.waitForTimeout(1000);
  await p.mouse.click(...(await at(p, "SIGNATURE")));
  await dialog.waitFor();
  await drawIn(p, dialog);
  await dialog.getByRole("button", { name: "Sign", exact: true }).click();
  await dialog.waitFor({ state: "detached" });
  await p
    .getByText("0 Fields Remaining")
    .first()
    .waitFor()
    .catch(async (e) => {
      // What was left unfilled, for whoever reads the failure.
      await p.screenshot({ path: `${E}/unsigned.png`, fullPage: true });
      throw new Error(`fields left unfilled; see ${E}/unsigned.png\n${e.message}`);
    });
  await p.getByRole("button", { name: "Complete" }).first().click();
  await p.locator('[role="dialog"]').getByRole("button", { name: /sign|complete/i }).last().click();
}

const phases = {
  // The steward's Documenso account. Email can't be confirmed without
  // SMTP; the shell marks it verified in Documenso's database.
  async signup() {
    const { b, p } = await browser();
    await p.goto(`${DOCUMENSO}/signup`, { waitUntil: "networkidle" });
    await p.locator('input[name="name"]').fill("E2E Steward");
    await p.locator('input[name="email"]').fill("steward@e2e.test");
    await p.locator('input[name="password"]').fill(secret("steward.pw"));
    await p.getByTestId("signature-pad-dialog-button").click();
    const d = p.locator('[role="dialog"]');
    await d.waitFor();
    await drawIn(p, d);
    await d.getByRole("button", { name: "Next" }).click();
    await p.getByRole("button", { name: "Create account" }).click();
    await p.waitForURL(/unverified-account|\/t\//, { timeout: 30000 });
    await b.close();
  },

  // An API token for the core; shown once, in an input.
  async token() {
    const { b, p, team } = await signIn();
    await p.goto(`${DOCUMENSO}/t/${team}/settings/tokens`, { waitUntil: "networkidle" });
    await p.getByRole("button", { name: "Create token" }).last().click();
    const d = p.locator('[role="dialog"]');
    await d.locator('input[name="tokenName"]').fill("homestead-core-e2e");
    await d.getByRole("button", { name: "Create token" }).click();
    await p.waitForTimeout(2000);
    const values = await p.locator("input").evaluateAll((e) => e.map((x) => x.value));
    const token = values.join(" ").match(/api_[A-Za-z0-9]+/);
    if (!token) throw new Error("Documenso showed no API token");
    fs.writeFileSync(`${E}/documenso-token`, token[0], { mode: 0o600 });
    await b.close();
  },

  // The webhook production has: completed and rejected, to the core.
  async webhook() {
    const { b, p, team } = await signIn();
    await p.goto(`${DOCUMENSO}/t/${team}/settings/webhooks`, { waitUntil: "networkidle" });
    await p.getByRole("button", { name: /create webhook/i }).last().click();
    const d = p.locator('[role="dialog"]');
    await d.locator('input[name="webhookUrl"]').fill("http://homestead-core:8088/hooks/documenso");
    await d.locator('input[name="secret"]').fill(secret("webhook-secret"));
    await d.getByPlaceholder("Select triggers").click();
    for (const t of [/document\.completed/i, /document\.rejected/i]) {
      await p.getByRole("option").filter({ hasText: t }).first().click();
    }
    // Escape would close the whole dialog; clicking its title closes the list.
    await d.locator("h2").first().click();
    await d.getByRole("button", { name: "Create" }).click();
    await p.waitForTimeout(2000);
    await b.close();
  },

  // `/onboard`, signed with the test key the core was configured with,
  // exactly as Discord signs interactions. Prints the core's answer.
  async onboard() {
    const [user, role] = [process.env.USER_ID, process.env.ROLE || "Volunteer"];
    const body = JSON.stringify({
      type: 2,
      application_id: "1",
      token: "e2e",
      data: {
        name: "onboard",
        options: [
          { name: "name", value: "Riley Tester" },
          { name: "email", value: "riley@e2e.test" },
          { name: "joining_as", value: role },
        ],
      },
      member: { user: { id: user, username: "riley.e2e" } },
    });
    const ts = String(Math.floor(Date.now() / 1000));
    const key = crypto.createPrivateKey(fs.readFileSync(`${E}/discord-key.pem`));
    const sig = crypto.sign(null, Buffer.from(ts + body), key).toString("hex");
    const r = await fetch(`http://${process.env.CORE}:8088/discord/interactions`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-signature-ed25519": sig, "x-signature-timestamp": ts },
      body,
    });
    console.log(JSON.stringify(await r.json()));
  },

  async reject() {
    const { b, p } = await browser();
    await p.goto(process.env.URL, { waitUntil: "networkidle" });
    await p.getByRole("button", { name: "Reject Document" }).click();
    const d = p.locator('[role="dialog"]');
    await d.locator("textarea").fill("E2E: the decline path");
    await d.getByRole("button", { name: "Reject Document" }).click();
    await p.waitForURL(/rejected/, { timeout: 30000 });
    await b.close();
  },

  async sign() {
    const { b, p } = await browser();
    await p.goto(process.env.URL, { waitUntil: "networkidle" });
    await signHere(p);
    await p.waitForURL(/\/complete/, { timeout: 30000 });
    await b.close();
  },

  // The Instagram link: the form, the signing page it leads to, and
  // Documenso sending the browser back to the join page. Prints that
  // page's text.
  async join() {
    const { b, p } = await browser();
    await p.goto("http://homestead-core:8088/join", { waitUntil: "networkidle" });
    await p.locator('input[name="name"]').fill("Riley Tester");
    await p.locator('input[name="email"]').fill("riley@e2e.test");
    await p.getByRole("button", { name: "Continue" }).click();
    await p.waitForURL(/\/join\/[0-9a-f]{64}$/);
    const mine = p.url();
    // The page refreshes itself until Documenso has the agreement.
    const sign = p.getByRole("link", { name: /Read and sign/ });
    await sign.waitFor({ timeout: 90000 });
    await sign.click();
    await p.waitForURL((u) => u.origin === DOCUMENSO, { timeout: 30000 });
    await p.waitForLoadState("networkidle");
    await signHere(p);
    await p.waitForURL((u) => u.href.startsWith(mine), { timeout: 60000 });
    // Documenso's webhook can land after the redirect; the page checks
    // again by itself, as it does for a person.
    await p.getByText(/Signed, thank you|declined|couldn't/).first().waitFor({ timeout: 60000 }).catch(() => {});
    console.log(await p.locator("main").innerText());
    await b.close();
  },
};

const phase = phases[process.argv[2]];
if (!phase) {
  console.error(`phases: ${Object.keys(phases).join(", ")}`);
  process.exit(2);
}
await phase();
