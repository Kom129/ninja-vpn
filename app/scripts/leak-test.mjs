// Проверка утечек в режиме «Только браузер»: узнают ли сайты настоящий адрес.
//
// Что делает (окно программы не трогает):
//   1. Подключается через командную строку мотора (`ninja connect <сервер>`) на свободном порту.
//   2. Запускает отдельный Chrome без окна с теми же ключами, что и программа (прокси + политика
//      WebRTC), и для сравнения — такой же Chrome без нашего VPN.
//   3. Сравнивает: адрес IPv4 и IPv6 у сайтов, кандидаты WebRTC (STUN), DNS-серверы (browserleaks),
//      и проверяет, что у Chrome нет ни одного прямого TCP-соединения в обход прокси.
//   4. Роняет ядро: сайты должны перестать открываться, а не уйти напрямую.
//   5. Если открыто окно Chrome, которое запустила сама программа, — только смотрит его соединения.
//
// Запуск: node scripts/leak-test.mjs [сервер]   (сервер — номер или часть имени, по умолчанию Poland)
// Итог — в runtime/leak-test.log.

import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const ROOT = path.resolve(import.meta.dirname, '..', '..');
const RUNTIME = path.join(ROOT, 'runtime');
const NINJA = path.join(ROOT, 'target', 'debug', 'ninja.exe');
const SERVER = process.argv[2] || 'Poland';
const CHROME = [
  path.join(process.env.ProgramFiles ?? '', 'Google', 'Chrome', 'Application', 'chrome.exe'),
  path.join(process.env['ProgramFiles(x86)'] ?? '', 'Google', 'Chrome', 'Application', 'chrome.exe'),
  path.join(process.env.LOCALAPPDATA ?? '', 'Google', 'Chrome', 'Application', 'chrome.exe'),
].find((p) => fs.existsSync(p));

const started = Date.now();
const lines = [];
const log = (text) => {
  const line = `[${((Date.now() - started) / 1000).toFixed(1).padStart(5)} с] ${text}`;
  lines.push(line);
  console.log(line);
};
const results = [];
const check = (ok, what) => {
  results.push(ok);
  log(`${ok ? 'ДА ' : 'НЕТ'} — ${what}`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const ps = (script) => execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], { encoding: 'utf8' }).trim();
const IP = /\b(?:\d{1,3}\.){3}\d{1,3}\b|\b(?:[0-9a-f]{1,4}:){2,7}[0-9a-f]{1,4}\b/gi;

// ---------- Подключение через командную строку ----------

function connectCli() {
  return new Promise((resolve, reject) => {
    const cli = spawn(NINJA, ['connect', SERVER], { cwd: ROOT, stdio: ['pipe', 'pipe', 'pipe'] });
    let out = '';
    const onData = (d) => {
      out += d.toString('utf8');
      const port = out.match(/Локальный прокси: 127\.0\.0\.1:(\d+)/)?.[1];
      const exit = out.match(/Сайты видят тебя здесь: (\S+) \(IP ([^)]+)\)/);
      if (port) resolve({ cli, port: Number(port), country: exit?.[1], ipMasked: exit?.[2] });
    };
    cli.stdout.on('data', onData);
    cli.stderr.on('data', (d) => (out += d.toString('utf8')));
    cli.on('exit', () => reject(new Error('мотор не подключился:\n' + out.split('\n').slice(-6).join('\n'))));
    setTimeout(() => reject(new Error('мотор не подключился за 60 с')), 60_000);
  });
}

// ---------- Отдельный Chrome без окна ----------

async function startChrome(name, debugPort, proxyPort) {
  const profile = path.join(RUNTIME, `leaktest-${name}`);
  fs.rmSync(profile, { recursive: true, force: true });
  const args = [
    '--headless=new',
    `--user-data-dir=${profile}`,
    `--remote-debugging-port=${debugPort}`,
    '--no-first-run',
    '--no-default-browser-check',
    'about:blank',
  ];
  // Ровно те же ключи, что ставит программа (motor/src/browser.rs).
  if (proxyPort) args.unshift(`--proxy-server=http://127.0.0.1:${proxyPort}`, '--force-webrtc-ip-handling-policy=disable_non_proxied_udp');
  const proc = spawn(CHROME, args, { stdio: 'ignore' });
  for (let i = 0; i < 60; i++) {
    try {
      const targets = await fetch(`http://127.0.0.1:${debugPort}/json/list`).then((r) => r.json());
      const page = targets.find((t) => t.type === 'page');
      if (page) return { proc, profile, page: await cdp(page.webSocketDebuggerUrl) };
    } catch {
      /* ещё запускается */
    }
    await sleep(250);
  }
  throw new Error(`Chrome «${name}» не запустился`);
}

