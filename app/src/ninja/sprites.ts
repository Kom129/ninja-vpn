// Кадры ниндзя: стиль A (мягкое 3D), сгенерированы Codex по утверждённому эскизу
// docs/design/ninja-concepts/concept-a.png, нарезаны scripts/build-sprites.mjs.
// У каждого кадра есть «точка опоры» (ax, ay): центр корпуса по горизонтали и низ ступней.
// Её ставим ровно на нить — тогда фигура не дёргается при смене кадров.

import meta from '../assets/ninja/ninja.json';

export const RUN_FRAMES = 6;
export type RunPose = `run-${0 | 1 | 2 | 3 | 4 | 5}`;
export type GuardPose = 'guard-idle' | 'guard-lunge' | 'guard-stumble' | 'guard-fallen' | 'guard-caught' | 'guard-carry';
export type Pose = 'idle' | 'jump' | 'slide' | 'deliver' | 'sad' | 'flip' | 'lean' | 'dash' | RunPose | GuardPose;

interface Frame {
  img: HTMLImageElement;
  w: number;
  h: number;
  ax: number;
  ay: number;
}

/** Файлы кадров вдвое крупнее, чем показываются: чёткость на экранах с масштабом 200 %. */
const FILE_TO_SCREEN = 0.5;

const urls = import.meta.glob('../assets/ninja/*.webp', { eager: true, query: '?url', import: 'default' }) as Record<string, string>;

const frames = new Map<string, Frame>();
const loading: Promise<unknown>[] = [];
for (const [name, m] of Object.entries(meta as Record<string, { w: number; h: number; ax: number; ay: number }>)) {
  const img = new Image();
  img.src = urls[`../assets/ninja/${name}.webp`];
  loading.push(img.decode().catch(() => undefined));
  frames.set(name, { img, ...m });
}

/** Все кадры загружены — после этого сцену стоит перерисовать. */
export const spritesReady = Promise.all(loading);

/** Высота кадра на экране при данном размере. */
export function poseHeight(pose: Pose, size: number) {
  return (frames.get(pose)?.h ?? 0) * FILE_TO_SCREEN * size;
}

export interface DrawOptions {
  /** Размер относительно базового (зависит от ширины окна). */
  size: number;
  /** 1 — смотрит вправо, -1 — влево. */
  facing?: number;
  opacity?: number;
  /** Наклон в радианах вокруг точки опоры. */
  lean?: number;
  /** Вращение в радианах вокруг середины фигуры — для сальто. */
  spin?: number;
  /** Сплющивание при ударе о землю: 0.1 — на 10 % ниже и шире. */
  squash?: number;
}

export function drawPose(ctx: CanvasRenderingContext2D, pose: Pose, x: number, y: number, o: DrawOptions) {
  const f = frames.get(pose);
  if (!f || !f.img.complete || f.img.naturalWidth === 0) return;
  const s = o.size * FILE_TO_SCREEN;
  ctx.save();
  ctx.globalAlpha *= Math.max(0, Math.min(1, o.opacity ?? 1));
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = 'high';
  ctx.translate(x, y);
  ctx.rotate(o.lean ?? 0);
  if (o.spin) {
    const mid = (f.ay - f.h / 2) * s;
    ctx.translate(0, -mid);
    ctx.rotate(o.spin * (o.facing ?? 1));
    ctx.translate(0, mid);
  }
  const squash = o.squash ?? 0;
  ctx.scale((o.facing ?? 1) * s * (1 + squash), s * (1 - squash));
  ctx.drawImage(f.img, -f.ax, -f.ay, f.w, f.h);
  ctx.restore();
}

/** Кадр бега по фазе шага (радианы; полный круг — 6 кадров). */
export function runPose(gait: number): RunPose {
  const i = Math.floor((((gait / (Math.PI * 2)) % 1) + 1) % 1 * RUN_FRAMES) % RUN_FRAMES;
  return `run-${i as 0 | 1 | 2 | 3 | 4 | 5}`;
}
