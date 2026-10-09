// «Окна в страны»: картинки мест от Codex → лёгкие файлы для окна программы.
//   docs/design/places/<код>.png (nl.png, de.png…, world.png) → src/assets/places/<код>.webp
// Квадрат 256×256 (кружок в окне — 94 px, с запасом на экраны с увеличением), обрезка по центру.
// Заодно — копия 768 px в docs/design/places/archive/: она хранится в Git вместо тяжёлого PNG
// (и из неё можно собрать картинки заново, если PNG нет).
// Запуск из app: node scripts/build-places.mjs

import sharp from 'sharp';
import { mkdirSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

const root = join(import.meta.dirname, '..', '..');
const src = join(root, 'docs', 'design', 'places');
const out = join(import.meta.dirname, '..', 'src', 'assets', 'places');
const archive = join(src, 'archive');
mkdirSync(out, { recursive: true });
mkdirSync(archive, { recursive: true });

// Только файлы с кодом страны (две буквы) или world — листы выбора стиля и примеры пропускаем.
const pngs = readdirSync(src).filter((f) => /^([a-z]{2}|world)\.png$/.test(f));
const archived = readdirSync(archive).filter((f) => /^([a-z]{2}|world)\.webp$/.test(f) && !pngs.includes(f.replace('.webp', '.png')));
const files = [...pngs.map((f) => join(src, f)), ...archived.map((f) => join(archive, f))];
for (const file of files) {
  const code = file.match(/([a-z]+)\.(png|webp)$/)[1];
  if (file.endsWith('.png')) {
    await sharp(file).resize(768, 768, { fit: 'cover' }).webp({ quality: 88, effort: 6 }).toFile(join(archive, `${code}.webp`));
  }
  const info = await sharp(file)
    .resize(256, 256, { fit: 'cover', position: 'centre' })
    .webp({ quality: 82, effort: 6 })
    .toFile(join(out, `${code}.webp`));
  console.log(`${code}: ${(info.size / 1024).toFixed(1)} КБ`);
}
console.log(`Готово: ${files.length} картинок в src/assets/places`);
