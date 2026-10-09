// «Режиссёр» сцены: что делает ниндзя на каждом этапе подключения.
//
// Главное правило из дизайн-брифа: анимация идёт за настоящими событиями мотора.
// Пока идёт проверка, ниндзя бежит и у последнего отрезка замедляется («ждёт у ворот»),
// но к серверу не приходит. Волна появляется только после события «подключено».
//
// По пути её пытаются поймать стражи в масках — крупные, но неуклюжие: тяжело бросаются,
// промахиваются, спотыкаются и шлёпаются, а она уходит подкатом и сальто. У ворот, пока
// идёт проверка, топчется ещё один и снова и снова пробует схватить — она уклоняется.
// Ждать конца сценки пользователь не должен: кнопка и надписи меняются сразу.

import { drawPose, poseHeight, runPose, type GuardPose, type Pose } from './sprites';

export type Phase =
  | 'disconnected'
  | 'preparing'
  | 'connecting'
  | 'verifying'
  | 'connected'
  | 'failed'
  | 'reconnecting'
  | 'disconnecting';

type Mode = 'idle' | 'run' | 'deliver' | 'wave' | 'fade' | 'fail' | 'broken';

/** Настройки волны из референса: скорость 1.3, высота 8 px. */
export const MOTION = { speed: 1.3, amplitude: 8 };

const clamp = (v: number, lo = 0, hi = 1) => Math.min(hi, Math.max(lo, v));
const smooth = (v: number) => {
  v = clamp(v);
  return v * v * (3 - 2 * v);
};
const smoother = (v: number) => {
  v = clamp(v);
  return v * v * v * (v * (v * 6 - 15) + 10);
};
const bump = (v: number, center: number, radius: number) => {
  const d = Math.abs(v - center) / radius;
  return d >= 1 ? 0 : Math.pow(0.5 + 0.5 * Math.cos(Math.PI * d), 2);
};
const lerp = (a: number, b: number, t: number) => a + (b - a) * t;

/** Где на холсте начинается и заканчивается путь (края кружков-«орбов»). */
export interface Geometry {
  width: number;
  height: number;
  /** Начало и конец нити. */
  left: number;
  right: number;
  /** Высота нити (центр орбов). */
  base: number;
}

interface G extends Geometry {
  start: number;
  end: number;
  /** Размер героини относительно базового: в узком окне чуть меньше. */
  size: number;
}

/** Длительность сцен в секундах. */
/** Общий размер героини и стражей (1 — кадры в половину своего размера в файле). */
const SCALE = 0.765;
const RUN_DELAY = 0.35;
const ARRIVAL = 1.2;
const FADE = 0.7;
const BREAK_AT = 0.4;
/** Полный круг бега (6 кадров) при полной скорости, секунды. */
const STRIDE = 0.6;

/** Где она на пути через `t` секунд бега: быстро стартует и замедляется у ворот (до 0,82 пути). */
const runProgress = (t: number) => 0.82 * (1 - Math.exp(-Math.max(0, t - RUN_DELAY) / 1.6));

type Dodge = 'dash' | 'flip';

/** Стражи на пути: через сколько секунд бега выходят из тумана и как она уходит от захвата.
 *  Время, а не место на пути: настоящее подключение бывает и за секунду, и за десять. */
const ENCOUNTERS: { spawn: number; dodge: Dodge }[] = [
  { spawn: 0.2, dodge: 'dash' }, // подкат под протянутыми руками
  { spawn: 1.6, dodge: 'flip' }, // сальто через того, кто бросился навстречу
];
/** Страж у ворот: где стоит, с какого места пути его видно, как часто пробует схватить (с). */
const GATE_AT = 0.99;
const GATE_SHOW = 0.7;
const GATE_CYCLE = 2.4;
/** Страж выходит из тумана, бросается, неуклюже промахивается, сидит, растворяется (секунды). */
const APPEAR = 0.35;
const LUNGE = 0.3;
const STUMBLE = 0.5;
const FALLEN_HOLD = 0.9;
const VANISH = 0.5;
/** Не дождался её — бросается сам через столько секунд после появления. */
const PATIENCE = 1.4;
/** Ошибка — её поймали: когда страж хватает, сколько держит (болтает ногами), сколько
 *  растворяется у начала. Если связь была и пропала — страж выходит из тумана, уже держа её. */
const CATCH_GRAB = 0.5;
const CATCH_APPEAR = 0.45;
const CATCH_HOLD = 1.3;
const CATCH_DROP = 0.6;
/** Её уворот: через сколько после броска начинается и сколько длится. */
const DODGE_DELAY = 0.04;
const DODGE_TIME = 0.56;

interface Guard {
  /** Где стоит — доля пути. */
  at: number;
  /** Часы сцены: когда вышел из тумана, когда бросился, когда ушёл не бросившись (NaN — ещё нет). */
  born: number;
  sprung: number;
  left: number;
  dodge: Dodge;
}

