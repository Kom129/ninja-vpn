// Живая проверка резерва и автовосстановления в НАСТОЯЩЕМ окне (не в демо).
//
// Как это работает: окно ninja-vpn — это страница внутри WebView2. Если запустить окно
// с переменной WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333,
// WebView2 открывает «отладочный порт» (только на 127.0.0.1), и этот скрипт управляет
// страницей так же, как человек: нажимает кнопки и читает надписи. Мотор, ядра и серверы — настоящие.
//
// Запуск (окно должно быть уже открыто так):
//   $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS='--remote-debugging-port=9333'; npm run tauri dev
//   node scripts/live-reserve-test.mjs
//
// Что проверяется:
//   1. Резерв: выбран сервер, который не ответил при проверке скорости → окно само переходит
//      на самый быстрый сервер источника и честно пишет «резерв вместо …».
//   2. Ядро упало (останавливаем наш xray/sing-box) → «Переподключение…» → снова «Подключено»
//      на том же порту; второе окно браузера не открывается.
//   3. Ядро зависло (замораживаем процесс) → сторож замечает, что данные не идут, и переподключается.
//   4. «Отключить» → ядер не осталось.
//   5. Резерв выключен → тот же сервер даёт честную ошибку.
// Настройки окна (выбранный сервер, резерв, режим) в конце возвращаются как были.

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const CDP = `http://127.0.0.1:${process.env.CDP_PORT || 9333}`;
const ROOT = path.resolve(import.meta.dirname, '..', '..');
const LOG_FILE = path.join(ROOT, 'runtime', 'live-test.log');
/** Настройки окна до проверки. Пишем в файл сразу: если проверка оборвётся, следующий запуск их вернёт. */
const SAVED_FILE = path.join(ROOT, 'runtime', 'live-test-saved.json');
const KEPT = ['ninja.server', 'ninja.reserve', 'ninja.autoRecover', 'ninja.mode', 'ninja.panel'];

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
/** Проверка не состоялась не по вине окна (например, «неработающий» сервер вдруг ответил). */
const skip = (what) => {
  results.push({ ok: true, skipped: true, what });
  log(`ПРОПУСК — ${what}`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------- Связь со страницей окна (Chrome DevTools Protocol) ----------

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
  /** Команды, которые окно отправило мотору: { t, cmd, args }. */
  const calls = [];
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && waiting.has(msg.id)) {
      waiting.get(msg.id)(msg);
      waiting.delete(msg.id);
    }
    // Объект команд Tauri защищён от подмены (и правильно), поэтому смотрим на сами запросы.
    const req = msg.method === 'Network.requestWillBeSent' ? msg.params.request : null;
    const ipc = req?.method === 'POST' && req.url.match(/^https?:\/\/ipc\.localhost\/([^?]+)/);
    if (ipc) {
      const cmd = decodeURIComponent(ipc[1]);
      if (cmd !== 'status' && !cmd.startsWith('plugin:')) {
        let args = null;
        try { args = JSON.parse(req.postData ?? 'null'); } catch { /* тело не JSON */ }
        calls.push({ t: Date.now(), cmd, args });
      }
    }
  };
  let closed = false;
  ws.onclose = () => {
    closed = true;
    for (const resolve of waiting.values()) resolve({ error: { message: 'связь с окном пропала: окно закрылось или перезапустилось' } });
    waiting.clear();
  };
  const send = (method, params = {}) =>
    new Promise((resolve) => {
      if (closed) return resolve({ error: { message: 'связь с окном пропала' } });
      id += 1;
      waiting.set(id, resolve);
      ws.send(JSON.stringify({ id, method, params }));
    });
  /** Выполнить выражение на странице и вернуть результат. */
  const ev = async (expression) => {
    const msg = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    const r = msg.result;
    if (msg.error) throw new Error(msg.error.message);
    if (!r || r.exceptionDetails) {
      throw new Error('ошибка на странице: ' + (r?.exceptionDetails?.exception?.description ?? JSON.stringify(r)));
    }
    return r.result.value;
  };
  await send('Network.enable');
  return { ev, calls, close: () => ws.close() };
}

