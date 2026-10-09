// Связь интерфейса с мотором (Rust) через Tauri.
// Если страница открыта в обычном браузере (для просмотра вёрстки), мотора нет —
// тогда работает «демо»: выдуманные серверы и подключение по таймеру. Окно честно
// подписывает такой режим, а в настоящем приложении он не включается.

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';

export type VpnState =
  | { state: 'disconnected' }
  | { state: 'preparing' }
  | { state: 'connecting' }
  | { state: 'verifying' }
  | {
      state: 'connected';
      /** Ключ сервера, который на самом деле подключился (может быть резервным). */
      key: string;
      server: string;
      /** Если подключились через резерв — имя сервера, который выбирали. */
      fallbackFrom: string | null;
      exitCountry: string | null;
      exitIp: string | null;
      firstMs: number;
      bulkMs: number;
      port: number;
    }
  | { state: 'failed'; message: string }
  | { state: 'reconnecting'; reason: string }
  | { state: 'disconnecting' };

/** «Сервер не ответил — пробую следующий». */
export interface AttemptInfo {
  attempt: number;
  total: number;
  server: string;
  /** Ядро этого сервера: «Xray 26.3.27». */
  engine: string;
  previousServer: string | null;
  previousReason: string | null;
}

export interface Server {
  key: string;
  name: string;
  summary: string;
  engine: string;
  problem: string | null;
  warnings: string[];
}

/** Источник серверов: подписка сервиса или одиночный ключ. Секретов окно не получает. */
export interface Source {
  id: string;
  name: string;
  kind: 'subscription' | 'key';
  title?: string | null;
  /** Узнаваемая часть ссылки: «your-durev.com» или «vless · host». */
  hint: string;
  usedBytes?: number | null;
  totalBytes?: number | null;
  expires?: string | null;
  /** Когда обновлялся список, секунды Unix. */
  updated?: number | null;
  servers: Server[];
  problem?: string | null;
}

/** Задержка через сервер: `ms` — время ответа, `null` — не ответил. */
export interface ProbeResult {
  key: string;
  ms: number | null;
  error: string | null;
}

export interface BrowserInfo {
  id: string;
  name: string;
}

/** Как запущен обычный браузер: закрыт; через это подключение; настроен на наш прокси, но VPN
 * выключен или на другом порту (сайты не откроет); открыт сам по себе — напрямую. */
export type BrowserRun = 'off' | 'vpn' | 'stale' | 'direct';

/** Как программе сказать «ходи через VPN»: ключом запуска (Chromium/Electron) или переменными окружения. */
export type AppSupport = 'chromium' | 'other';

/** Программа в режиме «Приложения». */
export interface AppItem {
  id: string;
  name: string;
  /** Значок PNG (data URL). */
  icon: string | null;
  /** Запускать сразу после подключения. */
  autostart: boolean;
  /** Какой exe запустится сейчас. */
  exe: string | null;
  support: AppSupport | null;
  problem: string | null;
}

/** Установленная программа (из меню «Пуск» или выбранная файлом) — кандидат в список. */
export interface Program {
  name: string;
  target: string;
  args: string;
  exe: string;
  icon: string | null;
  support: AppSupport;
  /** Приложение из Microsoft Store: его «адрес» в Windows (папка меняется с каждой версией). */
  package?: { aumid: string; family: string; exe: string } | null;
}

/** Что сторож видит у программы прямо сейчас. */
export interface AppStatus {
  id: string;
  /** Сколько процессов запущено (0 — не запущена). */
  processes: number;
  /** Сколько из них запустили мы через VPN. */
  ours: number;
  /** Наши процессы настроены на текущий порт VPN. */
  portOk: boolean;
  viaVpn: number;
  /** Соединения напрямую в интернет: до пяти адресов и сколько всего. */
  direct: string[];
  directTotal: number;
}

