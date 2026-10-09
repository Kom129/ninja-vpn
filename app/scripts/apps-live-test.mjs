// Живая проверка режима «Приложения» в НАСТОЯЩЕМ окне (как live-reserve-test.mjs).
//
// Запуск (окно должно быть уже открыто так):
//   $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS='--remote-debugging-port=9333'; npm run tauri dev
//   node scripts/apps-live-test.mjs [программа]
//
// [программа] — настоящая программа из меню «Пуск» на Chromium/Electron (по умолчанию VS Code).
// Вторая программа — тестовый «нарушитель»: копия node.exe, которая нарочно ходит в интернет
// напрямую, не слушая настройки VPN. Сторож обязан её заметить.
//
// Что проверяется:
//   1. После «Подключить» программы из списка открываются сами; настоящая программа ходит
//      через VPN (соединения к нашему прокси) и ни одного соединения напрямую.
//   2. Нарушитель замечен: окно пишет «напрямую».
//   3. «Отключить» → программа остаётся открытой, но напрямую в интернет не выходит (ждёт VPN).
//   4. Снова «Подключить» → программа снова ходит через VPN, без перезапуска.
//   5. «Закрыть программу» закрывает все её процессы.
// В конце тестовые программы убираются из списка, настройки окна возвращаются.

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const CDP = `http://127.0.0.1:${process.env.CDP_PORT || 9333}`;
const ROOT = path.resolve(import.meta.dirname, '..', '..');
const LOG_FILE = path.join(ROOT, 'runtime', 'apps-live-test.log');
const TEST_DIR = path.join(ROOT, 'runtime', 'test-apps');
const REAL = process.argv[2] || 'Visual Studio Code';
const KEPT = ['ninja.mode'];

const started = Date.now();
const lines = [];
const log = (text) => {
  const line = `[${((Date.now() - started) / 1000).toFixed(1).padStart(6)} с] ${text}`;
  lines.push(line);
  console.log(line);
};
const results = [];
const check = (ok, what) => {
  results.push({ ok, what });
  log(`${ok ? 'ДА ' : 'НЕТ'} — ${what}`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function openPage() {
  const targets = await fetch(`${CDP}/json/list`).then((r) => r.json());
  const page = targets.find((t) => t.type === 'page' && /1420|tauri/.test(t.url));
  if (!page) throw new Error('не нашёл страницу окна ninja-vpn на отладочном порту');
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((ok, fail) => {
    ws.onopen = ok;
    ws.onerror = () => fail(new Error('не удалось подключиться к отладочному порту'));
  });
  let id = 0;
  const waiting = new Map();
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && waiting.has(msg.id)) {
      waiting.get(msg.id)(msg);
      waiting.delete(msg.id);
    }
  };
  const send = (method, params = {}) =>
    new Promise((resolve) => {
      id += 1;
      waiting.set(id, resolve);
      ws.send(JSON.stringify({ id, method, params }));
    });
  const ev = async (expression) => {
    const msg = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    const r = msg.result;
    if (msg.error) throw new Error(msg.error.message);
    if (!r || r.exceptionDetails) throw new Error('ошибка на странице: ' + (r?.exceptionDetails?.exception?.description ?? JSON.stringify(r)));
    return r.result.value;
  };
  return { ev, close: () => ws.close() };
}

