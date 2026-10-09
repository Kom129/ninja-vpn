// Живая проверка вкладки «Устройство» (весь компьютер через VPN) в НАСТОЯЩЕМ окне.
//
// Работает сам, без связи с Claude: два VPN на весь компьютер мешают друг другу, поэтому
// другой VPN (Durev) на время проверки выключают. Порядок:
//   1. Окно запущено с отладочным портом (как в live-reserve-test.mjs); для подробного журнала
//      TUN — ещё и с NINJA_TUN_DEBUG=1.
//   2. node scripts/device-live-test.mjs — если Durev включён, сценарий ждёт, пока его выключат (до 10 минут).
//   3. Запоминает настоящий адрес, нажимает «Подключить» во вкладке «Устройство»,
//      ждёт «Да» в окне Windows, проверяет интернет по адресу (без DNS) и по имени у разных
//      программ — все должны выходить НЕ с настоящего адреса; нажимает «Отключить» и проверяет,
//      что всё вернулось. Из журнала TUN — куда шли соединения и ошибки.
//   4. Если Durev выключали — окошко «Готово — включи Durev». Итог — в runtime/device-live-test.log.

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const CDP = `http://127.0.0.1:${process.env.CDP_PORT || 9333}`;
const ROOT = path.resolve(import.meta.dirname, '..', '..');
const LOG = path.join(ROOT, 'runtime', 'device-live-test.log');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const started = Date.now();
const lines = [];
const log = (s) => {
  const line = `[${((Date.now() - started) / 1000).toFixed(1).padStart(6)} с] ${s}`;
  lines.push(line);
  console.log(line);
  fs.mkdirSync(path.dirname(LOG), { recursive: true });
  fs.writeFileSync(LOG, lines.join('\n') + '\n');
};
const results = [];
const check = (ok, what) => {
  results.push(ok);
  log(`${ok ? 'ДА ' : 'НЕТ'} — ${what}`);
};
const mask = (ip) => ip.replace(/^(\d+)\.\d+\.\d+\.(\d+)$/, '$1.*.*.$2');

const ps = (cmd) => {
  try {
    return execFileSync('powershell', ['-NoProfile', '-Command', cmd], { encoding: 'utf8', timeout: 30000 }).trim();
  } catch {
    return '';
  }
};
const adapterUp = (name) => ps(`@(Get-NetAdapter -Name '${name}' -ErrorAction SilentlyContinue | Where-Object Status -eq 'Up').Count`) === '1';
/** Адрес, с которого видна программа: curl и PowerShell — две разные программы, обе без прокси.
 *  По имени сайта (нужен DNS) и по адресу 1.1.1.1 (без DNS) — так видно, что именно сломалось. */
const traceCurl = (url = 'https://www.cloudflare.com/cdn-cgi/trace') => {
  try {
    const t = execFileSync('curl.exe', ['-s', '-m', '12', url], { encoding: 'utf8' });
    return { ip: t.match(/ip=(.*)/)?.[1] ?? '?', loc: t.match(/loc=(.*)/)?.[1] ?? '?' };
  } catch {
    return { ip: 'нет ответа', loc: '?' };
  }
};
const tracePs = () => {
  const t = ps("(Invoke-WebRequest -UseBasicParsing -TimeoutSec 12 'https://www.cloudflare.com/cdn-cgi/trace').Content");
  return { ip: t.match(/ip=(.*)/)?.[1] ?? 'нет ответа', loc: t.match(/loc=(.*)/)?.[1] ?? '?' };
};
/** Что было в подробном журнале TUN (окно запущено с NINJA_TUN_DEBUG=1): куда шли соединения, ошибки. */
const tunSummary = () => {
  const text = (() => {
    try {
      // Установленная программа пишет рабочие файлы в %LOCALAPPDATA%\com.ninjavpn.app\runtime —
      // для неё RUNTIME_DIR=… ; у окна разработки — папка runtime проекта.
      return fs.readFileSync(path.join(process.env.RUNTIME_DIR || path.join(ROOT, 'runtime'), 'tun', 'sing-box.log'), 'utf8');
    } catch {
      return '';
    }
  })();
  if (!text.trim()) return 'журнал TUN пуст (окно запущено без NINJA_TUN_DEBUG=1?)';
  const count = (re) => (text.match(re) || []).length;
  const errors = text
    .split(/\r?\n/)
    // \b — чтобы «NOERROR» (успешный ответ DNS) не считался ошибкой.
    .filter((l) => /\b(ERROR|WARN|FATAL)\b/.test(l))
    .slice(0, 8)
    .map((l) => '    ' + l.replace(/^.*?\b(ERROR|WARN|FATAL)\b/, '$1'));
  return [
    `соединений в VPN: ${count(/outbound\/socks\[vpn\]: outbound connection/g)}, напрямую: ${count(/outbound\/direct\[direct\]: outbound connection/g)}, DNS-запросов: ${count(/inbound DNS packet/g)}, ответов DNS: ${count(/dns: exchanged [A-Za-z0-9.-]+ NOERROR/g)}`,
    errors.length ? ['ошибки:', ...errors].join('\n') : 'ошибок нет',
  ].join('\n  ');
};
const popup = (text) => ps(`Add-Type -AssemblyName PresentationFramework; [void][System.Windows.MessageBox]::Show('${text.replace(/'/g, "''")}', 'ninja-vpn: проверка «Устройство»')`);

