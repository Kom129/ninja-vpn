// Проверка в настоящем окне: смена сервера во время подключения — без «Отключить».
// Режим «Браузер» (без TUN и окна «Да»; обычный браузер не трогается — открытый напрямую
// мотор только помечает). Подключаемся к FROM, в панели «Профили» выбираем TO, смотрим:
// между ними нет «Отключено»/«Ошибка», окно пишет «Переключаюсь…», итог — TO на том же порту.
// Запуск: FROM=Finland TO=Poland node scripts/switch-live-test.mjs  (окно с портом 9333).
const FROM = process.env.FROM ?? 'Finland';
const TO = process.env.TO ?? 'Poland';

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
const caption = () => ev(`document.querySelector('p.caption')?.textContent ?? ''`);
const results = [];
const check = (ok, text) => {
  results.push(ok);
  console.log(`${ok ? 'ДА ' : 'НЕТ'} ${text}`);
};

const saved = await ev(`JSON.stringify({ mode: localStorage.getItem('ninja.mode'), server: localStorage.getItem('ninja.server') })`);
const servers = (await inv('list_sources')).flatMap((s) => s.servers);
const from = servers.find((s) => s.name.includes(FROM) && !s.problem);
const to = servers.find((s) => s.name.includes(TO) && !s.problem);
if (!from || !to) throw new Error(`нет серверов ${FROM} / ${TO}`);

await ev(`localStorage.setItem('ninja.mode', '"browser"'); localStorage.setItem('ninja.server', ${JSON.stringify(JSON.stringify(from.key))}); location.reload(); 1`).catch(() => {});
await sleep(4000);
await inv('connect_vpn', { keys: [from.key], autoRecover: true, except: null });
let s = {};
for (let i = 0; i < 120 && s.state !== 'connected' && s.state !== 'failed'; i++) {
  await sleep(500);
  s = await inv('status');
}
check(s.state === 'connected', `подключено к «${from.name}»: ${s.state}, выход ${s.exitCountry}, порт ${s.port}`);
const port = s.port;

// Как человек: панель «Профили» → другой сервер.
await ev(`document.querySelector('button[aria-label="Выбрать сервер"]')?.click(); 1`);
await sleep(800);
const clicked = await ev(`(() => { const l = [...document.querySelectorAll('.server label')].find((l) => l.title === ${JSON.stringify(to.name)}); const r = l?.querySelector('input'); if (!r || r.disabled) return r ? 'кнопка неактивна' : 'нет строки'; r.click(); return 'ok'; })()`);
check(clicked === 'ok', `выбрал «${to.name}» во время подключения: ${clicked}`);

const seen = [];
let sawSwitching = false;
for (let i = 0; i < 240; i++) {
  s = await inv('status');
  if (seen.at(-1) !== s.state) seen.push(s.state);
  if (/Переключаюсь/.test(await caption())) sawSwitching = true;
  if ((s.state === 'connected' && s.server === to.name) || s.state === 'failed' || s.state === 'disconnected') break;
  await sleep(250);
}
console.log('   состояния:', seen.join(' → '));
check(!seen.includes('disconnected') && !seen.includes('failed'), 'между серверами не было «Отключено» и «Ошибки»');
check(sawSwitching, 'окно писало «Переключаюсь на …»');
check(s.state === 'connected' && s.server === to.name, `итог: ${s.state} «${s.server}», выход ${s.exitCountry}`);
check(s.port === port, `порт тот же: ${port} → ${s.port} (браузеру и TUN перезапуск не нужен)`);

await inv('disconnect_vpn');
await sleep(1000);
const back = JSON.parse(saved);
await ev(`(() => { const put = (k, v) => (v === null ? localStorage.removeItem(k) : localStorage.setItem(k, v)); put('ninja.mode', ${JSON.stringify(back.mode)}); put('ninja.server', ${JSON.stringify(back.server)}); location.reload(); return 1; })()`).catch(() => {});
console.log(`Итог: ${results.filter(Boolean).length} из ${results.length}`);
ws.close();
