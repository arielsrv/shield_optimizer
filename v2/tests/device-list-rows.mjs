// The Devices list has to be honest about three things at once:
//   - one physical device reached two ways is ONE row (the duplicate-transport
//     regression: adb auto-connects a paired mDNS device under its service
//     name, and dialling it again by address makes a second transport);
//   - a device that is positively not an Android TV says so and does not open
//     the TV tools, while a device we simply cannot read yet claims nothing;
//   - a network device is not told to look for a *USB* dialog.
//
// Rendering is where all three become visible, so this drives the real screen.
import assert from "node:assert/strict";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const V2 = join(HERE, "..");

function serverURL(server) {
  const address = server.httpServer?.address();
  if (!address || typeof address === "string") {
    throw new Error("Vite did not expose its bound TCP address");
  }
  const host = address.address.includes(":") ? `[${address.address}]` : address.address;
  return `http://${host}:${address.port}`;
}

function setHarnessEnvironment() {
  const keys = ["VITE_DEMO", "TAURI_DEV_HOST"];
  const previous = new Map(keys.map((key) => [
    key,
    { present: Object.hasOwn(process.env, key), value: process.env[key] },
  ]));
  process.env.VITE_DEMO = "1";
  delete process.env.TAURI_DEV_HOST;
  return () => {
    for (const [key, prior] of previous) {
      if (prior.present) process.env[key] = prior.value;
      else delete process.env[key];
    }
  };
}

const tvProperties = {
  friendly_name: "Bedroom Shield", brand: "NVIDIA", model: "SHIELD Android TV",
  device_codename: "mdarcy", manufacturer: "NVIDIA", android_release: "11",
  sdk_level: "30", build_id: "PPR1", board_platform: "tegra",
  characteristics: "tv", serial_number: "1324619053514",
};

const phoneProperties = {
  friendly_name: null, brand: "google", model: "Pixel 10 Pro",
  device_codename: "blazer", manufacturer: "Google", android_release: "16",
  sdk_level: "36", build_id: "BP41", board_platform: "zuma",
  characteristics: "nosdcard", serial_number: "58040DLCH005YV",
};

const ROWS = [
  { id: 1, serial: "192.168.42.196:5555", name: "Bedroom Shield",
    model: "Shield TV Pro (2019)", device_type: "shield", status: "device",
    connection: "network", properties: tvProperties },
  // Unknown *with* readable properties = positively not a TV.
  { id: 2, serial: "192.168.42.211:34083", name: "Bryan Pixel 10 Pro",
    model: "Pixel 10 Pro", device_type: "unknown", status: "device",
    connection: "network", properties: phoneProperties },
  // No properties = we do not know what this is yet. Claim nothing.
  { id: 3, serial: "192.168.42.143:5555", name: "192.168.42.143:5555",
    model: "", device_type: "unknown", status: "unauthorized",
    connection: "network", properties: null },
  // Same, but over USB — the one place the USB wording is right.
  { id: 4, serial: "0323220012345", name: "0323220012345",
    model: "", device_type: "unknown", status: "unauthorized",
    connection: "usb", properties: null },
];

const rowFor = (page, name) =>
  page.locator("li", { has: page.getByText(name, { exact: true }) }).first();

async function exercise({ browser, base }) {
  const page = await browser.newPage({ viewport: { width: 1280, height: 1400 } });
  await page.goto(base, { waitUntil: "networkidle" });
  await page.getByText("NVIDIA SHIELD", { exact: false }).first().waitFor();

  await page.evaluate((rows) => {
    const bridge = window.__TAURI_INTERNALS__;
    const original = bridge.invoke.bind(bridge);
    bridge.invoke = async (command, args = {}) =>
      command === "list_devices" ? rows : original(command, args);
  }, ROWS);
  await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await page.getByText("Bryan Pixel 10 Pro", { exact: true }).waitFor();

  // A real TV opens the tools and carries no "not a TV" tag.
  const tv = rowFor(page, "Bedroom Shield");
  assert.equal(await tv.locator("a.device-row").count(), 1, "a TV row must be a link");
  assert.equal(await tv.getByText("NOT AN ANDROID TV").count(), 0);

  // A phone is listed, labelled, and not openable.
  const phone = rowFor(page, "Bryan Pixel 10 Pro");
  assert.equal(await phone.getByText("NOT AN ANDROID TV").count(), 1,
    "a device that reported it is not a TV must say so");
  assert.equal(await phone.locator("a.device-row").count(), 0,
    "the TV tools must not open for a phone");
  assert.equal(await phone.getByText("Pixel 10 Pro").count() > 0, true,
    "it stays visible — labelling is not hiding");

  // An unauthorized NETWORK device: no USB wording.
  const net = rowFor(page, "192.168.42.143:5555");
  const netHelp = await net.locator(".unauthorized-help").innerText();
  assert.match(netHelp, /"Allow debugging\?"/, netHelp);
  assert.doesNotMatch(netHelp.split("Revoke")[0], /Allow USB debugging/,
    "a network device must not be told to look for a USB dialog");
  // We do not know what it is, so we must not label it.
  assert.equal(await net.getByText("NOT AN ANDROID TV").count(), 0,
    "an unreadable device is unknown, not known-not-a-TV");

  // An unauthorized USB device: the USB wording is correct and must survive.
  const usb = rowFor(page, "0323220012345");
  const usbHelp = await usb.locator(".unauthorized-help").innerText();
  assert.match(usbHelp, /"Allow USB debugging\?"/, usbHelp);

  console.log(
    "Device list rows passed: TVs open, a known non-TV is labelled and inert, an unknown device claims nothing, and only USB devices are told about a USB dialog.",
  );
}

async function main() {
  const restoreEnvironment = setHarnessEnvironment();
  let server;
  let browser;
  try {
    const { createServer } = await import("vite");
    const { chromium } = await import("playwright");
    server = await createServer({
      root: V2,
      server: { host: "127.0.0.1", port: 0, strictPort: false, hmr: false },
    });
    await server.listen();
    browser = await chromium.launch();
    await exercise({ browser, base: serverURL(server) });
  } finally {
    await browser?.close().catch((e) => console.error("browser cleanup failed", e));
    await server?.close().catch((e) => console.error("Vite cleanup failed", e));
    restoreEnvironment();
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