async function openPage() {
  const page = (await fetch(`${CDP}/json/list`).then((r) => r.json())).find((t) => t.type === 'page' && /1420|tauri/.test(t.url));
  if (!page) throw new Error('не нашёл окно ninja-vpn на отладочном порту');
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((ok) => (ws.onopen = ok));
  let id = 0;
  const waiting = new Map();
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    waiting.get(msg.id)?.(msg);
  };
  const ev = (expression) =>
    new Promise((resolve) => {
      waiting.set(++id, (msg) => resolve(msg.result?.result?.value));
      ws.send(JSON.stringify({ id, method: 'Runtime.evaluate', params: { expression, awaitPromise: true, returnByValue: true } }));
    });
  return { ev, close: () => ws.close() };
}

const page = await openPage();
const state = async () => (await page.ev(`window.__TAURI_INTERNALS__.invoke('status').then((s) => JSON.stringify(s))`)) ?? '{}';
const stateName = async () => JSON.parse(await state()).state;
const click = () => page.ev(`document.querySelector('.primary').click(), 1`);
/** Записать настройки окна (`null` — удалить) и перезагрузить его. */
async function reload(settings) {
  const code = Object.entries(settings)
    .map(([key, value]) => (value === null ? `localStorage.removeItem(${JSON.stringify(key)})` : `localStorage.setItem(${JSON.stringify(key)}, ${JSON.stringify(value)})`))
    .join('; ');
  await page.ev(`(() => { ${code}; location.reload(); return 1; })()`).catch(() => {});
  await sleep(2500);
}

