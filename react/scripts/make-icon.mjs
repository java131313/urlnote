// 生成 Tauri 图标源图 icons-src.png(1024x1024,无依赖手写 PNG)。
// 运行:node scripts/make-icon.mjs
import { deflateSync } from 'node:zlib';
import { writeFileSync } from 'node:fs';

const W = 1024, H = 1024;
const px = new Uint8Array(W * H * 4);

// 调色板
const BG = [79, 70, 229, 255]; // 靛蓝底
const CARD = [250, 250, 250, 255];
const LINE = [148, 163, 184, 255];
const ACCENT = [79, 70, 229, 255];

function set(x, y, c) {
  if (x < 0 || y < 0 || x >= W || y >= H) return;
  const i = (y * W + x) * 4;
  px[i] = c[0]; px[i + 1] = c[1]; px[i + 2] = c[2]; px[i + 3] = c[3];
}
function roundedRect(x0, y0, x1, y1, r, c) {
  for (let y = y0; y < y1; y++)
    for (let x = x0; x < x1; x++) {
      const cx = x < x0 + r ? x0 + r : x >= x1 - r ? x1 - r - 1 : x;
      const cy = y < y0 + r ? y0 + r : y >= y1 - r ? y1 - r - 1 : y;
      if ((x - cx) ** 2 + (y - cy) ** 2 <= r * r) set(x, y, c);
    }
}

// 底 + 便签卡片 + 三行文字 + 右下角强调块
px.fill(0);
for (let i = 0; i < W * H; i++) px.set(BG, i * 4);
roundedRect(300, 240, 724, 784, 44, CARD);
roundedRect(352, 372, 672, 406, 8, LINE);
roundedRect(352, 476, 672, 510, 8, LINE);
roundedRect(352, 580, 560, 614, 8, LINE);
roundedRect(596, 664, 692, 760, 20, ACCENT);

// ---- PNG 编码 ----
const crcTable = new Int32Array(256).map((_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c;
});
const crc32 = (buf) => {
  let c = -1;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
};
function chunk(type, data) {
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type), data]);
  const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(W, 0); ihdr.writeUInt32BE(H, 4);
ihdr[8] = 8; ihdr[9] = 6; // 8bit RGBA
const raw = Buffer.alloc(H * (1 + W * 4));
for (let y = 0; y < H; y++) {
  raw[y * (1 + W * 4)] = 0; // filter: none
  Buffer.from(px.buffer, y * W * 4, W * 4).copy(raw, y * (1 + W * 4) + 1);
}
const png = Buffer.concat([
  Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
  chunk('IHDR', ihdr),
  chunk('IDAT', deflateSync(raw, { level: 9 })),
  chunk('IEND', Buffer.alloc(0)),
]);
writeFileSync(new URL('../src-tauri/icons-src.png', import.meta.url), png);
console.log('icons-src.png written,', png.length, 'bytes');