export class NinjaScene {
  private mode: Mode = 'idle';
  private t = 0;
  /** Часы сцены: идут всегда и не сбрасываются при смене этапа — по ним живут стражи. */
  private clock = 0;
  private wave = 0;
  private progress = 0;
  private gait = 0;
  private from = 0;
  private reduced = false;
  /** Длительность последнего кадра — чтобы шаги не зависели от частоты экрана. */
  private dt = 1 / 60;
  /** Стражи на пути и сколько из запланированных уже вышло. */
  private guards: Guard[] = [];
  private spawned = 0;
  /** Страж у ворот: с какого момента стоит, насколько виден, когда бросился в последний раз. */
  private gateSince = NaN;
  private gateShown = 0;
  private gateFall = NaN;

  setReducedMotion(reduced: boolean) {
    this.reduced = reduced;
  }

  /** Сообщить сцене настоящий этап подключения. */
  setPhase(phase: Phase) {
    const m = this.mode;
    switch (phase) {
      case 'preparing':
      case 'connecting':
      case 'verifying':
      case 'reconnecting':
        if (m !== 'run') this.enter('run', 0);
        break;
      case 'connected':
        if (m === 'run') this.enter('deliver', this.progress);
        else if (m !== 'deliver' && m !== 'wave') this.enter('wave', 1);
        break;
      case 'failed':
        if (m === 'run' || m === 'deliver') this.enter('fail', this.progress);
        else if (m === 'wave') this.enter('fail', 1);
        else if (m !== 'fail') this.enter('broken', 0);
        break;
      case 'disconnecting':
        if (m === 'wave' || m === 'deliver') this.enter('fade', 1);
        break;
      case 'disconnected':
        if (m === 'wave' || m === 'deliver') this.enter('fade', 1);
        else if (m !== 'fade') this.enter('idle', 0);
        break;
    }
  }

  /** Нужно ли продолжать перерисовку кадр за кадром. */
  get animating() {
    if (this.reduced) return false;
    return this.mode !== 'idle' && this.mode !== 'broken';
  }

  private enter(mode: Mode, from: number) {
    const was = this.mode;
    this.mode = mode;
    this.t = 0;
    this.from = from;
    if (mode === 'run' || mode === 'idle' || mode === 'broken') {
      this.progress = 0;
      this.guards = [];
      this.spawned = 0;
      this.gateSince = NaN;
      this.gateShown = 0;
      this.gateFall = NaN;
    }
    if (mode === 'deliver') {
      if (this.gateShown > 0) {
        // Страж у ворот бросается в последний раз — и промахивается.
        this.gateFall = this.clock;
      } else if (was === 'run' && this.guards.length === 0 && from < 0.75) {
        // Подключилось так быстро, что никто не успел выйти, — один страж всё равно попробует.
        this.guards.push({ at: from + 0.4 * (1 - from), born: this.clock - APPEAR * 0.7, sprung: this.clock + 0.06, left: NaN, dodge: 'dash' });
      }
    }
    if (mode === 'fail') {
      // Кто не успел броситься — уходит обратно в туман.
      for (const guard of this.guards) if (Number.isNaN(guard.sprung)) guard.left = this.clock;
    }
  }

  update(dt: number) {
    if (this.reduced) return;
    this.dt = dt;
    this.t += dt;
    this.clock += dt;
    this.wave += dt * MOTION.speed;
    if (this.mode === 'run') {
      while (this.spawned < ENCOUNTERS.length && this.t >= ENCOUNTERS[this.spawned].spawn) {
        const e = ENCOUNTERS[this.spawned++];
        // Встаёт туда, где она окажется чуть позже, — чтобы она успела к нему подбежать.
        const at = Math.min(0.76, runProgress(e.spawn + 0.75) + 0.04);
        this.guards.push({ at, born: this.clock, sprung: NaN, left: NaN, dodge: e.dodge });
      }
    }
    if (this.mode === 'deliver' && this.t >= this.deliverDuration() + ARRIVAL) this.enter('wave', 1);
    if (this.mode === 'fade' && this.t >= FADE) this.enter('idle', 0);
    if (this.mode === 'fail' && this.t >= this.failDuration()) this.enter('broken', 0);
  }

  private deliverDuration() {
    return 0.3 + 0.55 * (1 - this.from);
  }

  private failDuration() {
    return CATCH_HOLD + this.carryDuration() + CATCH_DROP;
  }

  /** Сколько страж тащит её обратно: чем дальше она успела убежать, тем дольше. */
  private carryDuration() {
    return Math.max(0.7, 2.2 * Math.min(1, this.from));
  }

