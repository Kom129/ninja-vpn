// Превращает листы с кадрами ниндзя (сгенерированы Codex) в готовые кадры для приложения.
//
//   node scripts/build-sprites.mjs
//
// Вход (прозрачный фон, фигуры в один ряд, могут касаться друг друга шарфом или хвостом):
//   docs/design/ninja-concepts/run-cycle.png — бег, 6 кадров
//   docs/design/ninja-concepts/poses.png     — idle, jump, slide, deliver, sad
//   docs/design/ninja-concepts/dodges.png    — стоит (только для масштаба), сальто, уклон, рывок
//   docs/design/ninja-concepts/guards.png    — страж в маске: стоит, бросается, спотыкается, упал
//   docs/design/ninja-concepts/caught.png    — страж поймал её (держит за шарф) и тащит под мышкой
// Выход:
//   src/assets/ninja/*.webp + ninja.json (размер кадра и «точка опоры»: центр корпуса и низ ступней)
//   docs/design/ninja-concepts/sprites-preview.png — все кадры на одной линии, для проверки глазами
//
// Как фигуры отделяются друг от друга: сначала находим «ядра» — плотные непрозрачные области
// (по одной на фигуру), потом от всех ядер одновременно «растекаемся» по полупрозрачным пикселям.
// Каждый пиксель достаётся той фигуре, чья волна дошла до него первой — даже если шарф одной
// фигуры касается ботинка соседней, граница проходит по месту встречи.

import sharp from 'sharp';
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const app = join(dirname(fileURLToPath(import.meta.url)), '..');
const srcDir = join(app, '..', 'docs', 'design', 'ninja-concepts');
const out = join(app, 'src', 'assets', 'ninja');

/** Рост стоящей ниндзя в пикселях файла (на экране — вдвое меньше: запас для чёткости на 4K). */
const STAND_HEIGHT = 176;
const PAD = 3;
/** Рост стража: крупнее ниндзя, но так, чтобы с прыжком помещался над нитью. */
const GUARD_HEIGHT = Math.round(STAND_HEIGHT * 1.3);
/** Лист бега Codex нарисовал чуть мельче листа поз: подогнано на глаз по размеру головы (scale-check). */
const RUN_SCALE_FIX = 1.1;
const CORE_ALPHA = 200;
const KEEP_ALPHA = 10;

async function load(file) {
  const { data, info } = await sharp(join(srcDir, file)).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
  return { data, w: info.width, h: info.height };
}

/** Разделить лист на `count` фигур. Возвращает для каждого пикселя номер фигуры (или -1). */
function segment(img, count) {
  const { data, w, h } = img;
  const n = w * h;
  const alpha = (p) => data[p * 4 + 3];

  // 1. Связные плотные области.
  const comp = new Int32Array(n).fill(-1);
  const areas = [];
  const queue = new Int32Array(n);
  for (let p = 0; p < n; p++) {
    if (comp[p] !== -1 || alpha(p) < CORE_ALPHA) continue;
    const id = areas.length;
    let head = 0, tail = 0, area = 0, sx = 0;
    queue[tail++] = p;
    comp[p] = id;
    while (head < tail) {
      const q = queue[head++];
      area++;
      sx += q % w;
      const x = q % w;
      for (const r of [q - 1, q + 1, q - w, q + w]) {
        if (r < 0 || r >= n || (r === q - 1 && x === 0) || (r === q + 1 && x === w - 1)) continue;
        if (comp[r] === -1 && alpha(r) >= CORE_ALPHA) {
          comp[r] = id;
          queue[tail++] = r;
        }
      }
    }
    areas.push({ id, area, cx: sx / area });
  }

  // 2. Ядра фигур — `count` самых больших областей, слева направо.
  const cores = [...areas].sort((a, b) => b.area - a.area).slice(0, count).sort((a, b) => a.cx - b.cx);
  if (cores.length < count) throw new Error(`нашёл только ${cores.length} фигур из ${count}`);
  const owner = new Int32Array(n).fill(-1);
  let head = 0, tail = 0;
  cores.forEach((c, figure) => {
    for (let p = 0; p < n; p++) {
      if (comp[p] === c.id) {
        owner[p] = figure;
        queue[tail++] = p;
      }
    }
  });

  // 3. Одновременное «растекание» от всех ядер по видимым пикселям.
  while (head < tail) {
    const q = queue[head++];
    const x = q % w;
    for (const r of [q - 1, q + 1, q - w, q + w]) {
      if (r < 0 || r >= n || (r === q - 1 && x === 0) || (r === q + 1 && x === w - 1)) continue;
      if (owner[r] === -1 && alpha(r) >= KEEP_ALPHA) {
        owner[r] = owner[q];
        queue[tail++] = r;
      }
    }
  }
  // Пиксели, не связанные ни с одной фигурой (мелкие соринки), остаются ничьими и выбрасываются.
  return owner;
}