export interface LaunchResult {
  id: string;
  /** Программа уже была открыта не через VPN — нужен перезапуск. */
  runningDirect: boolean;
  error: string | null;
}
export interface Backend {
  demo: boolean;
  listSources(): Promise<Source[]>;
  /** Проверить ссылку или ключ и сохранить (зашифровано). */
  addSource(name: string | null, link: string): Promise<Source>;
  refreshSource(id: string): Promise<Source>;
  renameSource(id: string, name: string): Promise<Source>;
  removeSource(id: string): Promise<void>;
  /** Проверить скорость всех серверов источника. */
  probeSource(id: string): Promise<ProbeResult[]>;
  browsers(): Promise<BrowserInfo[]>;
  status(): Promise<VpnState>;
  /** Включён другой VPN на весь компьютер — имя его сетевой карты (иначе null).
   *  С ним режимы через TUN («Устройство», «все, кроме этих») не работают. */
  otherVpn(): Promise<string | null>;
  /** Язык сообщений мотора (ошибки, предупреждения о серверах): ru, en, es, pt, tr, zh, fa. */
  setLanguage(lang: string): Promise<void>;
  /** `keys` — выбранный сервер и разрешённые резервные, по порядку.
   *  `except` — режим «все программы через VPN, кроме этих» (id программ); `null` — обычный. */
  connect(keys: string[], autoRecover: boolean, except?: string[] | null): Promise<void>;
  /** Переключиться на другой сервер, не отключаясь (тот же порт, TUN остаётся). */
  switchServer(keys: string[]): Promise<void>;
  disconnect(): Promise<void>;
  /** Открыть обычный браузер через VPN. Уже открытый напрямую не трогает — отвечает `direct` / `stale`. */
  openBrowser(id: string): Promise<BrowserRun>;
  browserStatus(id: string): Promise<BrowserRun>;
  /** Перезапустить браузер (вкладки вернутся): через VPN или обратно напрямую. */
  restartBrowser(id: string, vpn: boolean): Promise<void>;
  listApps(): Promise<AppItem[]>;
  installedPrograms(): Promise<Program[]>;
  /** Окно «Открыть файл»; 
ull — передумали. */
  pickProgram(): Promise<Program | null>;
  addApp(p: Program): Promise<AppItem>;
  removeApp(id: string): Promise<void>;
  setAppAutostart(id: string, on: boolean): Promise<void>;
  /** Сторож работает, пока выбран режим «Приложения». */
  setAppsGuard(on: boolean): Promise<void>;
  appsStatus(): Promise<AppStatus[]>;
  launchApps(ids: string[]): Promise<LaunchResult[]>;
  restartApp(id: string): Promise<void>;
  closeApp(id: string): Promise<void>;
  onAppsStatus(callback: (s: AppStatus[]) => void): Promise<() => void>;
  onState(callback: (s: VpnState) => void): Promise<() => void>;
  onAttempt(callback: (a: AttemptInfo) => void): Promise<() => void>;
  /** Windows спрашивает разрешение администратора (режим «все, кроме этих»). */
  onElevation(callback: (waiting: boolean) => void): Promise<() => void>;
  /** Открыть страницу «ключ с телефона» в домашней сети: адрес и QR-код (SVG). */
  pairStart(): Promise<{ url: string; qrSvg: string }>;
  pairStop(): Promise<void>;
  /** С телефона пришёл ключ или подписка — уже добавлены как источник. */
  onPairAdded(callback: (source: Source) => void): Promise<() => void>;
  minimize(): void;
  toggleMaximize(): void;
  close(): void;
}

const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

function tauriBackend(): Backend {
  const win = getCurrentWindow();
  return {
    demo: false,
    listSources: () => invoke('list_sources'),
    addSource: (name, link) => invoke('add_source', { name, link }),
    refreshSource: (id) => invoke('refresh_source', { id }),
    renameSource: (id, name) => invoke('rename_source', { id, name }),
    removeSource: (id) => invoke('remove_source', { id }),
    probeSource: (id) => invoke('probe_source', { id }),
    browsers: () => invoke('browsers'),
    status: () => invoke('status'),
    otherVpn: () => invoke('other_vpn'),
    setLanguage: (lang) => invoke('set_language', { lang }),
    connect: (keys, autoRecover, except) => invoke('connect_vpn', { keys, autoRecover, except: except ?? null }),
    switchServer: (keys) => invoke('switch_server', { keys }),
    disconnect: () => invoke('disconnect_vpn'),
    openBrowser: (id) => invoke('open_browser', { id }),
    browserStatus: (id) => invoke('browser_status', { id }),
    restartBrowser: (id, vpn) => invoke('restart_browser', { id, vpn }),
    listApps: () => invoke('list_apps'),
    installedPrograms: () => invoke('installed_programs'),
    pickProgram: () => invoke('pick_program'),
    addApp: (p) => invoke('add_app', { name: p.name, target: p.target, args: p.args, icon: p.icon, package: p.package ?? null }),
    removeApp: (id) => invoke('remove_app', { id }),
    setAppAutostart: (id, on) => invoke('set_app_autostart', { id, on }),
    setAppsGuard: (on) => invoke('set_apps_guard', { on }),
    appsStatus: () => invoke('apps_status'),
    launchApps: (ids) => invoke('launch_apps', { ids }),
    restartApp: (id) => invoke('restart_app', { id }),
    closeApp: (id) => invoke('close_app', { id }),
    onAppsStatus: (callback) => listen<AppStatus[]>('apps-status', (e) => callback(e.payload)),
    onState: (callback) => listen<VpnState>('vpn-state', (e) => callback(e.payload)),
    onAttempt: (callback) => listen<AttemptInfo>('vpn-attempt', (e) => callback(e.payload)),
    onElevation: (callback) => listen<boolean>('vpn-elevation', (e) => callback(e.payload)),
    pairStart: () => invoke('pair_start'),
    pairStop: () => invoke('pair_stop'),
    onPairAdded: (callback) => listen<Source>('pair-added', (e) => callback(e.payload)),
    minimize: () => void win.minimize(),
    toggleMaximize: () => void win.toggleMaximize(),
    close: () => void win.close(),
  };
}