  /** Шаг вперёд по кругу бега: чем быстрее бежит, тем чаще кадры. */
  private stride(run: number) {
    this.gait += this.dt * ((Math.PI * 2) / STRIDE) * clamp(run / 0.45, 0.35, 1);
  }

  private geometry(g: Geometry): G {
    // Рост героини ~68 px, стражей ~88 px (при кружках 104 px): на 15% меньше первой версии —
    // по просьбе автора (2026-10-06), чтобы сцена была просторнее, а кружки главнее.
    return { ...g, start: g.left + 26, end: g.right - 30, size: SCALE * clamp(g.width / 760, 0.85, 1.05) };
  }

  draw(ctx: CanvasRenderingContext2D, geo: Geometry) {
    const g = this.geometry(geo);
    ctx.clearRect(0, 0, g.width, g.height);
    if (this.reduced) return this.drawStill(ctx, g);

    switch (this.mode) {
      case 'idle':
        return this.drawIdle(ctx, g);
      case 'broken':
        breakThread(ctx, g, 3);
        return this.drawHero(ctx, g, 'sad', g.start, 0);
      case 'wave':
        wave(ctx, g, this.wave);
        // Отставшие стражи досиживают и растворяются.
        this.drawGuards(ctx, g, null);
        return this.drawGateFall(ctx, g);
      case 'fade':
        return wave(ctx, g, this.wave, 1 - smooth(this.t / FADE));
      case 'run':
        return this.drawRun(ctx, g);
      case 'deliver':
        return this.drawDeliver(ctx, g);
      case 'fail':
        return this.drawFail(ctx, g);
    }
  }

  /** Героиня с тенью: x — точка опоры, hop — насколько оторвалась от нити. */
  private drawHero(ctx: CanvasRenderingContext2D, g: G, pose: Pose, x: number, hop: number, facing = 1, opacity = 1, lean = 0, spin = 0) {
    shadow(ctx, x, g, hop, opacity);
    drawPose(ctx, pose, x, g.base - hop, { size: g.size, facing, opacity, lean, spin });
  }

  /** Режим «меньше движения»: только спокойные неподвижные кадры, без погони. */
  private drawStill(ctx: CanvasRenderingContext2D, g: G) {
    switch (this.mode) {
      case 'wave':
      case 'deliver':
        return wave(ctx, g, 0);
      case 'run': {
        const x = lerp(g.start, g.end, 0.5);
        line(ctx, pathBetween(g.left, x, () => g.base));
        return this.drawHero(ctx, g, 'idle', x, 0);
      }
      case 'fail':
      case 'broken':
        breakThread(ctx, g, 3);
        return this.drawHero(ctx, g, 'sad', g.start, 0);
      default:
        return this.drawIdle(ctx, g);
    }
  }

  private drawIdle(ctx: CanvasRenderingContext2D, g: G) {
    line(ctx, [[g.left, g.base], [g.right, g.base]], 'rgba(182,205,218,.45)', 0.48, false);
    this.drawHero(ctx, g, 'idle', g.start, 0);
  }

  /** Бег, пока мотор готовит, запускает и проверяет. К серверу без подтверждения не приходит. */
  private drawRun(ctx: CanvasRenderingContext2D, g: G) {
    const t = Math.max(0, this.t - RUN_DELAY);
    const p = runProgress(this.t);
    const speed = this.t < RUN_DELAY ? 0 : (0.82 / 1.6) * Math.exp(-t / 1.6);
    const run = clamp(speed * 1.35, 0, 0.6) * smooth(t / 0.25);
    this.stride(run);
    this.progress = p;
    this.drawTraveller(ctx, g, p, run, true);
  }

  /** Подтверждение пришло: добегает до сервера, отдаёт пакет и растворяется в волне. */
  private drawDeliver(ctx: CanvasRenderingContext2D, g: G) {
    const d = this.deliverDuration();
    if (this.t < d) {
      const u = smoother(this.t / d);
      const p = this.from + (1 - this.from) * u;
      this.progress = p;
      this.stride(0.6);
      // Мимо стража у ворот проскакивает низким рывком.
      const dash = !Number.isNaN(this.gateFall) && u > 0.1 && u < 0.8;
      this.drawTraveller(ctx, g, p, 0.6 * Math.pow(Math.sin(Math.PI * u), 2) + 0.15, false, dash);
      return;
    }
    const a = this.t - d;
    wave(ctx, g, this.wave, 1, MOTION.amplitude * smoother(a / 1.1));
    this.drawGuards(ctx, g, null);
    this.drawGateFall(ctx, g);
    const opacity = 1 - smoother((a - 0.45) / 0.65);
    if (opacity > 0) this.drawHero(ctx, g, 'deliver', g.end, 0, 1, opacity, 0.03 * bump(a, 0.15, 0.15));
    // Пакет перелетает в сервер.
    if (a >= 0.12 && a < 0.75) {
      const p = smoother((a - 0.12) / 0.63);
      const h = poseHeight('idle', g.size);
      const px = lerp(g.end + 0.3 * h, g.right + 8, p);
      const py = lerp(g.base - 0.55 * h, g.base, p) - Math.sin(Math.PI * p) * 10;
      parcel(ctx, px, py, g.size, 1 - p * 0.7);
    }
    if (a >= 0.45) dissolve(ctx, g.end, g.base - poseHeight('idle', g.size) * 0.45, (a - 0.45) / 0.8, g);
  }

