// Rasterize the lotus mark into every icon file the bundle uses.
//   node src-tauri/tools/icon/render.mjs            → writes src-tauri/icons/*
//   node src-tauri/tools/icon/render.mjs --preview  → writes a contact sheet only
// Uses the Playwright Chromium already installed for the e2e tests.
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildSvg, PHI } from "./lotus.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const icons = path.resolve(here, "../../icons");
const preview = process.argv.includes("--preview");

/** Below 48 px the gaps would blur shut: keep them at least this many pixels wide. */
const minGap = (size) => (size <= 16 ? 1.25 : size <= 24 ? 1.4 : size <= 32 ? 1.6 : size <= 48 ? 1.8 : 0);

function findChromium() {
  const root = path.join(process.env.LOCALAPPDATA || "", "ms-playwright");
  const dirs = fs.existsSync(root) ? fs.readdirSync(root).filter((d) => /^chromium-\d+$/.test(d)).sort().reverse() : [];
  for (const d of dirs) {
    const exe = path.join(root, d, "chrome-win64", "chrome.exe");
    if (fs.existsSync(exe)) return exe;
  }
  return undefined;
}

const browser = await chromium.launch({ executablePath: findChromium() });
const page = await browser.newPage();

async function png(size) {
  // Small icons get almost no padding from Windows, so the mark fills 1 − 1/φ⁶ of them.
  const svg = buildSvg({ size, minGapPx: minGap(size), fill: size <= 32 ? 1 - 1 / Math.pow(PHI, 6) : undefined });
  await page.setViewportSize({ width: size, height: size });
  await page.setContent(`<style>html,body{margin:0;background:transparent}</style>${svg}`);
  return page.locator("svg").screenshot({ omitBackground: true, type: "png" });
}

/** ICO with PNG-compressed entries (Windows Vista and later read these at every size). */
function ico(entries) {
  const head = Buffer.alloc(6 + 16 * entries.length);
  head.writeUInt16LE(0, 0);
  head.writeUInt16LE(1, 2);
  head.writeUInt16LE(entries.length, 4);
  let offset = head.length;
  entries.forEach(({ size, data }, i) => {
    const o = 6 + 16 * i;
    head.writeUInt8(size >= 256 ? 0 : size, o);
    head.writeUInt8(size >= 256 ? 0 : size, o + 1);
    head.writeUInt16LE(1, o + 4);
    head.writeUInt16LE(32, o + 6);
    head.writeUInt32LE(data.length, o + 8);
    head.writeUInt32LE(offset, o + 12);
    offset += data.length;
  });
  return Buffer.concat([head, ...entries.map((e) => e.data)]);
}

if (preview) {
  const out = path.resolve(here, "preview");
  fs.mkdirSync(out, { recursive: true });
  const sizes = [16, 24, 32, 48, 64, 128, 256];
  const cells = [];
  for (const s of sizes) cells.push({ s, b64: (await png(s)).toString("base64") });
  const logo = fs.readFileSync(path.resolve(here, "../../../../lotusproxy_logo.png")).toString("base64");
  const row = (bg) =>
    `<div style="display:flex;gap:24px;align-items:flex-end;padding:24px;background:${bg}">` +
    cells.map((c) => `<img src="data:image/png;base64,${c.b64}" width="${c.s}" height="${c.s}">`).join("") +
    `</div>`;
  const zoom = cells.filter((c) => c.s <= 32).map((c) => `<img src="data:image/png;base64,${c.b64}" style="width:${c.s * 8}px;image-rendering:pixelated;background:#fff">`).join("");
  await page.setViewportSize({ width: 1200, height: 1100 });
  await page.setContent(
    `<body style="margin:0;font:13px sans-serif">${row("#ffffff")}${row("#202020")}` +
      `<div style="display:flex;gap:24px;padding:24px;background:#eee;align-items:flex-start">` +
      `<img src="data:image/png;base64,${logo}" width="320"><div style="background:#fff">${buildSvg({ size: 320 })}</div>${zoom}</div></body>`,
  );
  await page.screenshot({ path: path.join(out, "sheet.png"), fullPage: true });
  fs.writeFileSync(path.join(out, "icon.svg"), buildSvg({ size: 512 }));
  console.log("preview →", path.join(out, "sheet.png"));
} else {
  const plain = { "32x32.png": 32, "64x64.png": 64, "128x128.png": 128, "128x128@2x.png": 256, "icon.png": 512,
    "Square30x30Logo.png": 30, "Square44x44Logo.png": 44, "Square71x71Logo.png": 71, "Square89x89Logo.png": 89,
    "Square107x107Logo.png": 107, "Square142x142Logo.png": 142, "Square150x150Logo.png": 150,
    "Square284x284Logo.png": 284, "Square310x310Logo.png": 310, "StoreLogo.png": 50, "icon-source.png": 1024 };
  for (const [name, size] of Object.entries(plain)) fs.writeFileSync(path.join(icons, name), await png(size));
  const entries = [];
  for (const size of [16, 20, 24, 32, 40, 48, 64, 128, 256]) entries.push({ size, data: await png(size) });
  fs.writeFileSync(path.join(icons, "icon.ico"), ico(entries));
  fs.writeFileSync(path.join(icons, "icon-source.svg"), buildSvg({ size: 512 }));
  // The title bar's brand mark, tuned for 16 px like the small icon files.
  fs.writeFileSync(path.resolve(here, "../../../shell/lotus-16.svg"), buildSvg({ size: 16, minGapPx: minGap(16), fill: 1 - 1 / Math.pow(PHI, 6) }));
  console.log("icons →", icons);
}
await browser.close();