/** Только для просмотра вёрстки в браузере: не подключается никуда. */
function demoBackend(): Backend {
  const demoServers = (source: string, list: string[][]): Server[] =>
    list.map(([name, summary, engine]) => ({ key: `${source}/${name}`, name, summary, engine, problem: null, warnings: [] }));
  const durev = demoServers('demo1', [
    ['Auto → [🚀 Оптимальная локация]', 'VLESS · XHTTP · REALITY', 'Xray 26.3.27'],
    ['Netherlands 🇳🇱', 'VLESS · XHTTP · REALITY', 'Xray 26.3.27'],
    ['Germany 🇩🇪', 'VLESS · XHTTP · REALITY', 'Xray 26.3.27'],
    ['Finland 🇫🇮', 'VLESS · XHTTP · REALITY', 'Xray 26.3.27'],
    ['Russia 🇷🇺 → [🏛 Госуслуги]', 'VLESS · XHTTP · REALITY', 'Xray 26.3.27'],
    ['Germany 92 🇩🇪 → [📃 Белые списки]', 'VLESS · WebSocket · TLS', 'sing-box 1.14.2'],
  ]);
  let sources: Source[] = [
    { id: 'demo1', name: 'Демо-подписка', kind: 'subscription', title: 'Демо-подписка', hint: 'sub.example.com', usedBytes: 0, expires: '2027-03-17', updated: Date.now() / 1000 - 3600, servers: durev },
    { id: 'demo2', name: 'Свой сервер', kind: 'key', hint: 'vless · nl.example.com', updated: Date.now() / 1000 - 86400, servers: demoServers('demo2', [['Мой сервер 🇳🇱', 'VLESS · TCP · REALITY · Vision', 'sing-box 1.14.2']]) },
  ];
  const find = (id: string) => {
    const s = sources.find((x) => x.id === id);
    if (!s) throw 'Источник не найден';
    return s;
  };
  const listeners = new Set<(s: VpnState) => void>();
  const attemptListeners = new Set<(a: AttemptInfo) => void>();
  let timers: number[] = [];
  let pairTimer = 0;
  const pairListeners = new Set<(s: Source) => void>();
  // Демо: обычный браузер уже открыт напрямую — видно предложение перезапуска.
  let demoBrowser: BrowserRun = 'direct';
  const send = (s: VpnState) => {
    connectedPort = s.state === 'connected' ? s.port : null;
    listeners.forEach((l) => l(s));
    pushApps();
  };
  // Демо режима «Приложения»: Discord уже открыт сам по себе (без VPN), остальные — нет.
  const demoPrograms: Program[] = [
    { name: 'Discord', target: 'C:\\Users\\demo\\AppData\\Local\\Discord\\Update.exe', args: '--processStart Discord.exe', exe: 'C:\\Users\\demo\\AppData\\Local\\Discord\\app-1.0.9203\\Discord.exe', icon: null, support: 'chromium' },
    { name: 'Telegram', target: 'C:\\Users\\demo\\AppData\\Roaming\\Telegram Desktop\\Telegram.exe', args: '', exe: 'C:\\Users\\demo\\AppData\\Roaming\\Telegram Desktop\\Telegram.exe', icon: null, support: 'other' },
    { name: 'Visual Studio Code', target: 'C:\\Users\\demo\\AppData\\Local\\Programs\\Microsoft VS Code\\Code.exe', args: '', exe: 'C:\\Users\\demo\\AppData\\Local\\Programs\\Microsoft VS Code\\Code.exe', icon: null, support: 'chromium' },
    { name: 'Steam', target: 'C:\\Program Files (x86)\\Steam\\steam.exe', args: '', exe: 'C:\\Program Files (x86)\\Steam\\steam.exe', icon: null, support: 'other' },
  ];
  let apps: AppItem[] = [];
  const running = new Map<string, 'ours' | 'direct'>([]);
  let connectedPort: number | null = null;
  const appsListeners = new Set<(s: AppStatus[]) => void>();
  const appsNow = (): AppStatus[] =>
    apps.map((a) => {
      const how = running.get(a.id);
      const ours = how === 'ours';
      return {
        id: a.id,
        processes: how ? 4 : 0,
        ours: ours ? 4 : 0,
        portOk: ours && connectedPort !== null,
        viaVpn: ours && connectedPort !== null ? 7 : 0,
        direct: how === 'direct' ? ['162.159.135.232:443', '162.159.128.233:443'] : [],
        directTotal: how === 'direct' ? 3 : 0,
      };
    });
  const pushApps = () => appsListeners.forEach((l) => l(appsNow()));
  const toItem = (p: Program, id: string): AppItem => ({ id, name: p.name, icon: p.icon, autostart: true, exe: p.exe, support: p.support, problem: null });
  const demo: Backend = {
    demo: true,
    listSources: async () => sources,
    addSource: async (name, link) => {
      await new Promise((r) => setTimeout(r, 700));
      const kind = link.startsWith('https://') ? 'subscription' : link.startsWith('vless://') ? 'key' : null;
      if (!kind) throw 'Демо: это не ссылка-подписка (https://…) и не ключ vless://';
      const id = 'demo' + (sources.length + 1);
      const added: Source = { id, name: name || 'Новый источник', kind, hint: 'example.org', updated: Date.now() / 1000, servers: demoServers(id, [['Finland 🇫🇮', 'VLESS · XHTTP · REALITY', 'Xray 26.3.27']]) };
      sources = [...sources, added];
      return added;
    },
    refreshSource: async (id) => {
      await new Promise((r) => setTimeout(r, 700));
      return { ...find(id), updated: Date.now() / 1000 };
    },
    renameSource: async (id, name) => {
      sources = sources.map((s) => (s.id === id ? { ...s, name } : s));
      return find(id);
    },
    removeSource: async (id) => {
      sources = sources.filter((s) => s.id !== id);
    },
    probeSource: async (id) => {
      await new Promise((r) => setTimeout(r, 1500));
      return find(id).servers.map((s, i) =>
        i % 4 === 3 ? { key: s.key, ms: null, error: 'нет ответа' } : { key: s.key, ms: 90 + ((i * 137) % 900), error: null },
      );
    },
    browsers: async () => [{ id: 'edge', name: 'Edge' }, { id: 'chrome', name: 'Chrome' }],
    status: async () => ({ state: 'disconnected' }),
    // Демо: ?other-vpn=DurevVPN — показать предупреждение о другом VPN.
    otherVpn: async () => new URLSearchParams(location.search).get('other-vpn'),
    setLanguage: async () => {},
    // Демо: серверы «Белых списков» «не отвечают» — так видно и резерв, и ошибку.
    connect: async (keys) => {
      const name = (k: string) => k.split('/').slice(1).join('/');
      const steps: [number, () => void][] = [];
      let t = 0;
      const ok = keys.findIndex((k) => !k.includes('Белые'));
      const tried = ok < 0 ? keys : keys.slice(0, ok + 1);
      const engineOf = (key: string) => sources.flatMap((s) => s.servers).find((x) => x.key === key)?.engine ?? '';
      tried.forEach((key, i) => {
        steps.push([t, () => attemptListeners.forEach((l) => l({ attempt: i + 1, total: keys.length, server: name(key), engine: engineOf(key), previousServer: i ? name(keys[i - 1]) : null, previousReason: i ? 'сервер не отвечает' : null }))]);
        steps.push([t, () => send({ state: 'preparing' })]);
        steps.push([t + 300, () => send({ state: 'connecting' })]);
        steps.push([t + 900, () => send({ state: 'verifying' })]);
        t += 1800;
      });
      steps.push(
        ok < 0
          ? [t, () => send({ state: 'failed', message: 'Демо: ни один сервер не ответил — так выглядит ошибка.' })]
          : [t + 400, () => send({ state: 'connected', key: keys[ok], server: name(keys[ok]), fallbackFrom: ok > 0 ? name(keys[0]) : null, exitCountry: 'NL', exitIp: '89.*.*.149', firstMs: 508, bulkMs: 450, port: 2080 })],
      );
      timers = steps.map(([ms, f]) => window.setTimeout(f, ms));
    },
    // Демо: переключение на лету — то же подключение, без «Отключено» между ними.
    switchServer: async (keys) => {
      timers.forEach(clearTimeout);
      await demo.connect(keys, true);
    },
    disconnect: async () => {
      timers.forEach(clearTimeout);
      send({ state: 'disconnecting' });
      window.setTimeout(() => send({ state: 'disconnected' }), 400);
    },
    openBrowser: async () => (demoBrowser = 'vpn'),
    browserStatus: async () => demoBrowser,
    restartBrowser: async (_id, vpn) => {
      await new Promise((r) => setTimeout(r, 900));
      demoBrowser = vpn ? 'vpn' : 'direct';
    },
    listApps: async () => apps,
    installedPrograms: async () => {
      await new Promise((r) => setTimeout(r, 400));
      return demoPrograms;
    },
    pickProgram: async () => null,
    addApp: async (p) => {
      if (apps.some((a) => a.exe === p.exe)) throw `«${p.name}» уже в списке.`;
      const item = toItem(p, 'demo-app-' + (apps.length + 1));
      apps = [...apps, item];
      if (p.name === 'Discord') running.set(item.id, 'direct');
      pushApps();
      return item;
    },
    removeApp: async (id) => {
      apps = apps.filter((a) => a.id !== id);
      running.delete(id);
      pushApps();
    },
    setAppAutostart: async (id, on) => {
      apps = apps.map((a) => (a.id === id ? { ...a, autostart: on } : a));
    },
    setAppsGuard: async () => {},
    appsStatus: async () => appsNow(),
    launchApps: async (ids) => {
      const results = ids.map((id) => {
        const directNow = running.get(id) === 'direct';
        if (!directNow) running.set(id, 'ours');
        return { id, runningDirect: directNow, error: null };
      });
      pushApps();
      return results;
    },
    restartApp: async (id) => {
      await new Promise((r) => setTimeout(r, 900));
      running.set(id, 'ours');
      pushApps();
    },
    closeApp: async (id) => {
      running.delete(id);
      pushApps();
    },
    onAppsStatus: async (callback) => {
      const own = (s: AppStatus[]) => callback(s);
      appsListeners.add(own);
      return () => void appsListeners.delete(own);
    },    onState: async (callback) => {
      // Своя обёртка на каждую подписку: одна и та же функция может подписаться дважды.
      const own = (s: VpnState) => callback(s);
      listeners.add(own);
      return () => void listeners.delete(own);
    },
    onAttempt: async (callback) => {
      const own = (a: AttemptInfo) => callback(a);
      attemptListeners.add(own);
      return () => void attemptListeners.delete(own);
    },
    onElevation: async () => () => {},
    // Демо: вместо настоящего QR — рамка; через 4 с «телефон» присылает ключ.
    pairStart: async () => {
      window.clearTimeout(pairTimer);
      pairTimer = window.setTimeout(() => {
        const id = 'phone' + (sources.length + 1);
        const added: Source = { id, name: 'С телефона', kind: 'key', hint: 'vless · phone.example.com', updated: Date.now() / 1000, servers: demoServers(id, [['Finland 🇫🇮', 'VLESS · TCP · REALITY', 'sing-box 1.14.2']]) };
        sources = [...sources, added];
        pairListeners.forEach((l) => l(added));
      }, 4000);
      return {
        url: 'http://192.168.1.20:52814/p/demo',
        qrSvg: '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 29 29" width="232" height="232"><rect width="29" height="29" fill="#fff"/><path d="M2 2h7v7H2zM20 2h7v7h-7zM2 20h7v7H2z" fill="none" stroke="#14283a" stroke-width="1.6"/><text x="14.5" y="16" font-size="3" text-anchor="middle" fill="#14283a">ДЕМО</text></svg>',
      };
    },
    pairStop: async () => window.clearTimeout(pairTimer),
    onPairAdded: async (callback) => {
      pairListeners.add(callback);
      return () => pairListeners.delete(callback);
    },
    minimize: () => {},
    toggleMaximize: () => {},
    close: () => {},
  };
  return demo;
}

export const backend: Backend = inTauri ? tauriBackend() : demoBackend();
