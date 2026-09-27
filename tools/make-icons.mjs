// Generates the app icons (PNG + ICO) with no external dependencies.
// Run: node tools/make-icons.mjs
import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const OUT = join(dirname(fileURLToPath(import.meta.url)), "..", "crates", "swarmo-app", "src-tauri", "icons");
mkdirSync(OUT, { recursive: true });

function crc32(buf) {
  let c, table = [];
  for (let n = 0; n < 256; n++) {
    c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  let crc = 0xffffffff;
  for (const b of buf) crc = table[(crc ^ b) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}

/** A rounded-square mark: a swarm of dots converging on a target. */
function pixels(size) {
  const raw = Buffer.alloc(size * (size * 4 + 1));
  const r = size / 2;
  const radius = size * 0.22;

  const dots = [
    [0.5, 0.5, 0.155],
    [0.26, 0.3, 0.062],
    [0.74, 0.28, 0.05],
    [0.2, 0.68, 0.05],
    [0.78, 0.7, 0.062],
    [0.5, 0.16, 0.042],
    [0.5, 0.85, 0.042],
  ];

  for (let y = 0; y < size; y++) {
    const rowStart = y * (size * 4 + 1);
    raw[rowStart] = 0; // filter: none
    for (let x = 0; x < size; x++) {
      const i = rowStart + 1 + x * 4;

      // Rounded-square background.
      const dx = Math.max(Math.abs(x - r + 0.5) - (r - radius), 0);
      const dy = Math.max(Math.abs(y - r + 0.5) - (r - radius), 0);
      const outside = Math.sqrt(dx * dx + dy * dy) - radius;
      const bgAlpha = Math.max(0, Math.min(1, 0.5 - outside));
      if (bgAlpha <= 0) continue;

      // Vertical indigo gradient.
      const t = y / size;
      let cr = Math.round(79 + t * 30);
      let cg = Math.round(70 + t * 20);
      let cb = Math.round(229 - t * 40);

      // Paint the dots in near-white.
      let dotA = 0;
      for (const [fx, fy, fr] of dots) {
        const cx = fx * size, cy = fy * size, rr = fr * size;
        const d = Math.hypot(x + 0.5 - cx, y + 0.5 - cy) - rr;
        dotA = Math.max(dotA, Math.max(0, Math.min(1, 0.5 - d)));
      }
      if (dotA > 0) {
        cr = Math.round(cr + (250 - cr) * dotA);
        cg = Math.round(cg + (250 - cg) * dotA);
        cb = Math.round(cb + (255 - cb) * dotA);
      }

      raw[i] = cr;
      raw[i + 1] = cg;
      raw[i + 2] = cb;
      raw[i + 3] = Math.round(bgAlpha * 255);
    }
  }
  return raw;
}

function png(size) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8;  // bit depth
  ihdr[9] = 6;  // RGBA
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(pixels(size), { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/** An ICO whose entries are embedded PNGs (supported since Vista). */
function ico(sizes) {
  const images = sizes.map((s) => ({ size: s, data: png(s) }));
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(images.length, 4);

  let offset = 6 + images.length * 16;
  const entries = [];
  for (const img of images) {
    const e = Buffer.alloc(16);
    e[0] = img.size >= 256 ? 0 : img.size;
    e[1] = img.size >= 256 ? 0 : img.size;
    e[2] = 0;
    e[3] = 0;
    e.writeUInt16LE(1, 4);
    e.writeUInt16LE(32, 6);
    e.writeUInt32LE(img.data.length, 8);
    e.writeUInt32LE(offset, 12);
    offset += img.data.length;
    entries.push(e);
  }
  return Buffer.concat([header, ...entries, ...images.map((i) => i.data)]);
}

for (const [name, size] of [
  ["32x32.png", 32],
  ["128x128.png", 128],
  ["128x128@2x.png", 256],
  ["icon.png", 512],
  ["Square30x30Logo.png", 30],
  ["Square44x44Logo.png", 44],
  ["Square71x71Logo.png", 71],
  ["Square89x89Logo.png", 89],
  ["Square107x107Logo.png", 107],
  ["Square142x142Logo.png", 142],
  ["Square150x150Logo.png", 150],
  ["Square284x284Logo.png", 284],
  ["Square310x310Logo.png", 310],
  ["StoreLogo.png", 50],
]) {
  writeFileSync(join(OUT, name), png(size));
}
writeFileSync(join(OUT, "icon.ico"), ico([16, 32, 48, 64, 256]));
// macOS bundling needs an .icns; Tauri accepts a PNG fallback for dev builds.
writeFileSync(join(OUT, "icon.icns"), png(512));

console.log("icons written to", OUT);