  /** Ошибка: нить рвётся, страж хватает её за шиворот и неуклюже утаскивает обратно к началу.
   *  Там растворяется в тумане, а она остаётся у браузера — грустная, с пакетом. */
  private drawFail(ctx: CanvasRenderingContext2D, g: G) {
    const a = this.t;
    this.drawGuards(ctx, g, null); // остальные стражи досиживают или уходят в туман
    const wasUp = this.from >= 1;
    const x0 = wasUp ? g.end : lerp(g.start, g.end, this.from);
    const head = g.base - 34 * g.size;

    // Нить: натягивается и рвётся (или волна обрывается, если связь была и пропала).
    if (wasUp) {
      if (a < BREAK_AT) wave(ctx, g, this.wave, 1, MOTION.amplitude * (1 - smooth(a / BREAK_AT)));
      else breakThread(ctx, g, a - BREAK_AT);
    } else if (a < BREAK_AT) {
      const strain = smoother(a / BREAK_AT);
      const reach = lerp(x0 + 2, g.right, strain);
      line(ctx, pathBetween(g.left, reach, (u) => g.base + Math.sin(u * Math.PI * 2) * 1.5 * (1 - strain)), 'rgba(238,240,245,.97)');
    } else {
      breakThread(ctx, g, a - BREAK_AT);
    }

    const grab = wasUp ? CATCH_APPEAR : CATCH_GRAB;
    if (a < grab) {
      if (wasUp) {
        // Её уже не было видно (растворилась в волне) — страж выходит из тумана, держа её.
        mist(ctx, x0, head, bump(a, grab, 0.35), g.size);
        return;
      }
      // Страж у ворот (или новый, из тумана) бросается — она отшатывается.
      const gate = this.gateShown > 0;
      const home = gate ? lerp(g.start, g.end, GATE_AT) : Math.min(g.end + 10, x0 + 70 * g.size);
      const appear = gate ? this.gateShown : smooth(a / 0.3);
      if (!gate) mist(ctx, home, head, 1 - appear, g.size);
      const k = smooth((a - 0.15) / (grab - 0.15));
      this.drawGuardFigure(ctx, g, a < 0.15 ? 'guard-idle' : 'guard-lunge', lerp(home, x0 + 34 * g.size, k), appear, -0.08 * k);
      this.drawHero(ctx, g, a < 0.12 ? 'idle' : 'lean', x0, 0, 1, 1, -0.05 * smooth(a / 0.3));
      return;
    }
    if (a < CATCH_HOLD) {
      // Поймал: держит за шиворот, она болтает ногами.
      const k = a - grab;
      const opacity = wasUp ? smooth(k / 0.25) : 1;
      if (wasUp) mist(ctx, x0, head, 1 - opacity, g.size);
      const kick = 0.035 * Math.sin(a * 26) * Math.exp(-k * 1.2);
      this.drawGuardFigure(ctx, g, 'guard-caught', x0, opacity, kick, 0.08 * Math.exp(-k * 10));
      return;
    }
    const carry = this.carryDuration();
    const c = (a - CATCH_HOLD) / carry;
    if (c < 1) {
      // Неуклюже тащит её обратно к началу, переваливаясь с ноги на ногу.
      const x = lerp(x0, g.start, smoother(c));
      const step = Math.sin(a * 9);
      this.drawGuardFigure(ctx, g, 'guard-carry', x, 1, 0.05 * step, 0, 2.5 * Math.abs(step));
      return;
    }
    // У браузера растворяется в тумане — она остаётся.
    const d = (a - CATCH_HOLD - carry) / CATCH_DROP;
    mist(ctx, g.start, head, bump(d, 0.35, 0.45), g.size);
    this.drawGuardFigure(ctx, g, 'guard-carry', g.start, 1 - smooth(d / 0.6));
    this.drawHero(ctx, g, 'sad', g.start, 0, 1, smooth((d - 0.25) / 0.6));
  }