/** Команда мотору прямо со страницы окна — так же, как её вызывает интерфейс. */
const invoke = (page, cmd, args = {}) => page.ev(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})`);

async function reload(page, settings) {
  await page.ev(`(() => {
    const s = ${JSON.stringify(settings)};
    for (const [k, v] of Object.entries(s)) v === null ? localStorage.removeItem(k) : localStorage.setItem(k, v);
    location.reload();
    return true;
  })()`).catch(() => {});
  await sleep(1500);
  for (let i = 0; i < 60; i++) {
    if (await page.ev(`!!document.querySelector('.primary') && !!window.__TAURI_INTERNALS__`).catch(() => false)) return;
    await sleep(500);
  }
  throw new Error('окно не загрузилось после перезагрузки');
}

const vpnState = (page) => invoke(page, 'status').then((s) => s.state);
async function waitState(page, want, seconds) {
  for (let i = 0; i < seconds * 2; i++) {
    const s = await vpnState(page);
    if (want.includes(s)) return s;
    await sleep(500);
  }
  return vpnState(page);
}
const clickPrimary = (page) => page.ev(`document.querySelector('.primary').click(), true`);
const chipText = (page, name) =>
  page.ev(`[...document.querySelectorAll('.app-chip')].find((c) => c.querySelector('.app-chip-name')?.textContent === ${JSON.stringify(name)})?.querySelector('.app-chip-state')?.textContent ?? null`);

/** Ждать, пока итог сторожа по программе не станет таким, как нужно. */
async function waitStatus(page, id, test, seconds) {
  let last = null;
  for (let i = 0; i < seconds * 2; i++) {
    last = (await invoke(page, 'apps_status')).find((s) => s.id === id) ?? null;
    if (last && test(last)) return last;
    await sleep(500);
  }
  return last;
}
const fmt = (s) => (s ? `процессов ${s.processes} (наших ${s.ours}), через VPN ${s.viaVpn}, напрямую ${s.directTotal}${s.direct.length ? ' ' + s.direct.join(', ') : ''}` : 'нет данных');

function prepareViolator() {
  fs.mkdirSync(TEST_DIR, { recursive: true });
  const exe = path.join(TEST_DIR, 'direct-app.exe');
  if (!fs.existsSync(exe)) fs.copyFileSync(process.execPath, exe);
  const script = path.join(TEST_DIR, 'direct.js');
  // fetch в Node не слушает переменные прокси (без NODE_USE_ENV_PROXY) — ходит напрямую.
  fs.writeFileSync(
    script,
    "// Тестовый «нарушитель» для apps-live-test.mjs: ходит в интернет напрямую, мимо VPN.\n" +
      "setInterval(() => fetch('https://www.cloudflare.com/cdn-cgi/trace').then((r) => r.text()).catch(() => {}), 1500);\n" +
      'setTimeout(() => process.exit(0), 180000);\n',
  );
  return { exe, args: `"${script}"` };
}

let page;
let saved = null;
const added = [];
try {
  page = await openPage();
  // «Ошибка подключения» — тоже покой: окно не подключено.
  if (!['disconnected', 'failed'].includes(await vpnState(page))) throw new Error('окно сейчас подключено — проверка не трогает окно, которым пользуются');
  saved = Object.fromEntries(await Promise.all(KEPT.map(async (k) => [k, await page.ev(`localStorage.getItem(${JSON.stringify(k)})`)])));
  log(`Настройки окна до проверки: ${JSON.stringify(saved)}`);

  // Программы для проверки.
  const before = await invoke(page, 'list_apps');
  const programs = await invoke(page, 'installed_programs');
  const real = programs.find((p) => p.name.toLowerCase() === REAL.toLowerCase());
  if (!real) throw new Error(`программа «${REAL}» не найдена в меню «Пуск»`);
  if (real.support !== 'chromium') throw new Error(`«${real.name}» не на Chromium — возьми другую программу`);
  const running = execFileSync('tasklist', ['/FI', `IMAGENAME eq ${path.basename(real.exe)}`, '/NH'], { encoding: 'utf8' });
  if (running.toLowerCase().includes(path.basename(real.exe).toLowerCase())) throw new Error(`«${real.name}» сейчас открыта — закрой её, проверка её перезапускает`);
  const violator = prepareViolator();

  const already = before.find((a) => a.exe?.toLowerCase() === real.exe.toLowerCase());
  const realApp = already ?? (await invoke(page, 'add_app', { name: real.name, target: real.target, args: real.args, icon: real.icon }));
  if (!already) added.push(realApp.id);
  const badApp = await invoke(page, 'add_app', { name: 'Тест: нарушитель', target: violator.exe, args: violator.args, icon: null });
  added.push(badApp.id);
  check(!!realApp.icon, `«${real.name}» добавлена, значок есть`);

  await reload(page, { 'ninja.mode': JSON.stringify('apps') });
  const names = await page.ev(`[...document.querySelectorAll('.app-chip-name')].map((n) => n.textContent)`);
  check(names.includes(real.name) && names.includes('Тест: нарушитель'), `обе программы видны в полосе приложений (${names.join(', ')})`);

  // 1–2. Подключение: программы открываются сами.
  log('Нажимаю «Подключить»…');
  await clickPrimary(page);
  // Если окно было в «Ошибке» от прошлой попытки — сначала дождаться, что подключение началось,
  // иначе старая «Ошибка» сойдёт за итог новой попытки.
  await waitState(page, ['preparing', 'connecting', 'verifying', 'connected'], 5);
  const s1 = await waitState(page, ['connected', 'failed'], 90);
  check(s1 === 'connected', `подключилось (${s1})`);
  if (s1 !== 'connected') throw new Error('без подключения дальше проверять нечего');

  const good = await waitStatus(page, realApp.id, (s) => s.ours > 0 && s.viaVpn > 0, 40);
  log(`${real.name}: ${fmt(good)}`);
  check(!!good && good.ours > 0, `«${real.name}» открылась сама после подключения, запущена нами`);
  check(!!good && good.viaVpn > 0, `«${real.name}» ходит через VPN (соединения к нашему прокси)`);
  // Напрямую не должно быть ни одного соединения — смотрим 10 секунд.
  let directSeen = 0;
  for (let i = 0; i < 20; i++) {
    const s = (await invoke(page, 'apps_status')).find((x) => x.id === realApp.id);
    directSeen = Math.max(directSeen, s?.directTotal ?? 0);
    await sleep(500);
  }
  check(directSeen === 0, `«${real.name}»: напрямую — ни одного соединения за 10 с (видели ${directSeen})`);
  const cmdline = execFileSync('powershell', ['-NoProfile', '-Command', `(Get-CimInstance Win32_Process -Filter "Name='${path.basename(real.exe)}'" | Select-Object -First 1).CommandLine`], { encoding: 'utf8' });
  check(/--proxy-server=http:\/\/127\.0\.0\.1:\d+/.test(cmdline), 'у процесса программы ключ --proxy-server на наш прокси');
  log(`Подпись в окне: «${await chipText(page, real.name)}»`);

  const bad = await waitStatus(page, badApp.id, (s) => s.directTotal > 0, 20);
  log(`Нарушитель: ${fmt(bad)}`);
  check(!!bad && bad.directTotal > 0, 'нарушитель замечен: соединения напрямую');
  const badText = await chipText(page, 'Тест: нарушитель');
  check(!!badText && badText.includes('напрямую'), `окно предупреждает о нарушителе: «${badText}»`);

  // 3. Отключение: программа остаётся, но напрямую не выходит.
  log('Нажимаю «Отключить»…');
  await clickPrimary(page);
  check((await waitState(page, ['disconnected'], 15)) === 'disconnected', 'отключилось');
  directSeen = 0;
  let still = null;
  for (let i = 0; i < 24; i++) {
    still = (await invoke(page, 'apps_status')).find((x) => x.id === realApp.id);
    directSeen = Math.max(directSeen, still?.directTotal ?? 0);
    await sleep(500);
  }
  log(`${real.name} без VPN: ${fmt(still)}`);
  check(!!still && still.processes > 0, `«${real.name}» осталась открытой`);
  check(directSeen === 0, `без VPN «${real.name}» напрямую не выходит (за 12 с соединений напрямую: ${directSeen})`);
  log(`Подпись в окне: «${await chipText(page, real.name)}»`);

  // 4. Снова подключение — без перезапуска программы.
  log('Снова «Подключить»…');
  await clickPrimary(page);
  check((await waitState(page, ['connected', 'failed'], 90)) === 'connected', 'подключилось снова');
  const again = await waitStatus(page, realApp.id, (s) => s.portOk && s.viaVpn > 0, 45);
  log(`${real.name} после повторного подключения: ${fmt(again)}`);
  check(!!again && again.portOk && again.viaVpn > 0, `«${real.name}» снова ходит через VPN без перезапуска`);

  // 5. Закрыть программы.
  await invoke(page, 'close_app', { id: realApp.id });
  await invoke(page, 'close_app', { id: badApp.id });
  const closed = await waitStatus(page, realApp.id, (s) => s.processes === 0, 10);
  check(!!closed && closed.processes === 0, `«Закрыть программу» закрыла все процессы «${real.name}»`);

  log('Отключаюсь и возвращаю настройки…');
  await clickPrimary(page);
  await waitState(page, ['disconnected'], 15);
  for (const id of added) await invoke(page, 'remove_app', { id });
  await reload(page, saved);
} catch (e) {
  log(`ОСТАНОВЛЕНО: ${e.message}`);
  results.push({ ok: false, what: e.message });
  if (page) {
    if ((await vpnState(page).catch(() => '')) === 'connected') await clickPrimary(page).catch(() => {});
    for (const id of added) await invoke(page, 'close_app', { id }).catch(() => {});
    for (const id of added) await invoke(page, 'remove_app', { id }).catch(() => {});
    if (saved) await reload(page, saved).catch(() => {});
  }
} finally {
  page?.close();
  const passed = results.filter((r) => r.ok).length;
  log(`Итог: ${passed} из ${results.length}`);
  fs.mkdirSync(path.dirname(LOG_FILE), { recursive: true });
  fs.writeFileSync(LOG_FILE, lines.join('\n') + '\n');
}