/** Журнал на странице: состояние мотора + надписи окна, каждые 150 мс. */
const HOOKS = `(() => {
  if (window.__ninja) return 'уже';
  const inner = window.__TAURI_INTERNALS__;
  const original = inner.invoke.bind(inner);
  const state = { timeline: [] };
  window.__ninja = state;
  let last = '';
  setInterval(async () => {
    const st = await original('status');
    const text = (sel) => document.querySelector(sel)?.textContent ?? null;
    const dom = {
      status: text('.status'), caption: text('.caption'), server: text('.endpoint.right .name'),
      meta: text('.endpoint.right .meta'), note: text('.fallback-note'), button: text('.primary'),
    };
    const sig = JSON.stringify([st.state, dom.caption, dom.note, dom.button]);
    if (sig !== last) { last = sig; state.timeline.push({ t: Date.now(), st, dom }); }
  }, 150);
  return 'ок';
})()`;

/** Сколько записей журнала окна уже прочитано. Обнуляется при перезагрузке страницы. */
let cursor = 0;

async function reloadWith(page, settings) {
  cursor = 0;
  await page.ev(`(() => {
    const s = ${JSON.stringify(settings)};
    for (const [k, v] of Object.entries(s)) v === null ? localStorage.removeItem(k) : localStorage.setItem(k, v);
    location.reload();
    return true;
  })()`).catch(() => {}); // страница перезагружается — ответ может не прийти
  await sleep(1500);
  for (let i = 0; i < 60; i++) {
    const ready = await page.ev(`!!document.querySelector('.primary') && !!window.__TAURI_INTERNALS__`).catch(() => false);
    if (ready) break;
    await sleep(500);
  }
  await page.ev(HOOKS);
}

/** Ждём состояние мотора; по пути пишем в журнал каждую новую надпись. */
async function waitState(page, wanted, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const fresh = await page.ev(`window.__ninja.timeline.slice(${cursor})`);
    for (const e of fresh) {
      cursor += 1;
      const d = e.dom;
      log(`   окно: «${d.status}» · ${d.caption}${d.note ? ` · [${d.note}]` : ''}`);
      if (wanted.includes(e.st.state)) return { ...e.st, dom: e.dom };
    }
    await sleep(300);
  }
  return null;
}

// ---------- Процессы ----------

function ps(script) {
  // «exit 0»: если процессов нет вовсе, Get-Process завершается кодом 1 даже с SilentlyContinue —
  // для нас это просто пустой ответ, а не сбой (раньше не всплывало: ядра Durev работали всегда).
  return execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', `${script}\nexit 0`], { encoding: 'utf8' }).trim();
}

/** Только НАШИ ядра — из папки engines\bin проекта. Ядра другого VPN (Durev) не трогаем. */
const OUR_ENGINES = `Get-Process xray,sing-box -ErrorAction SilentlyContinue | Where-Object { $_.Path -like '*ninja-vpn*\\engines\\bin\\*' }`;

function ourEngines() {
  const out = ps(`${OUR_ENGINES} | ForEach-Object { "$($_.Id) $($_.ProcessName)" }`);
  return out ? out.split(/\r?\n/).map((l) => l.trim()) : [];
}

function killOurEngine() {
  return ps(`${OUR_ENGINES} | ForEach-Object { Stop-Process -Id $_.Id -Force; "$($_.Id) $($_.ProcessName)" }`);
}

function freezeOurEngine() {
  return ps(`Add-Type -Name Nt -Namespace Ninja -MemberDefinition '[DllImport("ntdll.dll")] public static extern int NtSuspendProcess(IntPtr h);'
    ${OUR_ENGINES} | ForEach-Object { [void][Ninja.Nt]::NtSuspendProcess($_.Handle); "$($_.Id) $($_.ProcessName)" }`);
}

/** Окна браузера, которые открыло наше приложение (свой профиль в runtime\\browser-…). */
function closeOurBrowser() {
  return ps(`Get-CimInstance Win32_Process -Filter "Name='chrome.exe' OR Name='msedge.exe'" |
    Where-Object { $_.CommandLine -like '*ninja-vpn*\\runtime\\browser-*' -or $_.CommandLine -like '*com.ninjavpn.app\\runtime\\browser-*' } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue; $_.ProcessId } | Measure-Object | ForEach-Object Count`);
}

/** Страна выхода через наш прокси — независимо от окна. До трёх попыток, два разных сайта:
 *  у серверов-балансировщиков отдельные соединения иногда уходят на неработающий выход. */