async function cdp(url) {
  const ws = new WebSocket(url);
  await new Promise((ok, fail) => {
    ws.onopen = ok;
    ws.onerror = () => fail(new Error('нет связи с Chrome'));
  });
  let id = 0;
  const waiting = new Map();
  const events = [];
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && waiting.has(msg.id)) {
      waiting.get(msg.id)(msg);
      waiting.delete(msg.id);
    } else if (msg.method) events.push(msg);
  };
  const send = (method, params = {}) =>
    new Promise((resolve) => {
      id += 1;
      waiting.set(id, resolve);
      ws.send(JSON.stringify({ id, method, params }));
    });
  await send('Page.enable');
  const ev = async (expression) => (await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })).result?.result?.value;
  /** Открыть адрес и вернуть текст страницы (или ошибку загрузки). */
  const open = async (target, waitMs = 0) => {
    events.length = 0;
    const nav = await send('Page.navigate', { url: target });
    if (nav.result?.errorText) return { error: nav.result.errorText };
    for (let i = 0; i < 80 && !events.some((e) => e.method === 'Page.loadEventFired'); i++) await sleep(250);
    if (waitMs) await sleep(waitMs);
    return { text: (await ev('document.body ? document.body.innerText : ""')) ?? '' };
  };
  return { ev, open, close: () => ws.close() };
}

/** Кандидаты WebRTC: какие адреса браузер готов показать собеседнику (через STUN-сервер Google). */
const WEBRTC = `new Promise((resolve) => {
  const found = [];
  const pc = new RTCPeerConnection({ iceServers: [{ urls: 'stun:stun.l.google.com:19302' }] });
  pc.createDataChannel('leak');
  pc.onicecandidate = (e) => { if (e.candidate && e.candidate.candidate) found.push(e.candidate.candidate); if (!e.candidate) done(); };
  const done = () => { try { pc.close(); } catch {} resolve(found); };
  pc.createOffer().then((o) => pc.setLocalDescription(o)).catch(done);
  setTimeout(done, 7000);
})`;

/** Прямые TCP-соединения процессов Chrome с этим профилем (всё, что не к 127.0.0.1). */
function directConnections(profileMarker) {
  const out = ps(`$ids = @(Get-CimInstance Win32_Process -Filter "Name='chrome.exe'" | Where-Object { $_.CommandLine -like '*${profileMarker}*' } | ForEach-Object { $_.ProcessId })
    if (-not $ids) { 'нет-процессов'; return }
    Get-NetTCPConnection -ErrorAction SilentlyContinue | Where-Object { $ids -contains $_.OwningProcess -and $_.RemoteAddress -notin @('127.0.0.1','::1','0.0.0.0','::') -and $_.State -ne 'Listen' } |
      ForEach-Object { "$($_.RemoteAddress):$($_.RemotePort) $($_.State)" }`);
  return out === 'нет-процессов' ? null : out.split(/\r?\n/).filter(Boolean);
}

/** Есть ли имя в кэше DNS самого Windows — то есть спрашивал ли его компьютер, а не VPN-сервер. */
function inLocalDns(marker) {
  return ps(`@(Get-DnsClientCache -ErrorAction SilentlyContinue | Where-Object { $_.Entry -like '*${marker}*' }).Count`) !== '0';
}

async function survey(name, chrome, marker) {
  // Уникальное выдуманное имя сайта: если компьютер спросит его сам, оно окажется в кэше DNS Windows.
  await chrome.open(`https://${marker}.nip.io/`);
  const localDns = inLocalDns(marker);
  const v4 = (await chrome.open('https://cloudflare.com/cdn-cgi/trace')).text ?? '';
  const ip4 = v4.match(/^ip=(.+)$/m)?.[1] ?? null;
  const country = v4.match(/^loc=(.+)$/m)?.[1] ?? null;
  const v6page = await chrome.open('https://api64.ipify.org');
  const ip64 = v6page.text?.trim() || null;
  await chrome.open('https://example.com');
  const candidates = (await chrome.ev(WEBRTC)) ?? [];
  const rtcIps = [...new Set(candidates.flatMap((c) => c.match(IP) ?? []))];
  const dnsPage = await chrome.open('https://browserleaks.com/dns', 9000);
  const dnsIps = [...new Set((dnsPage.text ?? '').match(IP) ?? [])];
  log(`   ${name}: IPv4 ${ip4 ?? '—'} (${country ?? '?'}), адрес по api64 ${ip64 ?? '—'}`);
  log(`   ${name}: WebRTC — ${candidates.length} кандидатов, адреса: ${rtcIps.join(', ') || 'нет'}`);
  log(`   ${name}: DNS-серверы (browserleaks): ${dnsIps.slice(0, 8).join(', ') || 'не прочитать'}`);
  log(`   ${name}: уникальное имя ${localDns ? 'ЕСТЬ' : 'нет'} в кэше DNS компьютера`);
  return { ip4, country, ip64, rtcIps, candidates, dnsIps, localDns };
}

