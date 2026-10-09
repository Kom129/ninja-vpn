// Проверка в настоящем окне: при включённом Durev вкладка «Устройство» предупреждает и не даёт подключиться.
import fs from 'node:fs';

const CDP = 'http://127.0.0.1:9333';
const page = (await fetch(`${CDP}/json/list`).then((r) => r.json())).find((t) => t.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((ok) => (ws.onopen = ok));
let id = 0;
const waiting = new Map();
ws.onmessage = (m) => {
  const msg = JSON.parse(m.data);
  waiting.get(msg.id)?.(msg);
};
const call = (method, params = {}) =>
  new Promise((resolve) => {
    waiting.set(++id, resolve);
    ws.send(JSON.stringify({ id, method, params }));
  });
const ev = async (expression) =>
  (await call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })).result?.result?.value;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const saved = await ev(`localStorage.getItem('ninja.mode')`);
await ev(`localStorage.setItem('ninja.mode', JSON.stringify('device')); location.reload(); 1`).catch(() => {});
await sleep(5000);
const out = {
  status: await ev(`window.__TAURI_INTERNALS__.invoke('status').then((s) => s.state)`),
  otherVpn: await ev(`window.__TAURI_INTERNALS__.invoke('other_vpn')`),
  caption: await ev(`[...document.querySelectorAll('*')].find((e) => e.children.length === 0 && /Сначала выключи/.test(e.textContent))?.textContent ?? null`),
  note: await ev(`document.querySelector('.split-note.warn')?.textContent ?? null`),
  buttonDisabled: await ev(`document.querySelector('.primary').disabled`),
  // Мотор тоже не пускает, даже если окно обойти: ошибка до запуска ядра.
  directCall: await ev(`window.__TAURI_INTERNALS__.invoke('connect_vpn', { keys: ['x/y'], autoRecover: false, except: [] }).then(() => 'ПОДКЛЮЧЕНИЕ НАЧАЛОСЬ', (e) => String(e))`),
  statusAfter: await ev(`window.__TAURI_INTERNALS__.invoke('status').then((s) => s.state)`),
};
const shot = await call('Page.captureScreenshot', { format: 'png' });
fs.writeFileSync(process.argv[2], Buffer.from(shot.result.data, 'base64'));
if (process.argv[3] !== 'keep') await ev(`(() => { ${saved === null ? "localStorage.removeItem('ninja.mode')" : `localStorage.setItem('ninja.mode', ${JSON.stringify(saved)})`}; location.reload(); return 1; })()`).catch(() => {});
console.log(JSON.stringify(out, null, 2));
ws.close();