  /** Бегущая с пакетом: нить тянется за ней, стражи пытаются её поймать.
   *  `gate` — пока идёт проверка, у ворот топчется страж; `dash` — проскакивает рывком. */
  private drawTraveller(ctx: CanvasRenderingContext2D, g: G, p: number, run: number, gate: boolean, dash = false) {
    const standing = poseHeight('idle', g.size);
    const x = lerp(g.start, g.end, p);
    const end = Math.max(g.left + 1, x - 6);
    line(ctx, pathBetween(g.left, end, (u) => g.base + Math.sin(u * Math.PI * 2 - this.t) * 1.5 * Math.sin(u * Math.PI)));
    for (let i = 0; i < 2; i++) {
      const tx = end - i * 8 - 6;
      if (tx > g.left) glow(ctx, tx, g.base, 7, 'rgba(120,239,238,.24)', 0.6 - i * 0.18);
    }

    const dodge = this.drawGuards(ctx, g, x);
    const gateDodge = gate ? this.drawGatekeeper(ctx, g, p) : 0;
    if (!gate) this.drawGateFall(ctx, g);

    let pose: Pose = dash ? 'dash' : run < 0.06 ? 'idle' : runPose(this.gait);
    let hop = 0;
    let spin = 0;
    let lean = dash ? 0 : 0.05 * run;
    if (dodge?.dodge === 'dash') {
      pose = 'dash';
      lean = 0;
    } else if (dodge?.dodge === 'flip') {
      const k = dodge.k;
      // Над нитью мало места (около 110 px): прыгает ровно настолько, чтобы при вращении
      // не задеть верх сцены. Полудиагональ кадра сальто — около 0,6 его высоты.
      const flip = poseHeight('flip', g.size);
      const room = Math.max(0, g.base - flip / 2 - 0.6 * flip - 2);
      hop = Math.min(0.62 * standing, room) * Math.sin(Math.PI * k);
      lean = 0;
      if (k < 0.15 || k > 0.85) {
        pose = 'jump'; // отталкивается и приземляется
      } else {
        // Кадр сальто нарисован вниз головой: поворот от −180° (прямо) через 0° (вверх ногами,
        // как на рисунке) до +180° (снова прямо) — полный оборот вперёд.
        pose = 'flip';
        spin = Math.PI * 2 * smoother((k - 0.15) / 0.7) - Math.PI;
      }
    } else if (gateDodge > 0.2) {
      // Страж у ворот тянет руки — она отклоняется назад.
      pose = 'lean';
      lean = -0.06 * gateDodge;
    }
    this.drawHero(ctx, g, pose, x, hop, 1, 1, lean, spin);
  }

  /** Все стражи на пути. `heroX` — где она сейчас (null — её на пути нет).
   *  Возвращает, уворачивается ли она сейчас и как далеко зашёл уворот (0…1). */
  private drawGuards(ctx: CanvasRenderingContext2D, g: G, heroX: number | null): { dodge: Dodge; k: number } | null {
    let result: { dodge: Dodge; k: number } | null = null;
    this.guards = this.guards.filter((guard) => {
      const home = lerp(g.start, g.end, guard.at);
      const near = heroX !== null && heroX >= home - 34 * g.size;
      if (Number.isNaN(guard.sprung) && Number.isNaN(guard.left) && (near || this.clock - guard.born > PATIENCE)) {
        guard.sprung = this.clock;
      }
      if (!this.drawGuard(ctx, g, guard, home)) return false; // растворился — больше не нужен
      if (heroX !== null && Math.abs(heroX - home) < 90 * g.size) {
        const k = (this.clock - guard.sprung - DODGE_DELAY) / DODGE_TIME;
        if (k >= 0 && k <= 1) result = { dodge: guard.dodge, k };
      }
      return true;
    });
    return result;
  }

  /** Один страж. Возвращает false, когда он полностью растворился. */
  private drawGuard(ctx: CanvasRenderingContext2D, g: G, guard: Guard, home: number): boolean {
    const appear = smooth((this.clock - guard.born) / APPEAR);
    const head = g.base - 34 * g.size;
    const sway = Math.sin(this.clock * 3.1 + guard.at * 10) * 0.035;
    if (!Number.isNaN(guard.left)) {
      // Не успел броситься — уходит обратно в туман.
      const away = (this.clock - guard.left) / 0.6;
      if (away >= 1) return false;
      mist(ctx, home, head, bump(away, 0.5, 0.5), g.size);
      this.drawGuardFigure(ctx, g, 'guard-idle', home, appear * (1 - smooth(away)), sway);
      return true;
    }
    const tau = this.clock - guard.sprung;
    if (Number.isNaN(tau) || tau < 0) {
      // Поджидает: выходит из тумана и тяжело переминается.
      mist(ctx, home, head, 1 - appear, g.size);
      this.drawGuardFigure(ctx, g, 'guard-idle', home, appear, sway);
      return true;
    }
    const reach = 16 * g.size;
    const slide = 64 * g.size;
    if (tau < LUNGE) {
      // Тяжёлый бросок навстречу, руки вперёд.
      const k = tau / LUNGE;
      this.drawGuardFigure(ctx, g, 'guard-lunge', home - reach * smoother(k), appear, -0.08 * smooth(k));
    } else if (tau < LUNGE + STUMBLE) {
      // Промахнулся: по инерции проносится мимо и спотыкается, размахивая руками.
      const k = (tau - LUNGE) / STUMBLE;
      this.drawGuardFigure(ctx, g, 'guard-stumble', home - reach - slide * smoother(k), appear, -0.22 * Math.sin(Math.PI * k) - 0.08 * k);
    } else {
      // Шлёпнулся: «шмяк» с пылью, посидел и растворился.
      const k = tau - LUNGE - STUMBLE;
      if (k > FALLEN_HOLD + VANISH) return false;
      const x = home - reach - slide;
      dust(ctx, x, g.base, k, g.size);
      mist(ctx, x, g.base - 22 * g.size, bump(k, FALLEN_HOLD + VANISH * 0.6, VANISH * 0.7), g.size);
      const opacity = appear * (1 - smooth((k - FALLEN_HOLD) / VANISH));
      this.drawGuardFigure(ctx, g, 'guard-fallen', x, opacity, 0, 0.16 * Math.exp(-k * 8) * Math.cos(k * 22));
    }
    return true;
  }

