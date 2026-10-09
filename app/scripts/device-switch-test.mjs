// Проверка в настоящем окне: смена страны в «Устройстве» (весь компьютер через TUN) — на лету.
// Подключение уже должно быть («Устройство»). Выбираем в панели TO, потом обратно прежний сервер.
// Смотрим: TUN-карта не пропадает, новый помощник (окно «Да») не запускается, страна выхода
// всего компьютера (curl без прокси — через TUN) меняется. Журнал — в файл: связь моргает.
// Запуск: TO=Poland node scripts/device-switch-test.mjs ../runtime/device-switch.log
import fs from 'node:fs';
import { execFileSync } from 'node:child_process';

const TO = process.env.TO ?? 'Poland';
const logFile = process.argv[2];
const log = (line) => fs.appendFileSync(logFile, `${new Date().toISOString().slice(11, 19)} ${line}\n`);
const results = [];
const check = (ok, text) => {
  results.push(ok);
  log(`${ok ? 'ДА ' : 'НЕТ'} ${text}`);
};
const ps = (command) => {
  try {
    return execFileSync('powershell', ['-NoProfile', '-Command', command], { encoding: 'utf8', timeout: 20000 }).trim();
  } catch {
    return '';
  }
};
const tunUp = () => ps("(Get-NetAdapter -Name 'ninja-vpn' -ErrorAction SilentlyContinue).Status") === 'Up';
// Помощник TUN — второй ninja-vpn.exe (с правами администратора); новый номер = новое окно «Да».
const helpers = () => ps("(Get-Process ninja-vpn -ErrorAction SilentlyContinue | Where-Object { -not $_.Path }).Id -join ','");
const country = () => {
  for (let i = 0; i < 3; i++) {
    try {
      return execFileSync('curl.exe', ['-s', '-m', '10', 'https://ipinfo.io/country'], { encoding: 'utf8' }).trim();
    } catch {}
  }
  return 'нет ответа';
};

const page = (await fetch('http://127.0.0.1:9333/json/list').then((r) => r.json())).find((t) => t.type === 'page');
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
const inv = (cmd, args = {}) => ev(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)}).catch((e) => ({ error: String(e) }))`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const switchTo = async (name) => {
  await ev(`document.querySelector('button[aria-label="Выбрать сервер"]')?.click(); 1`);
  await sleep(800);
  // Свёрнутый источник показывает только выбранный сервер — разворачиваем.
  await ev(`(() => { document.querySelectorAll('.source.collapsed .source-toggle').forEach((b) => b.click()); return 1; })()`);
  await sleep(500);
  const clicked = await ev(`(() => { const l = [...document.querySelectorAll('.server label')].find((l) => l.title === ${JSON.stringify(name)}); const r = l?.querySelector('input'); if (!r || r.disabled) return r ? 'кнопка неактивна' : 'нет строки'; r.click(); return 'ok'; })()`);
  const seen = [];
  let s = {};
  let tunDropped = false;
  for (let i = 0; i < 160; i++) {
    s = await inv('status');
    if (seen.at(-1) !== s.state) seen.push(s.state);
    if (!tunUp()) tunDropped = true;
    if ((s.state === 'connected' && s.server === name) || s.state === 'failed' || s.state === 'disconnected') break;
    await sleep(250);
  }
  return { clicked, seen, s, tunDropped };
};

log('старт');
let s = await inv('status');
const servers = (await inv('list_sources')).flatMap((x) => x.servers);
const from = servers.find((x) => x.key === s.key);
const to = servers.find((x) => x.name.includes(TO) && !x.problem);
check(s.state === 'connected' && !!from && !!to && tunUp(), `исходно: ${s.state} «${s.server}», TUN ${tunUp() ? 'есть' : 'нет'}, цель «${to?.name}»`);
const helpersBefore = helpers();
log(`страна выхода до: ${country()}, помощник: ${helpersBefore}`);

for (const target of [to, from]) {
  const r = await switchTo(target.name);
  log(`   выбор «${target.name}»: ${r.clicked}; состояния: ${r.seen.join(' → ')}`);
  check(r.s.state === 'connected' && r.s.server === target.name, `подключено к «${target.name}» без отключения (выход по проверке: ${r.s.exitCountry})`);
  check(!r.tunDropped && !r.seen.includes('disconnected') && !r.seen.includes('failed'), 'TUN-карта не пропадала, «Отключено»/«Ошибки» не было');
  check(helpers() === helpersBefore, `помощник тот же (${helpers()}) — нового окна «Да» не было`);
  const c = country();
  check(c === r.s.exitCountry, `весь компьютер выходит через ${c} (ожидалось ${r.s.exitCountry})`);
}
log(`итог: ${results.filter(Boolean).length} из ${results.length}`);
ws.close();