/** Отдельная картинка фигуры: только её пиксели, остальное прозрачное. */
function extract(img, owner, figure) {
  const { data, w, h } = img;
  let x0 = w, y0 = h, x1 = -1, y1 = -1;
  for (let p = 0; p < w * h; p++) {
    if (owner[p] !== figure) continue;
    const x = p % w, y = (p / w) | 0;
    if (x < x0) x0 = x;
    if (x > x1) x1 = x;
    if (y < y0) y0 = y;
    if (y > y1) y1 = y;
  }
  const fw = x1 - x0 + 1, fh = y1 - y0 + 1;
  const buf = Buffer.alloc(fw * fh * 4);
  for (let y = y0; y <= y1; y++) {
    for (let x = x0; x <= x1; x++) {
      const p = y * w + x;
      if (owner[p] === figure) data.copy(buf, ((y - y0) * fw + (x - x0)) * 4, p * 4, p * 4 + 4);
    }
  }
  return { buf, w: fw, h: fh };
}

/** Центр корпуса: средняя полоса фигуры (без хвоста наверху и без ног внизу). */
function torsoX(fig) {
  let sum = 0, cnt = 0;
  for (let y = Math.round(fig.h * 0.42); y <= Math.round(fig.h * 0.68); y++) {
    for (let x = 0; x < fig.w; x++) {
      if (fig.buf[(y * fig.w + x) * 4 + 3] > 128) {
        sum += x;
        cnt++;
      }
    }
  }
  return cnt ? sum / cnt : fig.w / 2;
}

/** Положить фигуру на холст w×h так, чтобы её точка (ax, ay) оказалась в (tx, ty). */
async function place(fig, ax, ay, w, h, tx, ty) {
  return sharp({ create: { width: w, height: h, channels: 4, background: { r: 0, g: 0, b: 0, alpha: 0 } } })
    .composite([{ input: fig.buf, raw: { width: fig.w, height: fig.h, channels: 4 }, left: Math.round(tx - ax), top: Math.round(ty - ay) }])
    .raw()
    .toBuffer();
}

async function save(name, raw, w, h, scale, ax, ay, meta) {
  const ow = Math.round(w * scale), oh = Math.round(h * scale);
  await sharp(raw, { raw: { width: w, height: h, channels: 4 } })
    .resize(ow, oh, { kernel: 'lanczos3' })
    .webp({ quality: 92, alphaQuality: 100, effort: 6 })
    .toFile(join(out, `${name}.webp`));
  meta[name] = { w: ow, h: oh, ax: Math.round(ax * scale), ay: Math.round(ay * scale) };
}

/** Лист отдельных поз: масштаб такой, чтобы первая (стоящая) фигура была ростом `height`.
 *  Пустое имя — фигура нужна только для масштаба и в приложение не попадает. */
async function sheet(file, names, height, meta) {
  const img = await load(file);
  const owner = segment(img, names.length);
  const figs = names.map((_, i) => extract(img, owner, i));
  const scale = height / figs[0].h;
  for (const [i, name] of names.entries()) {
    if (!name) continue;
    const fig = figs[i];
    const w = fig.w + PAD * 2, h = fig.h + PAD * 2;
    const raw = await place(fig, 0, 0, w, h, PAD, PAD);
    await save(name, raw, w, h, scale, torsoX(fig) + PAD, fig.h + PAD, meta);
  }
}