function exitVia(port) {
  const sites = [
    ['https://cloudflare.com/cdn-cgi/trace', (text) => text.match(/^loc=(.+)$/m)?.[1]],
    ['https://ipinfo.io/json', (text) => {
      try {
        return JSON.parse(text).country;
      } catch {
        return null;
      }
    }],
  ];
  for (let i = 0; i < 3; i++) {
    const [url, parse] = sites[i % sites.length];
    try {
      const out = execFileSync('curl.exe', ['-s', '-m', '8', '-x', `http://127.0.0.1:${port}`, url], { encoding: 'utf8' });
      const country = parse(out);
      if (country) return country;
    } catch {
      /* следующая попытка — на новом соединении */
    }
  }
  return null;
}

const nameOf = (key) => key.split('/').slice(1).join('/');

// ---------- Сценарий ----------

/** `--soak 30` — вместо обычного сценария долгая проверка на 30 минут. */
const SOAK_MINUTES = (() => {
  const i = process.argv.indexOf('--soak');
  return i > 0 ? Number(process.argv[i + 1] || 30) : 0;
})();

/** Память (МБ): наших ядер и самого окна — если растёт час от часу, где-то утечка. */
function memoryMb() {
  const sum = (filter) => Number(ps(`(${filter} | Measure-Object WorkingSet64 -Sum).Sum`) || 0) / 1048576;
  return { engine: sum(OUR_ENGINES), app: sum(`Get-Process ninja-vpn -ErrorAction SilentlyContinue`) };
}

/** Долгая проверка: подключиться к самому быстрому серверу и N минут следить, что всё работает. */
async function soak(page, minutes) {
  const sources = await page.ev(`window.__TAURI_INTERNALS__.invoke('list_sources')`);
  const probes = JSON.parse((await page.ev(`localStorage.getItem('ninja.probes')`)) ?? '{}');
  const servers = sources[0].servers.filter((s) => !s.problem);
  const fastest = servers.filter((s) => probes[s.key]?.ms != null).sort((a, b) => probes[a.key].ms - probes[b.key].ms);
  const server = fastest[0] ?? servers[0];
  await reloadWith(page, {
    'ninja.server': JSON.stringify(server.key),
    'ninja.reserve': JSON.stringify('group'),
    'ninja.autoRecover': 'true',
    'ninja.mode': JSON.stringify('browser'),
  });
  log(`Долгая проверка на ${minutes} мин. Сервер «${server.name}», резерв — та же группа.`);
  const startedAt = Date.now();
  await page.ev(`document.querySelector('.primary').click(), true`);
  const up = await waitState(page, ['connected', 'failed'], 150_000);
  check(up?.state === 'connected', 'подключено');
  if (up?.state !== 'connected') return;
  const port = up.port;
  const memory = [];
  let good = 0;
  let bad = 0;
  const reconnects = [];
  const until = Date.now() + minutes * 60_000;
  while (Date.now() < until) {
    // Минуту ждём, не случится ли переподключение или обрыв.
    const event = await waitState(page, ['reconnecting', 'failed', 'disconnected'], 60_000);
    if (event?.state === 'reconnecting') {
      reconnects.push(event.reason);
      const back = await waitState(page, ['connected', 'failed'], 150_000);
      if (back?.state !== 'connected') return check(false, `после переподключения связь не вернулась: «${back?.dom?.caption}»`);
    } else if (event) {
      return check(false, `соединение потеряно: «${event.dom?.caption}»`);
    }
    const country = exitVia(port);
    if (country) good++;
    else bad++;
    const mb = memoryMb();
    memory.push(mb);
    const minute = Math.round((Date.now() - startedAt) / 60_000);
    log(`   ${minute} мин: данные ${country ? `идут (${country})` : 'НЕ идут'} · память ядра ${mb.engine.toFixed(0)} МБ, окна ${mb.app.toFixed(0)} МБ`);
  }
  const share = good / Math.max(1, good + bad);
  check(share >= 0.95, `данные шли в ${good} из ${good + bad} ежеминутных проверок`);
  check(reconnects.length === 0, reconnects.length ? `переподключений: ${reconnects.length} (${reconnects.join('; ')})` : 'ни одного переподключения');
  // Утечка памяти: сравниваем начало и конец (первые и последние 3 замера).
  const avg = (list, key) => list.reduce((s, m) => s + m[key], 0) / Math.max(1, list.length);
  for (const [key, title] of [['engine', 'ядра'], ['app', 'окна']]) {
    const growth = avg(memory.slice(-3), key) - avg(memory.slice(0, 3), key);
    check(growth < 50, `память ${title} за ${minutes} мин выросла на ${growth.toFixed(0)} МБ (порог — 50 МБ)`);
  }
  log('Нажимаю «Отключить»…');
  await page.ev(`document.querySelector('.primary').click(), true`);
  const off = await waitState(page, ['disconnected'], 10_000);
  check(off?.state === 'disconnected', 'отключено');
  await sleep(1000);
  check(ourEngines().length === 0, 'ни одного нашего ядра не осталось');
}