  /** Страж у ворот. Возвращает, насколько сильно она сейчас отклоняется от его рук (0…1). */
  private drawGatekeeper(ctx: CanvasRenderingContext2D, g: G, p: number): number {
    const show = smooth((p - GATE_SHOW) / 0.06);
    this.gateShown = show;
    if (show <= 0) return 0;
    if (Number.isNaN(this.gateSince)) this.gateSince = this.clock;
    const home = lerp(g.start, g.end, GATE_AT);
    const c = (this.clock - this.gateSince) % GATE_CYCLE;
    let pose: GuardPose = 'guard-idle';
    let x = home;
    let lean = Math.sin(this.clock * 3.1) * 0.035;
    if (c >= 1.2 && c < 1.5) {
      const k = (c - 1.2) / 0.3;
      pose = 'guard-lunge';
      x = home - 20 * g.size * smoother(k);
      lean = -0.1 * smooth(k);
    } else if (c >= 1.5 && c < 2.2) {
      // Схватил воздух, покачнулся и неуклюже вернулся на место.
      const k = (c - 1.5) / 0.7;
      pose = 'guard-stumble';
      x = home - 20 * g.size * (1 - smoother(k));
      lean = 0.18 * Math.sin(Math.PI * k * 2) * (1 - k);
    }
    mist(ctx, home, g.base - 34 * g.size, 1 - show, g.size);
    this.drawGuardFigure(ctx, g, pose, x, show, lean);
    return bump(c, 1.42, 0.38);
  }

  /** «Подключено»: страж у ворот бросается в последний раз, промахивается и падает. */
  private drawGateFall(ctx: CanvasRenderingContext2D, g: G) {
    if (Number.isNaN(this.gateFall)) return;
    const t = this.clock - this.gateFall;
    const home = lerp(g.start, g.end, GATE_AT);
    const reach = 18 * g.size;
    const slide = 40 * g.size;
    if (t < 0.25) {
      this.drawGuardFigure(ctx, g, 'guard-lunge', home - reach * smoother(t / 0.25), this.gateShown, -0.1);
    } else if (t < 0.7) {
      const k = (t - 0.25) / 0.45;
      this.drawGuardFigure(ctx, g, 'guard-stumble', home - reach - slide * smoother(k), this.gateShown, -0.22 * Math.sin(Math.PI * k));
    } else {
      const k = t - 0.7;
      if (k > 0.5 + VANISH) {
        this.gateFall = NaN; // растворился
        return;
      }
      const x = home - reach - slide;
      dust(ctx, x, g.base, k, g.size);
      mist(ctx, x, g.base - 22 * g.size, this.gateShown * bump(k, 0.5 + VANISH * 0.6, VANISH * 0.7), g.size);
      const opacity = this.gateShown * (1 - smooth((k - 0.5) / VANISH));
      this.drawGuardFigure(ctx, g, 'guard-fallen', x, opacity, 0, 0.16 * Math.exp(-k * 8) * Math.cos(k * 22));
    }
  }

  /** Страж с тенью; смотрит влево — навстречу ниндзя. */
  private drawGuardFigure(ctx: CanvasRenderingContext2D, g: G, pose: GuardPose, x: number, opacity: number, lean = 0, squash = 0, lift = 0) {
    if (opacity <= 0) return;
    ctx.save();
    ctx.globalAlpha = 0.13 * opacity;
    ctx.fillStyle = '#4d4866';
    ctx.filter = 'blur(3px)';
    ctx.beginPath();
    ctx.ellipse(x, g.base + 2, 34 * g.size, 4, 0, 0, Math.PI * 2);
    ctx.fill();
    ctx.restore();
    drawPose(ctx, pose, x, g.base - lift, { size: g.size, facing: -1, opacity, lean, squash });
  }
}