async function main() {
  mkdirSync(out, { recursive: true });
  const meta = {};

  // ——— Позы: каждая отдельно, опора — центр корпуса и низ ступней ———
  const poses = await load('poses.png');
  const poseNames = ['idle', 'jump', 'slide', 'deliver', 'sad'];
  const poseOwner = segment(poses, poseNames.length);
  const poseFigs = poseNames.map((_, i) => extract(poses, poseOwner, i));
  const scale = STAND_HEIGHT / poseFigs[0].h;
  for (const [i, name] of poseNames.entries()) {
    const fig = poseFigs[i];
    const w = fig.w + PAD * 2, h = fig.h + PAD * 2;
    const raw = await place(fig, 0, 0, w, h, PAD, PAD);
    await save(name, raw, w, h, scale, torsoX(fig) + PAD, fig.h + PAD, meta);
  }

  // ——— Увороты: первая фигура листа стоит ровно — по ней подгоняем масштаб к остальным кадрам ———
  await sheet('dodges.png', ['', 'flip', 'lean', 'dash'], STAND_HEIGHT, meta);
  // ——— Стражи в масках: масштаб по стоящему ———
  await sheet('guards.png', ['guard-idle', 'guard-lunge', 'guard-stumble', 'guard-fallen'], GUARD_HEIGHT, meta);
  // ——— Поймали: страж того же роста, что и в остальных кадрах ———
  await sheet('caught.png', ['guard-caught', 'guard-carry'], GUARD_HEIGHT, meta);

  // ——— Бег: общий холст на все кадры. По горизонтали совмещаем корпуса (чтобы фигура
  //     не «дрожала»), по вертикали — ступни самого низкого кадра (подпрыгивание из рисунка
  //     сохраняется: все кадры стоят на одной земле листа) ———
  const run = await load('run-cycle.png');
  const runOwner = segment(run, 6);
  const runFigs = [...Array(6)].map((_, i) => {
    const fig = extract(run, runOwner, i);
    // Где фигура стояла на листе по вертикали: низ её рамки.
    let bottom = 0;
    for (let p = 0; p < run.w * run.h; p++) if (runOwner[p] === i) bottom = Math.max(bottom, (p / run.w) | 0);
    return { ...fig, ax: torsoX(fig), bottom };
  });
  const ground = Math.max(...runFigs.map((f) => f.bottom));
  const left = Math.max(...runFigs.map((f) => f.ax)) + PAD;
  const right = Math.max(...runFigs.map((f) => f.w - f.ax)) + PAD;
  const top = Math.max(...runFigs.map((f) => f.h + (ground - f.bottom))) + PAD;
  const cw = Math.ceil(left + right), ch = Math.ceil(top + PAD);
  for (const [i, fig] of runFigs.entries()) {
    // Точка опоры фигуры (центр корпуса, низ рамки) ставится на (left, top - отрыв от земли).
    const lift = ground - fig.bottom;
    const raw = await place(fig, fig.ax, fig.h, cw, ch, left, top - lift);
    await save(`run-${i}`, raw, cw, ch, scale * RUN_SCALE_FIX, left, top, meta);
  }

  writeFileSync(join(out, 'ninja.json'), JSON.stringify(meta, null, 2) + '\n');
  for (const [k, v] of Object.entries(meta)) console.log(k.padEnd(8), `${v.w}×${v.h}`, 'опора', v.ax, v.ay);

  // ——— Контрольный лист: все кадры на одной линии земли (бирюзовая линия) ———
  const names = Object.keys(meta);
  const gap = 20, groundY = 220;
  const width = names.reduce((s, n) => s + meta[n].w + gap, gap);
  const layers = [];
  let x = gap;
  for (const n of names) {
    layers.push({ input: join(out, `${n}.webp`), left: x, top: groundY - meta[n].ay });
    layers.push({ input: Buffer.from(`<svg width="${meta[n].w}" height="2"><rect width="100%" height="2" fill="#2bb7c4"/></svg>`), left: x, top: groundY });
    layers.push({ input: Buffer.from(`<svg width="2" height="12"><rect width="2" height="12" fill="#e2394b"/></svg>`), left: x + meta[n].ax - 1, top: groundY + 3 });
    x += meta[n].w + gap;
  }
  await sharp({ create: { width, height: groundY + 24, channels: 4, background: '#e4f2f9' } })
    .composite(layers)
    .png()
    .toFile(join(srcDir, 'sprites-preview.png'));
  console.log('контрольный лист: docs/design/ninja-concepts/sprites-preview.png');
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