async function main() {
  if (!CHROME) throw new Error('Chrome не найден');
  if (!fs.existsSync(NINJA)) throw new Error('нет target/debug/ninja.exe — сначала cargo build -p ninja-motor');
  fs.mkdirSync(RUNTIME, { recursive: true });

  log('Без нашего VPN (как видят сайты, если ходить напрямую — у тебя это адрес Durev)…');
  const plain = await startChrome('plain', 9445, null);
  const marker = (tag) => `ninja-leak-${tag}-${Date.now().toString(36)}`;
  const base = await survey('напрямую', plain.page, marker('plain'));
  plain.page.close();
  plain.proc.kill();

  log(`Подключаюсь через командную строку к «${SERVER}»…`);
  const vpn = await connectCli();
  log(`   подключено: порт 127.0.0.1:${vpn.port}, выход ${vpn.country} (${vpn.ipMasked})`);
  const chrome = await startChrome('vpn', 9444, vpn.port);
  try {
    const via = await survey('через VPN', chrome.page, marker('vpn'));
    check(!!via.ip4 && via.ip4 !== base.ip4, `сайты видят другой IPv4: ${via.ip4} (${via.country}), а не ${base.ip4}`);
    const real = [base.ip4, base.ip64].filter(Boolean);
    check(!!via.ip64 && !real.includes(via.ip64), `проверка IPv6/двойного стека: сайт видит ${via.ip64}, настоящих адресов нет`);
    // WebRTC: «srflx» — адрес, который браузер узнал у STUN-сервера напрямую по UDP, мимо прокси.
    const srflx = via.candidates.filter((c) => / typ srflx /.test(c));
    const rtcReal = via.rtcIps.filter((ip) => real.includes(ip));
    const rtcWhat = via.candidates.length
      ? `${via.candidates.length} кандидатов: ${via.rtcIps.join(', ') || 'только скрытые .local'}`
      : 'кандидатов нет — UDP мимо прокси запрещён';
    check(srflx.length === 0 && rtcReal.length === 0, `WebRTC не выдаёт настоящий адрес (${rtcWhat})`);
    // DNS: напрямую имя должно попасть в кэш компьютера (иначе способ не работает), через VPN — нет.
    if (base.localDns) {
      check(!via.localDns, via.localDns ? 'DNS-УТЕЧКА: имя сайта спросил сам компьютер, а не VPN' : 'DNS не утекает: имена сайтов спрашивает VPN-сервер, а не компьютер');
    } else {
      log('   DNS этим способом не проверить: и напрямую Chrome не кладёт имена в кэш Windows (у него свой DNS-клиент)');
    }
    log(`   DNS-серверы для сведения: напрямую ${base.dnsIps.slice(0, 4).join(', ') || '—'}; через VPN ${via.dnsIps.slice(0, 4).join(', ') || '—'} (у Durev и наших серверов они могут совпадать — это один сервис)`);
    const direct = directConnections('leaktest-vpn');
    check(direct !== null && direct.length === 0, `у Chrome через VPN нет прямых соединений в обход прокси${direct?.length ? ': ' + direct.join(', ') : ''}`);

    // Обрыв: роняем ядро этого подключения (по порту — окно программы с его ядром не трогаем).
    const pid = ps(`(Get-NetTCPConnection -LocalPort ${vpn.port} -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1).OwningProcess`);
    if (pid) {
      ps(`Stop-Process -Id ${pid} -Force`);
      log(`Роняю ядро этого подключения (процесс ${pid}) и снова открываю сайт…`);
      await sleep(800);
      const after = await chrome.page.open('https://cloudflare.com/cdn-cgi/trace');
      const ipAfter = after.text?.match(/^ip=(.+)$/m)?.[1];
      check(!ipAfter, ipAfter ? `ПОСЛЕ ОБРЫВА САЙТ ОТКРЫЛСЯ НАПРЯМУЮ: ${ipAfter}` : `после обрыва сайт не открылся (${after.error ?? 'ошибка прокси'}) — напрямую не ушёл`);
      const directAfter = directConnections('leaktest-vpn');
      check(directAfter !== null && directAfter.length === 0, 'и после обрыва ни одного прямого соединения');
    }
  } finally {
    chrome.page.close();
    chrome.proc.kill();
    vpn.cli.stdin.end('\n');
    await sleep(1500);
    for (const name of ['plain', 'vpn']) fs.rmSync(path.join(RUNTIME, `leaktest-${name}`), { recursive: true, force: true });
  }

  // Окно Chrome, которое открыла сама программа (если сейчас открыто), — только смотрим.
  const appChrome = directConnections('runtime\\browser-');
  if (appChrome === null) log('Окно Chrome от программы сейчас не открыто — его не проверяю.');
  else check(appChrome.length === 0, `окно Chrome, открытое программой: прямых соединений нет${appChrome.length ? ' — есть: ' + appChrome.join(', ') : ''}`);

  const bad = results.filter((r) => !r).length;
  log(`ИТОГ: ${results.length - bad} из ${results.length} проверок без утечек.`);
  fs.writeFileSync(path.join(RUNTIME, 'leak-test.log'), lines.join('\n') + '\n');
  process.exitCode = bad ? 1 : 0;
}

main().catch((e) => {
  log('ОСТАНОВКА: ' + e.message);
  fs.writeFileSync(path.join(RUNTIME, 'leak-test.log'), lines.join('\n') + '\n');
  process.exitCode = 2;
});