/** `--demo` — показ анимации на настоящих серверах: сначала неработающий сервер без резерва
 *  (погоня, потом её ловят), потом самый быстрый (проскакивает мимо стражей). Метки времени —
 *  в runtime/demo-marks.json, чтобы по ним нарезать запись окна. */
async function demo(page) {
  const marks = [];
  const mark = (name) => marks.push([name, Date.now()]);
  const click = () => page.ev(`document.querySelector('.primary').click(), true`);
  const sources = await page.ev(`window.__TAURI_INTERNALS__.invoke('list_sources')`);
  const probes = JSON.parse((await page.ev(`localStorage.getItem('ninja.probes')`)) ?? '{}');
  const servers = sources[0].servers.filter((s) => !s.problem);
  const fastest = servers.filter((s) => probes[s.key]?.ms != null).sort((a, b) => probes[a.key].ms - probes[b.key].ms);
  const resets = (x) => (/сбросил/.test(probes[x.key]?.error ?? '') ? 0 : 1);
  const silent = servers.filter((s) => probes[s.key] && probes[s.key].ms == null).sort((a, b) => resets(a) - resets(b));
  if (!fastest.length || !silent.length) throw new Error('нет свежей проверки скорости — сначала нажми «Проверить скорость» в окне');

  // 1. Неработающий сервер без резерва: погоня, проверка, ошибка — её ловят.
  for (const bad of silent.slice(0, 3)) {
    await reloadWith(page, { 'ninja.server': JSON.stringify(bad.key), 'ninja.reserve': JSON.stringify('off'), 'ninja.mode': JSON.stringify('browser') });
    await sleep(1500);
    log(`Показ 1: «${bad.name}», резерв выключен — погоня, потом поймают…`);
    mark('fail-start');
    await click();
    const end = await waitState(page, ['connected', 'failed'], 60_000);
    if (end?.state === 'failed') {
      mark('failed');
      await sleep(9000); // поймали, тащит к началу, растворяется
      mark('fail-end');
      break;
    }
    log('   сервер на этот раз ответил — отключаюсь и беру другой');
    await click();
    await waitState(page, ['disconnected'], 10_000);
    closeOurBrowser();
    marks.length = 0;
  }

  // 2. Самый быстрый сервер: проскакивает мимо стражей и доставляет пакет.
  await reloadWith(page, { 'ninja.server': JSON.stringify(fastest[0].key), 'ninja.reserve': JSON.stringify('group') });
  await sleep(1500);
  log(`Показ 2: «${fastest[0].name}» — проскакивает мимо стражей…`);
  mark('ok-start');
  await click();
  const up = await waitState(page, ['connected', 'failed'], 60_000);
  mark(up?.state === 'connected' ? 'connected' : 'ok-failed');
  await sleep(1200);
  closeOurBrowser(); // окно Chrome, которое программа открывает после подключения
  await sleep(4000);
  mark('ok-end');
  await click();
  await waitState(page, ['disconnected'], 10_000);
  fs.writeFileSync(path.join(ROOT, 'runtime', 'demo-marks.json'), JSON.stringify(marks, null, 1));
  log('Показ окончен.');
}

