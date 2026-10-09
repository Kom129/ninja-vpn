// Живая проверка режима «все программы через VPN, кроме этих» в НАСТОЯЩЕМ окне.
//
// Окно запускать в безопасном режиме: TUN забирает только два адреса, остальная сеть не меняется.
//   $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS='--remote-debugging-port=9333'
//   $env:NINJA_TUN_TEST_ROUTES='1.1.1.1/32,1.0.0.1/32'; npm run tauri dev
//   node scripts/tun-live-test.mjs
// При подключении Windows спросит разрешение администратора — нужно нажать «Да».
//
// Проверяется: исключение (curl-direct) уходит напрямую, остальные (curl-vpn) — через VPN;
// после «Отключить» сетевой карты ninja-vpn больше нет. Настройки окна возвращаются.

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const CDP = `http://127.0.0.1:${process.env.CDP_PORT || 9333}`;
const ROOT = path.resolve(import.meta.dirname, '..', '..');
const APPS = path.join(ROOT, 'runtime', 'test-apps');
const TUN_LOG = path.join(ROOT, 'runtime', 'tun', 'sing-box.log');
const KEPT = ['ninja.mode', 'ninja.appsSplit'];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (ok, what) => {
  results.push(ok);
  console.log(`${ok ? 'ДА ' : 'НЕТ'} — ${what}`);
};

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
    new Promise((resolve, reject) => {
      waiting.set(++id, (msg) => (msg.result?.exceptionDetails ? reject(new Error(JSON.stringify(msg.result.exceptionDetails))) : resolve(msg.result?.result?.value)));
      ws.send(JSON.stringify({ id, method: 'Runtime.evaluate', params: { expression, awaitPromise: true, returnByValue: true } }));
    });
  return { ev, close: () => ws.close() };
}

const invoke = (page, cmd, args = {}) => page.ev(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})`);
const state = (page) => invoke(page, 'status').then((s) => s);
async function reload(page, settings) {
  await page.ev(`(() => { const s = ${JSON.stringify(settings)}; for (const [k, v] of Object.entries(s)) v === null ? localStorage.removeItem(k) : localStorage.setItem(k, v); location.reload(); return 1; })()`).catch(() => {});
  await sleep(1500);
  for (let i = 0; i < 60 && !(await page.ev(`!!document.querySelector('.primary')`).catch(() => false)); i++) await sleep(500);
}
const trace = (exe) => {
  try {
    return execFileSync(exe, ['-s', '-m', '10', 'https://1.1.1.1/cdn-cgi/trace'], { encoding: 'utf8' }).match(/ip=(.*)/)?.[1] ?? 'нет ответа';
  } catch {
    return 'нет ответа';
  }
};
/** Из журнала TUN: какая программа куда ушла. */
function routes() {
  const log = fs.existsSync(TUN_LOG) ? fs.readFileSync(TUN_LOG, 'utf8') : '';
  const proc = new Map();
  const out = new Map();
  for (const line of log.split('\n')) {
    const id = line.match(/\[(\d+) /)?.[1];
    if (!id) continue;
    const p = line.match(/found process path: (.+)$/)?.[1];
    if (p) proc.set(id, p.trim());
    if (line.includes('outbound/socks[vpn]')) out.set(id, 'vpn');
    if (line.includes('outbound/direct[direct]')) out.set(id, 'direct');
  }
  return [...out].map(([id, o]) => [proc.get(id) ?? '?', o]);
}
const adapterUp = () =>
  execFileSync('powershell', ['-NoProfile', '-Command', "@(Get-NetAdapter -Name 'ninja-vpn' -ErrorAction SilentlyContinue).Count"], { encoding: 'utf8' }).trim() !== '0';

const page = await openPage();
let saved = null;
let added = null;
try {
  if ((await state(page)).state !== 'disconnected') throw new Error('окно сейчас подключено — проверка его не трогает');
  saved = Object.fromEntries(await Promise.all(KEPT.map(async (k) => [k, await page.ev(`localStorage.getItem(${JSON.stringify(k)})`)])));
  fs.mkdirSync(APPS, { recursive: true });
  const curl = path.join(process.env.WINDIR ?? 'C:\\Windows', 'System32', 'curl.exe');
  for (const name of ['curl-vpn.exe', 'curl-direct.exe']) fs.copyFileSync(curl, path.join(APPS, name));
  added = await invoke(page, 'add_app', { name: 'Тест: исключение', target: path.join(APPS, 'curl-direct.exe'), args: '', icon: null, package: null });
  await reload(page, { 'ninja.mode': JSON.stringify('apps'), 'ninja.appsSplit': JSON.stringify('except') });

  console.log('Нажимаю «Подключить» — Windows спросит разрешение администратора…');
  await page.ev(`document.querySelector('.primary').click(), 1`);
  let s;
  for (let i = 0; i < 360; i++) {
    s = await state(page);
    if (s.state === 'connected' || s.state === 'failed') break;
    await sleep(500);
  }
  check(s.state === 'connected', `подключилось (${s.state}${s.message ? ': ' + s.message : ''})`);
  if (s.state !== 'connected') throw new Error('без подключения проверять нечего');
  check(adapterUp(), 'сетевая карта ninja-vpn появилась');
  const ipVpn = trace(path.join(APPS, 'curl-vpn.exe'));
  const ipDirect = trace(path.join(APPS, 'curl-direct.exe'));
  await sleep(500);
  const r = routes();
  const went = (exe, out) => r.some(([p, o]) => p.endsWith(exe) && o === out);
  check(went('curl-vpn.exe', 'vpn') && !went('curl-vpn.exe', 'direct'), `обычная программа ушла через VPN (ответ: ${ipVpn !== 'нет ответа' ? 'есть' : 'нет'})`);
  check(went('curl-direct.exe', 'direct') && !went('curl-direct.exe', 'vpn'), `исключение ушло напрямую (ответ: ${ipDirect !== 'нет ответа' ? 'есть' : 'нет'})`);
  const caption = await page.ev(`document.querySelector('.caption')?.textContent`);
  console.log(`Подпись в окне: «${caption}»`);

  await page.ev(`document.querySelector('.primary').click(), 1`);
  for (let i = 0; i < 30 && (await state(page)).state !== 'disconnected'; i++) await sleep(500);
  await sleep(1500);
  check(!adapterUp(), 'после «Отключить» сетевой карты ninja-vpn нет');
} catch (e) {
  console.log(`ОСТАНОВЛЕНО: ${e.message}`);
  results.push(false);
  if ((await state(page).catch(() => ({}))).state === 'connected') await page.ev(`document.querySelector('.primary').click(), 1`).catch(() => {});
} finally {
  if (added) await invoke(page, 'remove_app', { id: added.id }).catch(() => {});
  if (saved) await reload(page, saved).catch(() => {});
  page.close();
  console.log(`Итог: ${results.filter(Boolean).length} из ${results.length}`);
}
