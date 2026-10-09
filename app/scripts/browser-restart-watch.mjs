// Наблюдение за живой проверкой режима «Браузер»: человек сам жмёт кнопки в окне,
// а здесь раз в 3 с пишется, как запущен его обычный браузер и куда идут соединения.
// «через ninja» — TCP к 127.0.0.1:<порт прокси>, «напрямую» — к внешним адресам.
// Запуск: node scripts/browser-restart-watch.mjs <журнал> [минут] [chrome|edge|…]
import fs from 'node:fs';
import { execFileSync } from 'node:child_process';

const logFile = process.argv[2];
const deadline = Date.now() + Number(process.argv[3] ?? 10) * 60_000;
const browserId = process.argv[4] ?? 'chrome';
const exeName = { chrome: 'chrome', edge: 'msedge', yandex: 'browser', brave: 'brave' }[browserId];
const log = (line) => fs.appendFileSync(logFile, `${new Date().toISOString().slice(11, 19)} ${line}\n`);

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

const connections = (port) => {
  const script = `$ids = (Get-Process ${exeName} -ErrorAction SilentlyContinue).Id; if (-not $ids) { '0 0'; exit }
$c = Get-NetTCPConnection -State Established -OwningProcess $ids -ErrorAction SilentlyContinue
$ours = @($c | Where-Object { $_.RemoteAddress -eq '127.0.0.1' -and $_.RemotePort -eq ${port || 0} }).Count
$direct = @($c | Where-Object { $_.RemoteAddress -notmatch '^(127\\.|::1|0\\.0\\.0\\.0|::)' }).Count
"$ours $direct"`;
  try {
    return execFileSync('powershell', ['-NoProfile', '-Command', script], { encoding: 'utf8', timeout: 20000 }).trim().split(/\s+/).map(Number);
  } catch {
    return [NaN, NaN];
  }
};

log(`старт: слежу за ${browserId}`);
let last = '';
while (Date.now() < deadline) {
  const s = await inv('status');
  const run = await inv('browser_status', { id: browserId });
  const mode = await ev(`localStorage.getItem('ninja.mode')`);
  const caption = await ev(`[...document.querySelectorAll('p.caption')].map((p) => p.textContent).join(' | ')`);
  const [ours, direct] = connections(s.port);
  const line = `${s.state}${s.server ? ` «${s.server}»` : ''} режим ${mode} | браузер: ${typeof run === 'string' ? run : JSON.stringify(run)} | соединений через ninja: ${ours}, напрямую: ${direct}`;
  if (line !== last) log(`${line}\n         окно: ${caption}`);
  last = line;
  await sleep(3000);
}
log('конец наблюдения');
ws.close();