async function main() {
  const page = await openPage();
  // Сначала убеждаемся, что окном сейчас никто не пользуется. Если подключено — ничего
  // не трогаем: ни настройки, ни окно браузера, которое программа открыла для работы.
  const status = await page.ev(`window.__TAURI_INTERNALS__.invoke('status')`);
  const busy =
    status.state !== 'disconnected' && status.state !== 'failed'
      ? `окно сейчас в состоянии «${status.state}» — им пользуются, ничего не трогаю`
      : ourEngines().length
        ? 'уже запущено наше ядро — закрой лишние окна ninja-vpn'
        : null;
  if (busy) {
    page.close();
    throw new Error(busy);
  }
  fs.mkdirSync(path.dirname(SAVED_FILE), { recursive: true });
  let saved;
  if (fs.existsSync(SAVED_FILE)) {
    saved = JSON.parse(fs.readFileSync(SAVED_FILE, 'utf8'));
    log('Прошлая проверка оборвалась — беру настройки окна, сохранённые до неё.');
  } else {
    saved = await page.ev(`Object.fromEntries(${JSON.stringify(KEPT)}.map((k) => [k, localStorage.getItem(k)]))`);
    fs.writeFileSync(SAVED_FILE, JSON.stringify(saved, null, 2));
    log('Настройки окна сохранены, в конце вернутся.');
  }
  try {
    await page.ev(HOOKS);
    if (SOAK_MINUTES) return await soak(page, SOAK_MINUTES);
    if (process.argv.includes('--demo')) return await demo(page);

    // 1. Свежая проверка скорости — настоящей кнопкой в панели «Профили».
    await reloadWith(page, { 'ninja.panel': 'true' });
    log('Проверяю скорость серверов кнопкой в панели…');
    await page.ev(`document.querySelector('button[aria-label="Проверить скорость всех серверов"]').click(), true`);
    await sleep(1000);
    for (let i = 0; i < 240; i++) {
      if (await page.ev(`!document.querySelector('button[aria-label="Проверить скорость всех серверов"]').disabled`)) break;
      await sleep(500);
    }
    const sources = await page.ev(`window.__TAURI_INTERNALS__.invoke('list_sources')`);
    const probes = JSON.parse((await page.ev(`localStorage.getItem('ninja.probes')`)) ?? '{}');
    const servers = sources[0].servers.filter((s) => !s.problem);
    const answered = servers.filter((s) => probes[s.key]?.ms != null).sort((a, b) => probes[a.key].ms - probes[b.key].ms);
    const silent = servers.filter((s) => probes[s.key] && probes[s.key].ms == null);
    log(`Источник «${sources[0].name}»: ответили ${answered.length} из ${servers.length}; быстрее всех — «${answered[0]?.name}» (${probes[answered[0]?.key]?.ms} мс)`);
    // Кандидаты в «неработающие»: сначала «сбросил соединение» (надёжнее, чем «нет ответа»).
    const resets = (x) => (/сбросил/.test(probes[x.key].error ?? '') ? 0 : 1);
    const candidates = [...silent].sort((a, b) => resets(a) - resets(b)).slice(0, 3);
    if (!candidates.length) throw new Error('все серверы ответили — нечем проверить резерв');

    // 2. Резерв «Весь источник». Серверы бывают нестабильны: если «неработающий» вдруг ответил сам,
    //    резерв не понадобился — отключаемся и берём следующего кандидата.
    let bad = null;
    let first = null;
    let clickedAt = 0;
    for (const candidate of candidates) {
      log(`Выбираю неработающий сервер: «${candidate.name}» (${probes[candidate.key].error})`);
      await reloadWith(page, {
        'ninja.server': JSON.stringify(candidate.key),
        'ninja.reserve': JSON.stringify('source'),
        'ninja.autoRecover': 'true',
        'ninja.mode': JSON.stringify('browser'),
      });
      log('Нажимаю «Подключить» (резерв: весь источник)…');
      clickedAt = Date.now();
      await page.ev(`document.querySelector('.primary').click(), true`);
      await sleep(500);
      const plan = page.calls.find((c) => c.cmd === 'connect_vpn' && c.t >= clickedAt)?.args;
      if (!plan) throw new Error('окно не отправило команду connect_vpn');
      log(`План подключения от окна: ${plan.keys.map(nameOf).map((n) => `«${n}»`).join(' → ')}`);
      check(plan.keys[0] === candidate.key && plan.keys.length > 1, 'окно передало мотору выбранный сервер и резервные');
      first = await waitState(page, ['connected', 'failed'], 150_000);
      if (first?.state === 'connected' && first.key === candidate.key) {
        log(`«${candidate.name}» на этот раз ответил сам — резерв не понадобился. Отключаюсь, беру другой.`);
        await page.ev(`document.querySelector('.primary').click(), true`);
        await waitState(page, ['disconnected'], 20_000);
        await sleep(1000);
        continue;
      }
      bad = candidate;
      break;
    }
    const opens = () => page.calls.filter((c) => c.cmd === 'open_browser' && c.t >= clickedAt).length;
    if (!bad) {
      skip('все «неработающие» серверы на этот раз ответили — резерв проверить не удалось');
      await reloadWith(page, { 'ninja.server': JSON.stringify(answered[0].key) });
      clickedAt = Date.now();
      await page.ev(`document.querySelector('.primary').click(), true`);
      first = await waitState(page, ['connected', 'failed'], 150_000);
    }
    check(first?.state === 'connected', bad ? 'подключение через резерв дошло до «Подключено»' : 'подключение дошло до «Подключено»');
    if (first?.state !== 'connected') return;
    if (bad) {
      const note = await page.ev(`document.querySelector('.fallback-note')?.textContent ?? null`);
      check(first.key !== bad.key && first.fallbackFrom === bad.name, `сработал резервный сервер «${first.server}», а не выбранный`);
      check(!!note && note.includes('резерв вместо'), `окно честно пишет: «${note}»`);
    }
    const port = first.port;
    const exit1 = exitVia(port);
    check(!!exit1, `через прокси 127.0.0.1:${port} идут данные, выход ${exit1}`);
    await sleep(3000);
    const opens1 = opens();
    check(opens1 === 1, `после подключения открыто окно браузера (вызовов: ${opens1})`);

    // 3. Ядро упало.
    const killed = killOurEngine();
    log(`Останавливаю наше ядро: ${killed}`);
    const r1 = await waitState(page, ['reconnecting', 'failed'], 15_000);
    check(r1?.state === 'reconnecting', `сторож заметил остановку ядра → «Переподключение…» (${r1?.reason ?? 'нет'})`);
    check(!!r1?.dom?.caption?.includes('Связь пропала'), `окно объясняет, что происходит: «${r1?.dom?.caption}»`);
    const back1 = await waitState(page, ['connected', 'failed'], 150_000);
    check(back1?.state === 'connected', `снова «Подключено» (сервер «${back1?.server}»)`);
    check(back1?.port === port, `тот же порт ${port} — открытое окно браузера продолжает работать`);
    check(!!exitVia(port), 'данные снова идут через прокси');
    await sleep(3000);
    const opens2 = opens();
    check(opens2 === 1, `второе окно браузера не открылось (вызовов всего: ${opens2})`);
    if (bad) {
      const note2 = await page.ev(`document.querySelector('.fallback-note')?.textContent ?? null`);
      check(!!note2 && note2.includes('резерв вместо'), `после переподключения пометка резерва на месте: «${note2}»`);
    }

    // 3б. Стабильность: в прошлом прогоне через ~90 с после восстановления окно переподключилось
    // само, хотя данные шли. Смотрим 130 с (шесть проверок сторожа): переподключений быть не должно.
    log('Жду 130 с: ложных переподключений быть не должно…');
    const spurious = await waitState(page, ['reconnecting', 'failed', 'disconnected'], 130_000);
    check(!spurious, spurious ? `ложное переподключение: ${spurious.reason ?? spurious.state} · «${spurious.dom?.caption}»` : '130 с без ложных переподключений');
    if (spurious) await waitState(page, ['connected'], 150_000);

    // 4. Ядро зависло: процесс жив, но данные не идут.
    const frozen = freezeOurEngine();
    log(`Замораживаю наше ядро (процесс жив, но молчит): ${frozen}. Сторож проверяет раз в 20 с (с повтором через 5 с), нужно 2 промаха…`);
    const r2 = await waitState(page, ['reconnecting', 'failed'], 150_000);
    check(r2?.state === 'reconnecting', `сторож заметил, что данные не идут → «Переподключение…» (${r2?.reason ?? 'нет'})`);
    const back2 = await waitState(page, ['connected', 'failed'], 150_000);
    check(back2?.state === 'connected' && back2?.port === port, `снова «Подключено» на том же порту ${back2?.port}`);
    const engines = ourEngines();
    check(engines.length === 1, `замороженное ядро остановлено, работает одно новое (${engines.join(', ')})`);

    // 5. Отключение в самый неудобный момент: ядро только что упало, сторож вот-вот
    //    начнёт переподключение — а человек жмёт «Отключить». Раньше «Отключить» могло
    //    проиграть эту гонку, и VPN включался обратно сам.
    const killed2 = killOurEngine();
    log(`Снова роняю ядро (${killed2}) и сразу жму «Отключить» — гонка со сторожем…`);
    await page.ev(`document.querySelector('.primary').click(), true`);
    const off = await waitState(page, ['disconnected'], 10_000);
    check(off?.state === 'disconnected', 'состояние «Не подключено» сразу после нажатия');
    const revived = await waitState(page, ['connected', 'reconnecting'], 30_000);
    check(!revived, revived ? `VPN включился обратно сам: ${revived.state}` : '30 с: VPN сам обратно не включился');
    check(ourEngines().length === 0, 'ни одного нашего ядра не осталось');

    // 5б. «Отмена» посреди подключения: окно освобождается сразу, ядро прерванной попытки
    //     останавливается само, и через полминуты подключение не «воскресает».
    await reloadWith(page, { 'ninja.server': JSON.stringify(answered[0].key), 'ninja.reserve': JSON.stringify('source') });
    log(`Нажимаю «Подключить» («${answered[0].name}») и через 1 с — «Отмена»…`);
    await page.ev(`document.querySelector('.primary').click(), true`);
    await sleep(1000);
    const cancelAt = Date.now();
    await page.ev(`document.querySelector('.primary').click(), true`);
    const cancelled = await waitState(page, ['disconnected', 'connected'], 10_000);
    check(cancelled?.state === 'disconnected', `«Отмена» сработала за ${((Date.now() - cancelAt) / 1000).toFixed(1)} с`);
    const late = await waitState(page, ['connected', 'reconnecting'], 30_000);
    check(!late, late ? `подключение «воскресло» после отмены: ${late.state}` : '30 с: после отмены подключение не «воскресло»');
    check(ourEngines().length === 0, 'ядро прерванной попытки остановлено');

    // 6. Резерв выключен — тот же неработающий сервер.
    if (!bad) return skip('без «неработающего» сервера проверить ошибку без резерва нельзя');
    await reloadWith(page, { 'ninja.server': JSON.stringify(bad.key), 'ninja.reserve': JSON.stringify('off') });
    log('Резерв выключен. Нажимаю «Подключить» на том же неработающем сервере…');
    await page.ev(`document.querySelector('.primary').click(), true`);
    const lone = await waitState(page, ['connected', 'failed'], 60_000);
    const caption = await page.ev(`document.querySelector('.caption')?.textContent`);
    if (lone?.state === 'connected') {
      // «Белые списки» нестабильны: сервер, который не ответил при проверке скорости, иногда отвечает потом.
      skip(`сервер на этот раз ответил («${caption}») — проверить ошибку без резерва не удалось`);
    } else {
      check(lone?.state === 'failed', `без резерва — честная ошибка: «${caption}»`);
    }
    if (lone?.state === 'connected') {
      await page.ev(`document.querySelector('.primary').click(), true`);
      await waitState(page, ['disconnected'], 20_000);
    }
  } finally {
    log('Возвращаю настройки окна.');
    try {
      await reloadWith(page, saved);
      fs.rmSync(SAVED_FILE, { force: true });
    } catch (e) {
      log(`Не смог вернуть настройки (${e.message}) — они в ${path.relative(ROOT, SAVED_FILE)}, следующий запуск вернёт.`);
    }
    const closed = closeOurBrowser();
    if (closed !== '0') log(`Закрыл окно браузера, открытое проверкой (процессов: ${closed}).`);
    page.close();
    const failed = results.filter((r) => !r.ok).length;
    const skipped = results.filter((r) => r.skipped).length;
    const done = results.length - skipped;
    log(`ИТОГ: ${done - failed} из ${done} проверок прошли${skipped ? `, ${skipped} не состоялись (сервер)` : ''}.`);
    fs.mkdirSync(path.dirname(LOG_FILE), { recursive: true });
    fs.writeFileSync(LOG_FILE, lines.join('\n') + '\n');
    process.exitCode = failed ? 1 : 0;
  }
}

main().catch((e) => {
  log('ОСТАНОВКА: ' + e.message);
  fs.mkdirSync(path.dirname(LOG_FILE), { recursive: true });
  fs.writeFileSync(LOG_FILE, lines.join('\n') + '\n');
  process.exitCode = 2;
});
