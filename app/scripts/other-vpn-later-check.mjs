// Проверка в настоящем окне: другой VPN включили, когда «Устройство» уже подключено.
// Ожидается: ninja-vpn сам отключается (в пределах пары секунд) и пишет, почему.
// Пишет журнал в файл на каждом шаге — пока два VPN спорят, связи у Claude может не быть,
// а итог должен остаться. Запуск: node scripts/other-vpn-later-check.mjs <журнал> [минут ожидания]
import fs from 'node:fs';

const logFile = process.argv[2];
const deadline = Date.now() + Number(process.argv[3] ?? 10) * 60_000;
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
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const probe = () =>
  ev(`Promise.all([window.__TAURI_INTERNALS__.invoke('status'), window.__TAURI_INTERNALS__.invoke('other_vpn')])
    .then(([s, other]) => ({ state: s.state, server: s.server ?? null, message: s.message ?? null, other }))`);

log('старт: жду «Подключено» в режиме «Устройство», потом включения другого VPN');
let connectedAt = null;
let otherSeenAt = null;
let last = '';
let verdict = 'НЕ ДОЖДАЛСЯ';
while (Date.now() < deadline) {
  const s = await probe();
  const line = `${s.state} ${s.server ?? ''} другой VPN: ${s.other ?? 'нет'}${s.message ? ` | ${s.message}` : ''}`;
  if (line !== last) log(line);
  last = line;
  if (s.state === 'connected' && !connectedAt) connectedAt = Date.now();
  if (connectedAt && s.other && !otherSeenAt) otherSeenAt = Date.now();
  if (connectedAt && (s.state === 'failed' || s.state === 'disconnected')) {
    const fair = s.state === 'failed' && /другой VPN/.test(s.message ?? '');
    const secs = otherSeenAt ? ((Date.now() - otherSeenAt) / 1000).toFixed(1) : '?';
    verdict = fair ? `ПРОЙДЕНО: отключился сам через ~${secs} с после появления другого VPN` : `НЕ ТО: ${s.state}, ${s.message ?? 'без сообщения'}`;
    break;
  }
  await sleep(500);
}
log(`итог: ${verdict}`);
ws.close();