// ——— Детали сцены (из референса) ———

type Pt = [number, number];

function pathBetween(a: number, b: number, yfn: (u: number) => number): Pt[] {
  const pts: Pt[] = [];
  for (let i = 0; i <= 72; i++) {
    const u = i / 72;
    pts.push([lerp(a, b, u), yfn(u)]);
  }
  return pts;
}

function line(ctx: CanvasRenderingContext2D, points: Pt[], color = 'rgba(247,255,255,.96)', alpha = 1, shine = true) {
  if (points.length < 2) return;
  ctx.save();
  ctx.globalAlpha = alpha;
  ctx.lineJoin = 'round';
  ctx.lineCap = 'round';
  const trace = () => {
    ctx.beginPath();
    points.forEach((p, i) => (i ? ctx.lineTo(p[0], p[1]) : ctx.moveTo(p[0], p[1])));
  };
  if (shine) {
    trace();
    ctx.lineWidth = 4;
    ctx.strokeStyle = 'rgba(77,218,227,.20)';
    ctx.shadowBlur = 10;
    ctx.shadowColor = '#66e8ed';
    ctx.stroke();
    ctx.shadowBlur = 0;
  }
  trace();
  ctx.lineWidth = 1.6;
  ctx.strokeStyle = color;
  ctx.stroke();
  ctx.restore();
}

function glow(ctx: CanvasRenderingContext2D, x: number, y: number, r: number, color: string, opacity = 1) {
  ctx.save();
  ctx.globalAlpha = clamp(opacity);
  const gradient = ctx.createRadialGradient(x, y, 0, x, y, r);
  gradient.addColorStop(0, color);
  gradient.addColorStop(1, 'rgba(94,230,231,0)');
  ctx.fillStyle = gradient;
  ctx.beginPath();
  ctx.arc(x, y, r, 0, Math.PI * 2);
  ctx.fill();
  ctx.restore();
}

/** Спокойная волна с тремя бегущими огоньками — состояние «подключено». */
function wave(ctx: CanvasRenderingContext2D, g: G, clock: number, alpha = 1, amp = MOTION.amplitude) {
  const y = (u: number) => g.base + amp * Math.sin(Math.PI * u) * Math.sin(u * Math.PI * 2 - clock * 0.75);
  line(ctx, pathBetween(g.left, g.right, y), 'rgba(249,255,255,.96)', alpha);
  [0, 0.333, 0.666].forEach((off, i) => {
    const u = (clock * 0.065 + 0.16 + off) % 1;
    const x = lerp(g.left, g.right, u);
    const py = y(u);
    const fade = Math.min(1, u * 10, (1 - u) * 10) * alpha;
    glow(ctx, x, py, i ? 13 : 20, 'rgba(60,232,233,.72)', fade);
    ctx.save();
    ctx.globalAlpha = fade;
    ctx.fillStyle = '#fbffff';
    ctx.beginPath();
    ctx.arc(x, py, i ? 2.5 : 3.9, 0, Math.PI * 2);
    ctx.fill();
    ctx.restore();
  });
}

/** Светящийся пакет данных, который ниндзя отдаёт серверу. */
function parcel(ctx: CanvasRenderingContext2D, x: number, y: number, size: number, opacity: number) {
  glow(ctx, x, y, 16 * size, 'rgba(99,229,224,.5)', opacity);
  ctx.save();
  ctx.globalAlpha = clamp(opacity);
  ctx.translate(x, y);
  ctx.scale(size, size);
  ctx.rotate(0.06);
  const g = ctx.createLinearGradient(0, -9, 0, 9);
  g.addColorStop(0, '#f2fdff');
  g.addColorStop(1, '#bfe9f6');
  ctx.fillStyle = g;
  ctx.strokeStyle = '#8fdbe6';
  ctx.lineWidth = 1;
  ctx.shadowColor = 'rgba(80,220,230,.8)';
  ctx.shadowBlur = 8;
  ctx.beginPath();
  ctx.roundRect(-9, -9, 18, 18, 5);
  ctx.fill();
  ctx.stroke();
  ctx.restore();
}

