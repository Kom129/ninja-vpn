// Готовит иконку приложения из картинки Codex (docs/design/icon/icon-a.png):
//  1. убирает «мусор» вокруг плитки — бледные точки и полоски, не связанные с рисунком;
//  2. делает плитку непрозрачной: у Codex стекло чуть просвечивает (альфа ~252),
//     и на панели задач сквозь иконку проступал бы фон;
//  3. сохраняет app/src-tauri/icons/app-icon.png (1024×1024), из которого
//     `npx tauri icon` делает все размеры (.ico для Windows и PNG).
//
// Запуск: cd app && node scripts/build-icon.mjs && npx tauri icon src-tauri/icons/app-icon.png

import sharp from 'sharp';
import path from 'node:path';

const ROOT = path.resolve(import.meta.dirname, '..', '..');
const SOURCE = path.join(ROOT, 'docs', 'design', 'icon', 'icon-a.png');
const OUT = path.join(ROOT, 'app', 'src-tauri', 'icons', 'app-icon.png');
const PREVIEW = path.join(ROOT, 'docs', 'design', 'icon', 'icon-sizes-preview.png');

/** Ниже этой прозрачности пиксель — тень или мусор, а не рисунок. */
const NOISE_ALPHA = 24;
/** Почти непрозрачное (стекло плитки) делаем полностью непрозрачным. */
const SOLID_ALPHA = 200;

const { data, info } = await sharp(SOURCE).ensureAlpha().resize(1024, 1024).raw().toBuffer({ resolveWithObject: true });
const { width: W, height: H } = info;
const alpha = (i) => data[i * 4 + 3];

// Связные области заметных пикселей: оставляем только самую большую — плитку с рисунком.
const label = new Int32Array(W * H).fill(-1);
const sizes = [];
for (let start = 0; start < W * H; start++) {
  if (label[start] !== -1 || alpha(start) < NOISE_ALPHA) continue;
  const id = sizes.length;
  let count = 0;
  const stack = [start];
  label[start] = id;
  while (stack.length) {
    const p = stack.pop();
    count++;
    const x = p % W, y = (p / W) | 0;
    for (const [dx, dy] of [[1, 0], [-1, 0], [0, 1], [0, -1]]) {
      const nx = x + dx, ny = y + dy;
      if (nx < 0 || ny < 0 || nx >= W || ny >= H) continue;
      const q = ny * W + nx;
      if (label[q] === -1 && alpha(q) >= NOISE_ALPHA) {
        label[q] = id;
        stack.push(q);
      }
    }
  }
  sizes.push(count);
}
const main = sizes.indexOf(Math.max(...sizes));
let removed = 0, solid = 0;
for (let p = 0; p < W * H; p++) {
  const a = p * 4 + 3;
  if (label[p] !== main) {
    if (data[a] > 0) removed++;
    data[a] = 0;
  } else if (data[a] >= SOLID_ALPHA && data[a] < 255) {
    data[a] = 255;
    solid++;
  }
}
await sharp(data, { raw: { width: W, height: H, channels: 4 } }).png({ compressionLevel: 9 }).toFile(OUT);
console.log(`областей: ${sizes.length}, оставлена самая большая (${sizes[main]} пикс.)`);
console.log(`убрано мусорных пикселей: ${removed}, сделано непрозрачными: ${solid}`);
console.log(`готово: ${path.relative(ROOT, OUT)}`);

// Контрольный лист: как иконка выглядит в настоящих размерах на тёмном и светлом фоне.
const sizesToShow = [256, 64, 48, 32, 24, 16];
const tiles = [];
let x = 0;
for (const size of sizesToShow) {
  const icon = await sharp(OUT).resize(size, size, { kernel: 'lanczos3' }).png().toBuffer();
  tiles.push({ input: icon, left: x + 12, top: 12 + (256 - size) / 2 });
  tiles.push({ input: icon, left: x + 12, top: 292 + (256 - size) / 2 });
  x += size + 24;
}
await sharp({ create: { width: x, height: 560, channels: 4, background: '#1f2937' } })
  .composite([{ input: { create: { width: x, height: 280, channels: 4, background: '#f3f4f6' } }, left: 0, top: 280 }, ...tiles])
  .png()
  .toFile(PREVIEW);
console.log(`контрольный лист: ${path.relative(ROOT, PREVIEW)}`);