// Настройки окна, которые проверка меняет и в конце возвращает как были.
let saved = null;
const durevWasOn = adapterUp('DurevVPN');
try {
  const initial = await stateName();
  if (initial === 'connected') throw new Error('окно сейчас подключено — проверка его не трогает');
  // Подключение уже идёт (например, окно Windows ещё ждёт «Да») — не нажимаем, а дожидаемся его.
  const pending = initial !== 'disconnected' && initial !== 'failed';
  saved = {
    'ninja.mode': await page.ev(`localStorage.getItem('ninja.mode')`),
    'ninja.server': await page.ev(`localStorage.getItem('ninja.server')`),
  };
  const settings = { 'ninja.mode': JSON.stringify('device') };
  // SERVER=Netherlands — проверить на этом сервере (часть имени), а не на выбранном в окне.
  if (process.env.SERVER) {
    const query = process.env.SERVER.toLowerCase();
    const found = await page.ev(
      `window.__TAURI_INTERNALS__.invoke('list_sources').then((list) => list.flatMap((s) => s.servers).find((x) => !x.problem && x.name.toLowerCase().includes(${JSON.stringify(query)})) ?? null)`,
    );
    if (!found) throw new Error(`сервер «${process.env.SERVER}» не найден`);
    log(`Сервер для проверки: ${found.name}`);
    settings['ninja.server'] = JSON.stringify(found.key);
  }
  await reload(settings);

  log('Жду, пока выключат Durev (до 10 минут)…');
  for (let i = 0; i < 300 && adapterUp('DurevVPN'); i++) await sleep(2000);
  if (adapterUp('DurevVPN')) throw new Error('Durev так и не выключили');
  await sleep(4000);
  const real = traceCurl();
  log(`Durev выключен. Настоящий адрес: ${mask(real.ip)} (${real.loc})`);
  if (real.ip === 'нет ответа') throw new Error('без Durev интернет не отвечает — проверять нечего');

  if (pending) {
    log('Подключение уже идёт (окно Windows ждёт «Да») — жду его, ничего не нажимая…');
  } else {
    // Окно проверяет «нет ли другого VPN» раз в 3 с — ждём, пока «Подключить» оживёт.
    for (let i = 0; i < 40 && (await page.ev(`document.querySelector('.primary').disabled`)); i++) await sleep(500);
    if (await page.ev(`document.querySelector('.primary').disabled`)) {
      throw new Error(`кнопка «Подключить» неактивна: ${await page.ev(`document.querySelector('.split-note')?.textContent ?? ''`)}`);
    }
    log('Нажимаю «Подключить» во вкладке «Устройство» — Windows спросит разрешение…');
    await click();
  }
  let s = 'preparing';
  // До 10 минут: человеку нужно заметить окно Windows и нажать «Да».
  for (let i = 0; i < 1200; i++) {
    s = await stateName();
    if (s === 'connected' || s === 'failed') break;
    await sleep(500);
  }
  check(s === 'connected', `подключилось (${s === 'failed' ? JSON.parse(await state()).message : s})`);
  if (s !== 'connected') throw new Error('без подключения проверять нечего');
  await sleep(3000);
  check(adapterUp('ninja-vpn'), 'сетевая карта ninja-vpn работает');
  const byIp = traceCurl('https://1.1.1.1/cdn-cgi/trace');
  log(`по адресу 1.1.1.1 (без DNS): ${mask(byIp.ip)} (${byIp.loc})`);
  check(byIp.ip !== 'нет ответа' && byIp.ip !== real.ip, 'по адресу (без DNS) — через VPN');
  const viaCurl = [traceCurl(), traceCurl(), traceCurl()];
  const viaPs = tracePs();
  log(`curl без прокси: ${viaCurl.map((t) => `${mask(t.ip)} (${t.loc})`).join(', ')}; PowerShell без прокси: ${mask(viaPs.ip)} (${viaPs.loc})`);
  check(viaCurl.every((t) => t.ip !== 'нет ответа') && viaPs.ip !== 'нет ответа', 'интернет работает у обычных программ');
  check(viaCurl.every((t) => t.ip !== real.ip) && viaPs.ip !== real.ip, 'ни одна программа не выходит с настоящего адреса — весь компьютер через VPN');

  // HOLD=90 — подержать подключение 90 с, проверяя каждые 15 с: ядро за это время открывает
  // новые соединения к серверу (и ищет его адрес) — так видно, что связь не отваливается.
  const hold = Number(process.env.HOLD || 0);
  if (hold > 0) {
    log(`Держу подключение ${hold} с (замер каждые 15 с; в журнал — раз в минуту и каждый сбой)…`);
    const from = Date.now();
    const rounds = [];
    const states = new Set();
    let misses = 0;
    let lastLog = 0;
    let early = null;
    while (Date.now() - from < hold * 1000) {
      await sleep(15000);
      const st = await stateName();
      states.add(st);
      if (st === 'disconnected' || st === 'failed') {
        early = `окно отключилось (${st})`;
        break;
      }
      const t = traceCurl();
      const good = t.ip !== 'нет ответа' && t.ip !== real.ip;
      rounds.push(good);
      misses = good ? 0 : misses + 1;
      const sec = Math.round((Date.now() - from) / 1000);
      if (!good || sec - lastLog >= 60) {
        log(`  через ${sec} с: ${mask(t.ip)} (${t.loc}), окно: ${st}`);
        lastLog = sec;
      }
      // Страховка: три замера подряд без интернета — отключаем раньше, чтобы человек не остался без сети.
      if (misses >= 3) {
        early = 'три замера подряд без интернета';
        break;
      }
    }
    if (early) log(`  Остановил раньше времени: ${early}`);
    const good = rounds.filter(Boolean).length;
    check(!early && good === rounds.length, `${hold} с подряд интернет через VPN (${good} из ${rounds.length} замеров; окно было: ${[...states].join(', ')})`);
  }

  // Уже отключено (человек сам нажал «Отключить») — не нажимаем: кнопка подключила бы снова.
  if (!['disconnected', 'failed'].includes(await stateName())) {
    log('Нажимаю «Отключить»…');
    await click();
  }
  for (let i = 0; i < 40 && (await stateName()) !== 'disconnected'; i++) await sleep(500);
  await sleep(3000);
  check(!adapterUp('ninja-vpn'), 'после «Отключить» сетевой карты ninja-vpn нет');
  const after = traceCurl();
  log(`После отключения: ${mask(after.ip)} (${after.loc})`);
  check(after.ip === real.ip, 'интернет снова напрямую, как до проверки');
  log(`Журнал TUN:\n  ${tunSummary()}`);
} catch (e) {
  log(`ОСТАНОВЛЕНО: ${e.message}`);
  results.push(false);
  if ((await stateName().catch(() => '')) === 'connected') await click().catch(() => {});
} finally {
  if (saved) await reload(saved).catch(() => {});
  page.close();
  const ok = results.filter(Boolean).length;
  log(`Итог: ${ok} из ${results.length}`);
  if (durevWasOn) popup(`Проверка закончена: ${ok} из ${results.length}.\n\nВключи Durev обратно и напиши Claude «готово».`);
}