/** Сиреневый туман, из которого выходят и в котором растворяются стражи. */
function mist(ctx: CanvasRenderingContext2D, x: number, y: number, amount: number, size: number) {
  if (amount <= 0.01) return;
  ctx.save();
  for (let i = 0; i < 6; i++) {
    const ox = Math.sin(i * 2.4) * 22 * size;
    const oy = Math.cos(i * 1.7) * 16 * size;
    const r = (20 + (i % 3) * 7) * size;
    ctx.globalAlpha = clamp(amount) * 0.55;
    const gradient = ctx.createRadialGradient(x + ox, y + oy, 0, x + ox, y + oy, r);
    gradient.addColorStop(0, 'rgba(176,166,214,.55)');
    gradient.addColorStop(1, 'rgba(176,166,214,0)');
    ctx.fillStyle = gradient;
    ctx.beginPath();
    ctx.arc(x + ox, y + oy, r, 0, Math.PI * 2);
    ctx.fill();
  }
  ctx.restore();
}

/** Облачка пыли, когда страж шлёпается на землю. `t` — секунды после падения. */
function dust(ctx: CanvasRenderingContext2D, x: number, y: number, t: number, size: number) {
  if (t > 0.6) return;
  const p = smoother(t / 0.6);
  ctx.save();
  for (let i = 0; i < 5; i++) {
    const side = i % 2 ? 1 : -1;
    const px = x + side * (10 + i * 7) * size * p;
    const py = y - (3 + (i % 3) * 3) * size * Math.sin(Math.PI * p);
    ctx.globalAlpha = (1 - p) * 0.35;
    ctx.fillStyle = '#c9c3d8';
    ctx.beginPath();
    ctx.arc(px, py, (3 + (i % 3)) * size * (0.6 + p), 0, Math.PI * 2);
    ctx.fill();
  }
  ctx.restore();
}

function shadow(ctx: CanvasRenderingContext2D, x: number, g: G, hop: number, alpha = 1) {
  ctx.save();
  ctx.globalAlpha = 0.1 * alpha * (1 - clamp(hop / 40) * 0.6);
  ctx.fillStyle = '#598391';
  ctx.filter = 'blur(3px)';
  ctx.beginPath();
  ctx.ellipse(x, g.base + 2, 22 * g.size, 3, 0, 0, Math.PI * 2);
  ctx.fill();
  ctx.restore();
}

function dissolve(ctx: CanvasRenderingContext2D, x: number, y: number, t: number, g: G) {
  const p = smoother(t);
  for (let i = 0; i < 14; i++) {
    const seed = (i * 1.618) % 1;
    const x0 = x + (seed - 0.5) * 40 * g.size;
    const y0 = y + ((i % 7) - 3) * 9 * g.size;
    const px = lerp(x0, g.right + Math.sin(i * 2.1) * 5, p);
    const py = lerp(y0, g.base, p) + Math.sin(p * Math.PI) * (i % 2 ? 6 : -8);
    ctx.save();
    ctx.globalAlpha = (1 - p) * smooth(t / 0.2) * 0.6;
    ctx.fillStyle = i % 3 ? '#f8ffff' : '#72dfe3';
    ctx.beginPath();
    ctx.arc(px, py, i % 4 === 0 ? 2 : 1.2, 0, Math.PI * 2);
    ctx.fill();
    ctx.restore();
  }
}

/** Оборванная нить: два конца отпрянули, в месте разрыва короткая вспышка. */
function breakThread(ctx: CanvasRenderingContext2D, g: G, t: number) {
  const u = 0.66;
  const split = lerp(g.left, g.right, u);
  const gap = 19 * smoother(t / 0.6);
  const pull = Math.sin(t * 7) * Math.exp(-t * 3) * 5 * smooth(t / 0.12);
  const leftY = g.base + 12 * smoother(t / 0.9) + pull;
  const rightY = g.base - 8 * smoother(t / 0.9) - pull * 0.5;
  const alpha = lerp(1, 0.4, smoother(t / 1.6));
  line(ctx, pathBetween(g.left, split - gap, (v) => lerp(g.base, leftY, v * v)), 'rgba(164,180,193,.9)', alpha, false);
  line(ctx, pathBetween(split + gap, g.right, (v) => lerp(rightY, g.base, Math.sin((v * Math.PI) / 2))), 'rgba(164,180,193,.9)', alpha, false);
  if (t < 0.55) {
    const p = t / 0.55;
    const light = Math.sin(Math.PI * p) * 0.5;
    glow(ctx, split, g.base, 13 + 8 * p, 'rgba(204,174,186,.38)', light);
    ctx.save();
    ctx.globalAlpha = light;
    ctx.strokeStyle = '#fff9fa';
    ctx.lineWidth = 1;
    for (let i = 0; i < 3; i++) {
      const ang = (i * Math.PI * 2) / 3 - 0.4;
      ctx.beginPath();
      ctx.moveTo(split + Math.cos(ang) * (2 + 5 * p), g.base + Math.sin(ang) * (2 + 5 * p));
      ctx.lineTo(split + Math.cos(ang) * (4 + 9 * p), g.base + Math.sin(ang) * (4 + 9 * p));
      ctx.stroke();
    }
    ctx.restore();
  }
}
