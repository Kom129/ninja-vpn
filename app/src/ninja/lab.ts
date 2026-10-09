// Раскадровка сцены — только для разработки (http://127.0.0.1:1420/scene-lab.html).
// Прогоняет сцену по точным часам (60 кадров в секунду) по нескольким сценариям подключения
// и рисует выбранные мгновения сеткой: всю погоню видно на одном снимке экрана.
// В сборку приложения не попадает: Vite собирает только index.html.

import { NinjaScene, type Phase } from './scene';
import { spritesReady } from './sprites';

/** Геометрия как в окне 1180×760: см. App.tsx (ORB_CENTER_X + ORB_RADIUS + CHEVRON_SPACE и т. д.). */
const W = 760;
const H = 226;
const GEO = { width: W, height: H, left: 184, right: W - 136, base: 112 };

interface Scenario {
  title: string;
  /** Когда (секунды) какой этап сообщает мотор. */
  events: [number, Phase][];
  /** В какие мгновения снять кадр. */
  shots: number[];
}

const range = (from: number, to: number, step: number) => {
  const out: number[] = [];
  for (let t = from; t <= to + 1e-9; t += step) out.push(Math.round(t * 100) / 100);
  return out;
};

const SCENARIOS: Scenario[] = [
  {
    title: 'Долгая проверка (8 с), потом «подключено»',
    events: [[0, 'preparing'], [0.3, 'connecting'], [0.9, 'verifying'], [8, 'connected']],
    shots: [...range(0.4, 2.8, 0.2), ...range(3.4, 7.8, 0.4), ...range(8.1, 9.3, 0.2)],
  },
  {
    title: 'Быстрое подключение (0,6 с) — один страж всё равно бросается',
    events: [[0, 'preparing'], [0.2, 'connecting'], [0.4, 'verifying'], [0.6, 'connected']],
    shots: range(0.3, 2.5, 0.2),
  },
  {
    title: 'Ошибка после 4,5 с — страж у ворот ловит её и тащит к началу',
    events: [[0, 'preparing'], [0.3, 'connecting'], [0.9, 'verifying'], [4.5, 'failed']],
    shots: range(4.6, 9.6, 0.3),
  },
  {
    title: 'Связь была и пропала — страж выходит из тумана, уже держа её',
    events: [[0, 'preparing'], [0.2, 'connecting'], [0.4, 'verifying'], [0.6, 'connected'], [4, 'failed']],
    shots: range(4.1, 8.6, 0.3),
  },
];

function render(scenario: Scenario, root: HTMLElement) {
  const h = document.createElement('h2');
  h.textContent = scenario.title;
  const strip = document.createElement('div');
  strip.className = 'strip';
  root.append(h, strip);

  const scene = new NinjaScene();
  const work = document.createElement('canvas');
  work.width = W * 2;
  work.height = H * 2;
  const ctx = work.getContext('2d')!;
  ctx.setTransform(2, 0, 0, 2, 0, 0);

  const dt = 1 / 60;
  const end = Math.max(...scenario.shots);
  const events = [...scenario.events];
  const shots = new Set(scenario.shots.map((s) => Math.round(s * 60)));
  for (let frame = 0; frame * dt <= end + dt; frame++) {
    const now = frame * dt;
    while (events.length && events[0][0] <= now + 1e-9) scene.setPhase(events.shift()![1]);
    scene.update(dt);
    scene.draw(ctx, GEO);
    if (!shots.has(frame)) continue;
    const fig = document.createElement('figure');
    const shot = document.createElement('canvas');
    shot.width = W;
    shot.height = H;
    shot.getContext('2d')!.drawImage(work, 0, 0, W, H);
    const cap = document.createElement('figcaption');
    cap.textContent = `${now.toFixed(1)} с`;
    fig.append(shot, cap);
    strip.append(fig);
  }
}

spritesReady.then(() => {
  const root = document.getElementById('root')!;
  // ?s=1 — только второй сценарий (номера с нуля): так кадры крупнее на одном снимке.
  const only = new URLSearchParams(location.search).get('s');
  // ?pick=1.0,2.4 — только эти мгновения, крупно в две колонки (для показа).
  const pick = new URLSearchParams(location.search).get('pick');
  if (pick) document.querySelector('style')!.append('.strip { grid-template-columns: repeat(2, 1fr) !important; }');
  SCENARIOS.forEach((s, i) => {
    if (only !== null && Number(only) !== i) return;
    render(pick ? { ...s, shots: pick.split(',').map(Number) } : s, root);
  });
  document.body.dataset.ready = '1';
});
