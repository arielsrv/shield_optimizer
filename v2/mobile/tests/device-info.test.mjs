import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { chromium } from "playwright";
import { startViteServer } from "./helpers/vite-harness.mjs";

let server;
let browser;
let origin;

before(async () => {
  ({ server, origin } = await startViteServer());
  browser = await chromium.launch({ headless: true });
});

after(async () => {
  await browser?.close();
  await server?.close();
});

const SHIELD_PROPERTIES = {
  friendly_name: "Living Room",
  brand: "NVIDIA",
  model: "SHIELD Android TV",
  device_codename: "mdarcy",
  manufacturer: "NVIDIA",
  android_release: "11",
  sdk_level: "30",
  build_id: "PPR1.180610.011",
  board_platform: "tegra",
  characteristics: "tv",
  serial_number: "0323220012345",
};

async function createPage(t, options = {}) {
  const page = await browser.newPage({ viewport: { width: 384, height: 812 } });
  t.after(() => page.close());
  await page.addInitScript((options) => {
    localStorage.clear();
    window.calls = [];
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        window.calls.push({ command, args });
        switch (command) {
          case "get_entitlement": return "pro";
          case "wireless_connect": return { ok: true };
          case "wireless_status": return { connected: true };
          case "list_devices": return [{
            id: 1,
            serial: "A:5555",
            name: "Living Room",
            model: "SHIELD Android TV",
            status: "device",
            connection: "network",
            device_type: "shield",
            properties: options.properties ?? null,
          }];
          case "health_report": return options.health ?? {
            ram: { free_mb: 1024, total_mb: 4096 },
            storage: { used: "8.1G", total: "16G", used_percent: 51 },
            display: {},
            top_memory: [],
          };
          case "app_list_for_device": return [];
          case "package_states": return {};
          case "safety_info":
            return { kind: "caution", reason: "Review whether you use this app." };
          default: return { ok: true, message: "done", transport: "channel" };
        }
      },
    };
  }, options);
  await page.goto(origin);
  await page.evaluate(async () => {
    const { session } = await import("/src/lib/session.svelte.ts");
    const { router } = await import("/src/lib/router.svelte.ts");
    window.session = session;
    window.router = router;
    await session.connect("A", 5555);
  });
  return page;
}

async function openDiagnostics(page) {
  await page.evaluate(() => {
    window.router.reset("dashboard");
    window.router.navigate("diagnostics");
  });
  await page.getByRole("heading", { name: "Diagnostics" }).waitFor();
}

/// Reads the value cell that follows a given label in the About this TV list.
function valueFor(page, label) {
  return page.locator(`dt:text-is("${label}") + dd`);
}

test("Diagnostics reports the TV's Android version and hardware details", async (t) => {
  const page = await createPage(t, { properties: SHIELD_PROPERTIES });
  await openDiagnostics(page);

  await page.getByText("About this TV").waitFor();
  assert.equal(await valueFor(page, "Android version").innerText(), "11 (API 30)");
  assert.equal(await valueFor(page, "Manufacturer").innerText(), "NVIDIA");
  assert.equal(await valueFor(page, "Model").innerText(), "SHIELD Android TV");
  assert.equal(await valueFor(page, "Codename").innerText(), "mdarcy");
  assert.equal(await valueFor(page, "Chipset").innerText(), "tegra");
  assert.equal(await valueFor(page, "Build ID").innerText(), "PPR1.180610.011");
  assert.equal(await valueFor(page, "Hardware ID").innerText(), "0323220012345");
  assert.equal(await valueFor(page, "Total RAM").innerText(), "4096 MB");
  assert.equal(await valueFor(page, "Total storage").innerText(), "16G");
});

test("Dashboard shows the Android version beside the address", async (t) => {
  const page = await createPage(t, { properties: SHIELD_PROPERTIES });
  await page.evaluate(() => window.router.reset("dashboard"));
  await page.getByText("A · Android 11").waitFor();
});

test("A property the TV did not report reads as unknown, never as a neighbour's value", async (t) => {
  // The failure this guards against is a shifted read making the Android
  // version render the model. An absent value must stay absent.
  const page = await createPage(t, {
    properties: { ...SHIELD_PROPERTIES, android_release: "", sdk_level: "", build_id: "unknown" },
  });
  await openDiagnostics(page);

  assert.equal(await valueFor(page, "Android version").innerText(), "—");
  assert.equal(await valueFor(page, "Build ID").innerText(), "—");
  // Its neighbours are untouched.
  assert.equal(await valueFor(page, "Model").innerText(), "SHIELD Android TV");
  assert.equal(await valueFor(page, "Chipset").innerText(), "tegra");
});

test("A device with no profile says so instead of showing blank rows", async (t) => {
  const page = await createPage(t, { properties: null });
  await openDiagnostics(page);

  await page.getByText("About this TV").waitFor();
  await page.getByText("The TV hasn't reported its details.").waitFor();
  assert.equal(await page.locator('dt:text-is("Android version")').count(), 0);
});

test("Missing health data leaves the totals blank rather than inventing them", async (t) => {
  const page = await createPage(t, {
    properties: SHIELD_PROPERTIES,
    health: { ram: {}, storage: {}, display: {}, top_memory: [] },
  });
  await openDiagnostics(page);

  assert.equal(await valueFor(page, "Total RAM").innerText(), "—");
  assert.equal(await valueFor(page, "Total storage").innerText(), "—");
  // The profile read is independent of the health read.
  assert.equal(await valueFor(page, "Android version").innerText(), "11 (API 30)");
});
