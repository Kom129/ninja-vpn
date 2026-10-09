// Главное окно ninja-vpn. Композиция — как на выбранном референсе docs/design/selected-glass.png:
// слева «откуда» (браузер), справа «куда» (сервер), между ними нить/волна с ниндзя,
// под ними кнопка, переключатель режимов и нижнее меню; справа выдвижная панель профилей.
// Все надписи — из словарей src/i18n (7 языков); язык выбирается во вкладке «Правила».

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { THEME_KEY, THEMES, followTheme, type ThemeChoice } from './theme';
import {
  ChevronDown, Globe, Layers, LayoutGrid, Link2, Minus, Monitor, SlidersHorizontal, Square, X,
} from 'lucide-react';
import { backend, type AppItem, type AppStatus, type AttemptInfo, type BrowserInfo, type BrowserRun, type ProbeResult, type Program, type Source, type VpnState } from './api';
import { connectPlan, RESERVE_OPTIONS, type ReservePolicy } from './reserve';
import { countryName, parseServerName, setNamesLocale, transportOf } from './names';
import { I18nProvider, LANGS, detectLang, makeI18n, useI18n, type Key, type Lang } from './i18n';
import { NinjaCanvas } from './ninja/NinjaCanvas';
import { Place } from './components/Place';
import { ServerPanel } from './components/ServerPanel';
import { AppIcon, AppPicker, AppsBar, describe } from './components/AppsBar';

type Mode = 'device' | 'apps' | 'browser';

const MODES: Mode[] = ['device', 'apps', 'browser'];

const STATUS: Record<VpnState['state'], Key> = {
  disconnected: 'status.disconnected',
  preparing: 'status.connecting',
  connecting: 'status.connecting',
  verifying: 'status.connecting',
  connected: 'status.connected',
  failed: 'status.failed',
  reconnecting: 'status.reconnecting',
  disconnecting: 'status.disconnecting',
};

/** Геометрия сцены: должна совпадать с размерами орбов в styles.css. */
const ORB_CENTER_X = 80;
const ORB_RADIUS = 52;
const ORB_TOP = 60;
/** Стрелка выбора справа от кружка: 10 px зазор + 34 px кнопка. */
const CHEVRON_SPACE = 10 + 34 + 8;

/** Значение, которое помнится между запусками. Сеттер принимает и функцию «от прошлого значения» —
 *  так несколько быстрых обновлений подряд не затирают друг друга. */
function useStored<T>(key: string, initial: T | (() => T)): [T, (v: T | ((prev: T) => T)) => void] {
  const [value, setValue] = useState<T>(() => {
    const fallback = () => (typeof initial === 'function' ? (initial as () => T)() : initial);
    try {
      const raw = localStorage.getItem(key);
      return raw === null ? fallback() : (JSON.parse(raw) as T);
    } catch {
      return fallback();
    }
  });
  const set = (v: T | ((prev: T) => T)) => {
    setValue((prev) => {
      const next = typeof v === 'function' ? (v as (prev: T) => T)(prev) : v;
      try {
        localStorage.setItem(key, JSON.stringify(next));
      } catch {
        /* хранилище недоступно — просто не запоминаем */
      }
      return next;
    });
  };
  return [value, set];
}

/** Человеческое имя сервера: «Germany 92 🇩🇪 → [📃 Белые списки]» → «Германия 92». */
const title = (name: string) => parseServerName(name).title;

export default function App() {
  // Язык окна: сначала — как у Windows, потом — что выбрал человек.
  const [lang, setLang] = useStored<Lang>('ninja.lang', detectLang);
  const [theme, setTheme] = useStored<ThemeChoice>(THEME_KEY, 'system');
  useEffect(() => followTheme(theme), [theme]);
  const i18n = useMemo(() => {
    const made = makeI18n(lang);
    setNamesLocale(made.info.locale); // названия стран — на том же языке
    return made;
  }, [lang]);
  const { t, num } = i18n;

  const [vpn, setVpn] = useState<VpnState>({ state: 'disconnected' });
  const [sources, setSources] = useState<Source[] | null>(null);
  const [sourcesError, setSourcesError] = useState<string | null>(null);
  // Результаты проверки скорости по ключу сервера (запоминаются между запусками).
  const [probes, setProbes] = useStored<Record<string, ProbeResult>>('ninja.probes', {});
  const [browsers, setBrowsers] = useState<BrowserInfo[]>([]);
  const [serverKey, setServerKey] = useStored<string | null>('ninja.server', null);
  const [mode, setMode] = useStored<Mode>('ninja.mode', 'browser');
  const [browserId, setBrowserId] = useStored<string | null>('ninja.browser', null);
  const [panelOpen, setPanelOpen] = useStored('ninja.panel', true);
  const [view, setView] = useState<'connection' | 'rules'>('connection');
  const [reserve, setReserve] = useStored<ReservePolicy>('ninja.reserve', 'group');
  const [autoRecover, setAutoRecover] = useStored('ninja.autoRecover', true);
  const [attempt, setAttempt] = useState<AttemptInfo | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [showDetails, setShowDetails] = useState(false);
  const [browserMenu, setBrowserMenu] = useState(false);
  // Режим «Приложения»: список программ, что видит сторож, окно «Добавить», заметка после запуска.
  const [apps, setApps] = useState<AppItem[]>([]);
  const [appStatuses, setAppStatuses] = useState<Record<string, AppStatus>>({});
  const [pickerOpen, setPickerOpen] = useState(false);
  const [appsNote, setAppsNote] = useState<string | null>(null);
  // «Только эти через VPN» (запуск через прокси) или «все, кроме этих» (TUN, права администратора).
  const [split, setSplit] = useStored<'only' | 'except'>('ninja.appsSplit', 'only');
  const [elevating, setElevating] = useState(false);

  // Язык страницы (для экранного диктора и шрифтов) и направление письма: персидский — справа налево.
  useEffect(() => {
    document.documentElement.lang = i18n.info.locale;
    document.documentElement.dir = i18n.info.dir;
  }, [i18n]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    let unlistenAttempt: (() => void) | undefined;
    backend.onState(setVpn).then((u) => (cancelled ? u() : (unlisten = u)));
    backend.onAttempt(setAttempt).then((u) => (cancelled ? u() : (unlistenAttempt = u)));
    let unlistenElevation: (() => void) | undefined;
    backend.onElevation(setElevating).then((u) => (cancelled ? u() : (unlistenElevation = u)));
    backend.status().then((s) => !cancelled && setVpn(s));
    // Сначала говорим мотору язык — тогда предупреждения о серверах сразу придут на нём.
    backend
      .setLanguage(lang)
      .catch(() => {})
      .finally(() => {
        backend.listSources().then(
          (list) => !cancelled && setSources(list),
          (e) => !cancelled && setSourcesError(String(e)),
        );
        backend.listApps().then((a) => !cancelled && setApps(a));
      });
    backend.browsers().then((b) => !cancelled && setBrowsers(b));
    let unlistenApps: (() => void) | undefined;
    const takeStatuses = (list: AppStatus[]) => setAppStatuses(Object.fromEntries(list.map((s) => [s.id, s])));
    backend.onAppsStatus(takeStatuses).then((u) => (cancelled ? u() : (unlistenApps = u)));
    return () => {
      cancelled = true;
      unlisten?.();
      unlistenAttempt?.();
      unlistenApps?.();
      unlistenElevation?.();
    };
    // Язык при запуске берём один раз; смену языка обрабатывает эффект ниже.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Сменили язык — мотор тоже переходит на него, а тексты, которые он уже прислал
  // (почему сервер не подходит, почему программа не найдена), запрашиваем заново.
  const firstLang = useRef(true);
  useEffect(() => {
    if (firstLang.current) {
      firstLang.current = false;
      return;
    }
    let cancelled = false;
    // Ошибка составлена на прежнем языке — убираем (ошибку подключения мотор уберёт сам).
    setActionError(null);
    backend
      .setLanguage(lang)
      .catch(() => {})
      .finally(() => {
        backend.listSources().then((list) => !cancelled && setSources(list), () => {});
        backend.listApps().then((a) => !cancelled && setApps(a), () => {});
      });
    return () => {
      cancelled = true;
    };
  }, [lang]);

  // Все подходящие серверы всех источников, вместе с источником (для подписи «Основной · XHTTP»).
  const usable = useMemo(
    () => (sources ?? []).flatMap((source) => source.servers.filter((s) => !s.problem).map((server) => ({ server, source }))),
    [sources],
  );
  const picked = usable.find((u) => u.server.key === serverKey) ?? usable[0] ?? null;
  const server = picked?.server ?? null;
  const browser = browsers.find((b) => b.id === browserId) ?? browsers[0] ?? null;
  const except = mode === 'apps' && split === 'except';
  const subtitle = except ? t('mode.apps.exceptSubtitle') : t(`mode.${mode}.subtitle`);
  const busy = vpn.state === 'preparing' || vpn.state === 'connecting' || vpn.state === 'verifying' || vpn.state === 'reconnecting';
  const connected = vpn.state === 'connected' ? vpn : null;
  // Какой сервер показывать справа: после подключения — тот, что на самом деле сработал (мог быть резерв).
  const shown = (connected && usable.find((u) => u.server.key === connected.key)) || picked;
  const shownParsed = shown ? parseServerName(shown.server.name) : null;
  const locked = busy || vpn.state === 'connected' || vpn.state === 'disconnecting';
  // Сервер можно выбрать и во время работы — переключимся на лету; нельзя только пока подключаемся или отключаемся.
  const selectLocked = busy || vpn.state === 'disconnecting';
  // Режимы, где весь компьютер идёт через нашу сетевую карту (TUN).
  const tunMode = mode === 'device' || except;
  // Другой VPN на весь компьютер (Durev и т. п.) с нашим TUN не уживается — проверяем, пока выбран такой режим.
  const [otherVpn, setOtherVpn] = useState<string | null>(null);
  useEffect(() => {
    if (!tunMode || locked) {
      setOtherVpn(null);
      return;
    }
    let alive = true;
    const check = () => backend.otherVpn().then((name) => alive && setOtherVpn(name), () => {});
    check();
    const timer = setInterval(check, 3000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [tunMode, locked]);
  const canConnect =
    !!server && !(tunMode && otherVpn) && (mode === 'browser' || mode === 'device' || (mode === 'apps' && (except || apps.length > 0)));

  // Сторож программ работает, пока выбран режим «Приложения».
  useEffect(() => {
    backend.setAppsGuard(mode === 'apps' && split === 'only').catch(() => {});
    if (mode === 'apps' && split === 'only') backend.appsStatus().then((list) => setAppStatuses(Object.fromEntries(list.map((s) => [s.id, s]))));
  }, [mode, split]);

  // В режиме «Приложения» после подключения открываем программы с галочкой «Открывать при подключении».
  // Уже открытые без VPN не трогаем без спроса — подсказываем перезапустить.
  const wantApps = useRef(false);
  useEffect(() => {
    if (vpn.state === 'failed' || vpn.state === 'disconnected') wantApps.current = false;
    if (vpn.state !== 'connected' || !wantApps.current || mode !== 'apps' || split !== 'only') return;
    wantApps.current = false;
    const ids = apps.filter((a) => a.autostart && !a.problem).map((a) => a.id);
    if (!ids.length) return;
    backend.launchApps(ids).then(
      (results) => {
        const name = (id: string) => apps.find((a) => a.id === id)?.name ?? id;
        const direct = results.filter((r) => r.runningDirect).map((r) => name(r.id));
        const failed = results.filter((r) => r.error).map((r) => `${name(r.id)}: ${r.error}`);
        const notes = [];
        if (direct.length) notes.push(t('apps.alreadyDirect', { names: direct.join(', ') }));
        if (failed.length) notes.push(failed.join('; '));
        setAppsNote(notes.length ? notes.join(' ') : null);
      },
      (e) => setAppsNote(String(e)),
    );
  }, [vpn.state, mode, apps, split, t]);

  // Как запущен обычный браузер (закрыт / через VPN / напрямую) — спрашиваем раз в 2 с, пока выбран «Браузер».
  const [browserRun, setBrowserRun] = useState<BrowserRun | null>(null);
  const [browserRestarting, setBrowserRestarting] = useState(false);
  const browserKey = mode === 'browser' ? (browser?.id ?? null) : null;
  useEffect(() => setBrowserRun(null), [browserKey]);
  useEffect(() => {
    if (!browserKey) return;
    let alive = true;
    const check = () => backend.browserStatus(browserKey).then((run) => alive && setBrowserRun(run), () => {});
    check();
    const timer = setInterval(check, 2000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [browserKey, vpn.state]);

  // В режиме «Браузер» после настоящего подключения сразу открываем твой обычный браузер —
  // один раз на нажатие «Подключить». Если он уже открыт напрямую, мотор его не трогает,
  // а окно предлагает перезапуск. После автопереподключения порт прежний, и уже открытый
  // браузер продолжает работать — второй раз не открываем. После перезагрузки окна — тоже.
  const wantBrowser = useRef(false);
  useEffect(() => {
    if (vpn.state === 'failed' || vpn.state === 'disconnected') wantBrowser.current = false;
    if (vpn.state === 'connected' && wantBrowser.current && mode === 'browser' && browser) {
      wantBrowser.current = false;
      backend.openBrowser(browser.id).then(setBrowserRun, (e) => setActionError(String(e)));
    }
  }, [vpn.state, mode, browser]);

  const restartBrowser = (vpnOn: boolean) => {
    if (!browser) return;
    setActionError(null);
    setBrowserRestarting(true);
    backend
      .restartBrowser(browser.id, vpnOn)
      .then(() => setBrowserRun(vpnOn ? 'vpn' : 'direct'), (e) => setActionError(String(e)))
      .finally(() => setBrowserRestarting(false));
  };

  useEffect(() => setShowDetails(false), [vpn.state]);

  // Выбрал другой сервер после ошибки — старая ошибка относится к прошлому выбору, убираем её.
  const failureFor = useRef(serverKey);
  useEffect(() => {
    if (failureFor.current === serverKey) return;
    failureFor.current = serverKey;
    setActionError(null);
    setVpn((current) => (current.state === 'failed' ? { state: 'disconnected' } : current));
  }, [serverKey]);
  useEffect(() => {
    if (vpn.state === 'connected' || vpn.state === 'failed' || vpn.state === 'disconnected') setAttempt(null);
  }, [vpn.state]);

  const closePicker = useCallback(() => setPickerOpen(false), []);

  // Выбрал другой сервер, пока подключено, — переключаемся сами, без «Отключить» → «Подключить».
  const [switchingTo, setSwitchingTo] = useState<string | null>(null);
  useEffect(() => {
    if (!busy) setSwitchingTo(null);
  }, [busy]);
  const selectServer = (key: string) => {
    if (key === serverKey) return;
    setServerKey(key);
    if (vpn.state !== 'connected') return;
    setActionError(null);
    setAttempt(null);
    setSwitchingTo(usable.find((u) => u.server.key === key)?.server.name ?? null);
    backend.switchServer(connectPlan(key, sources ?? [], probes, reserve)).catch((e) => {
      setSwitchingTo(null);
      setActionError(String(e));
    });
  };

  const onPrimary = async () => {
    setActionError(null);
    try {
      if (busy || vpn.state === 'connected') await backend.disconnect();
      else if (server) {
        setAttempt(null);
        wantBrowser.current = true;
        wantApps.current = true;
        setAppsNote(null);
        await backend.connect(connectPlan(server.key, sources ?? [], probes, reserve), autoRecover, mode === 'device' ? [] : except ? apps.map((a) => a.id) : null);
      }
    } catch (e) {
      setActionError(String(e));
    }
  };

  // Действия с источниками: ошибки возвращаем панели, она покажет их у нужного источника.
  const replaceSource = (updated: Source) => setSources((list) => (list ?? []).map((s) => (s.id === updated.id ? updated : s)));
  const sourceActions = {
    onAdd: async (name: string | null, link: string) => {
      const added = await backend.addSource(name, link);
      setSources((list) => [...(list ?? []), added]);
      // Первый источник — сразу выбираем его первый подходящий сервер.
      if (!server) {
        const first = added.servers.find((s) => !s.problem);
        if (first) setServerKey(first.key);
      }
    },
    onRefresh: async (id: string) => replaceSource(await backend.refreshSource(id)),
    onRename: async (id: string, name: string) => replaceSource(await backend.renameSource(id, name)),
    onProbe: async (id: string) => {
      const results = await backend.probeSource(id);
      setProbes((prev) => ({ ...prev, ...Object.fromEntries(results.map((r) => [r.key, r])) }));
    },
    onRemove: async (id: string) => {
      await backend.removeSource(id);
      setSources((list) => (list ?? []).filter((s) => s.id !== id));
    },
  };

  const primaryLabel =
    vpn.state === 'connected' ? t('btn.disconnect')
    : busy ? t('btn.cancel')
    : vpn.state === 'disconnecting' ? t('btn.disconnecting')
    : vpn.state === 'failed' ? t('btn.retry')
    : t('btn.connect');

  const failure = vpn.state === 'failed' ? vpn.message : actionError;
  const [failureLine, ...failureRest] = (failure ?? '').split('\n');
  // Мотор пишет с маленькой буквы (так удобнее склеивать фразы) — в подписи начинаем с большой.
  const failureHead = failureLine.charAt(0).toLocaleUpperCase(i18n.info.locale) + failureLine.slice(1);
  const retrying = busy && attempt && attempt.attempt > 1 ? attempt : null;
  const caption =
    vpn.state === 'reconnecting' && retrying ? t('cap.reconnectRetry', { prev: title(retrying.previousServer ?? ''), next: title(retrying.server), n: retrying.attempt, total: retrying.total })
    : vpn.state === 'reconnecting' ? (attempt ? t('cap.reconnectingTo', { reason: vpn.reason, server: title(attempt.server) }) : t('cap.reconnecting', { reason: vpn.reason }))
    : retrying ? t('cap.retry', { prev: title(retrying.previousServer ?? ''), next: title(retrying.server), n: retrying.attempt, total: retrying.total })
    : switchingTo && busy ? t('cap.switching', { server: title(switchingTo) })
    : vpn.state === 'preparing' ? t('cap.preparing')
    : vpn.state === 'connecting' ? t('cap.connecting', { engine: attempt?.engine ?? server?.engine ?? '' })
    : elevating ? t('cap.elevating')
    : vpn.state === 'verifying' ? t('cap.verifying')
    : connected ? t('cap.connected', { first: connected.firstMs, bulk: num(connected.bulkMs / 1000) })
    : vpn.state === 'disconnecting' ? t('cap.disconnecting')
    : failure ? failureHead
    : tunMode && otherVpn ? t('cap.otherVpn')
    : mode === 'device' && server ? t('cap.readyDevice')
    : except && server ? (apps.length ? t('cap.readyExcept') : t('cap.readyDevice'))
    : mode === 'apps' && !apps.length ? t('cap.addApps')
    : mode === 'apps' && server ? t('cap.readyApps')
    : server ? t('cap.ready')
    : t('cap.noServer');

  // Левый кружок в режиме «Приложения»: значки программ, название и короткий итог сторожа.
  const appTones = except ? [] : apps.map((a) => describe(a, appStatuses[a.id], !!connected, t).tone);
  const appsBad = appTones.filter((tone) => tone === 'bad').length;
  const appsOk = appTones.filter((tone) => tone === 'ok').length;
  // Подсказка «перезапусти через VPN» больше не нужна, когда всё идёт как надо.
  const appsAllFine = appTones.every((tone) => tone !== 'bad' && tone !== 'warn');
  useEffect(() => {
    if (appsAllFine) setAppsNote(null);
  }, [appsAllFine]);
  const appsTitle = except && !apps.length ? t('mode.device.title') : apps.length === 1 ? apps[0].name : apps.length ? t('left.apps', { n: apps.length }) : t('mode.apps');
  const appsMeta = except ? (apps.length ? t('left.appsExcept') : t('left.noExceptions'))
    : !apps.length ? t('left.addBelow')
    : appsBad ? t('left.bypass', { n: appsBad })
    : connected ? t('left.viaVpn', { ok: appsOk, total: apps.length })
    : t('left.willOpen');

  const appActions = {
    onLaunch: async (id: string) => {
      const [result] = await backend.launchApps([id]);
      if (result?.error) throw result.error;
      if (result?.runningDirect) throw t('apps.launchDirect');
    },
    onRestart: (id: string) => backend.restartApp(id),
    onClose: (id: string) => backend.closeApp(id),
    onRemove: async (id: string) => {
      await backend.removeApp(id);
      setApps((list) => list.filter((a) => a.id !== id));
    },
    onAutostart: async (id: string, on: boolean) => {
      await backend.setAppAutostart(id, on);
      setApps((list) => list.map((a) => (a.id === id ? { ...a, autostart: on } : a)));
    },
  };
  const addApp = async (p: Program) => {
    const added = await backend.addApp(p);
    setApps((list) => [...list, added]);
  };

  const exitCountry = connected?.exitCountry ?? null;
  const rightFlag = exitCountry ?? shownParsed?.country ?? null;
  // «При проверке»: у серверов-балансировщиков («Оптимальная локация») разные соединения
  // могут выходить в разных странах — честно говорим, что это страна нашей проверки.
  const rightMeta = connected && exitCountry
    ? t('right.exit', { country: countryName(exitCountry) }) + (connected.exitIp ? ` · ${connected.exitIp}` : '')
    : shown ? `${shown.source.name} · ${transportOf(shown.server.summary)}` : t('right.noServer');
  const fallbackNote = connected?.fallbackFrom ? t('right.fallback', { name: parseServerName(connected.fallbackFrom).title }) : null;

  return (
    <I18nProvider value={i18n}>
    <div className={`app${panelOpen ? '' : ' wide'}`} data-state={vpn.state}>
      <WindowControls />
      <main className="main">
        <header className="header" data-tauri-drag-region>
          <span className="brand" data-tauri-drag-region>VPN</span>
          <span className="status" role="status">
            <span className="led" aria-hidden="true" />
            {t(STATUS[vpn.state])}
          </span>
        </header>

        {backend.demo && <div className="demo-ribbon">{t('demo.ribbon')}</div>}

        {view === 'connection' ? (
          <section className="content">
            <div className="heading">
              <h1>{t(`mode.${mode}.title`)}</h1>
              <p>{subtitle}</p>
            </div>

            <div className="stage">
              <div className="endpoint left">
                <div className="orb">
                  {mode === 'browser' ? <Globe size={40} strokeWidth={1.3} />
                    : mode === 'device' ? <Monitor size={40} strokeWidth={1.3} />
                    : apps.length ? (
                      <span className={`orb-apps n${Math.min(apps.length, 3)}`}>
                        {apps.slice(0, 3).map((a) => <AppIcon key={a.id} icon={a.icon} name={a.name} size={apps.length === 1 ? 54 : 36} />)}
                      </span>
                    )
                    : <LayoutGrid size={40} strokeWidth={1.3} />}
                  {mode === 'browser' && browsers.length > 1 && (
                    <button
                      className="chevron"
                      aria-label={t('left.chooseBrowser')}
                      aria-expanded={browserMenu}
                      onClick={() => setBrowserMenu(!browserMenu)}
                      disabled={locked}
                    >
                      <ChevronDown size={18} strokeWidth={1.8} />
                    </button>
                  )}
                </div>
                <div className="name">{mode === 'browser' ? (browser?.name ?? t('left.noBrowser')) : mode === 'device' ? t('left.device') : appsTitle}</div>
                <div className="meta">{mode === 'browser' ? t('left.browserMeta') : mode === 'device' ? t('left.deviceMeta') : appsMeta}</div>
                {browserMenu && (
                  <div className="menu" role="menu">
                    {browsers.map((b) => (
                      <button
                        key={b.id}
                        role="menuitemradio"
                        aria-checked={b.id === browser?.id}
                        onClick={() => {
                          setBrowserId(b.id);
                          setBrowserMenu(false);
                        }}
                      >
                        {b.name}
                      </button>
                    ))}
                  </div>
                )}
              </div>

              <NinjaCanvas
                phase={vpn.state}
                insetLeft={ORB_CENTER_X + ORB_RADIUS + CHEVRON_SPACE}
                insetRight={ORB_CENTER_X + ORB_RADIUS + 4}
                base={ORB_TOP + ORB_RADIUS}
              />

              <div className="endpoint right">
                <div className="orb">
                  <Place code={rightFlag} fallbackIcon={shownParsed?.groupIcon ?? '🌐'} label={rightFlag ? countryName(rightFlag) : t('right.world')} />
                  <button className="chevron" aria-label={t('right.chooseServer')} onClick={() => setPanelOpen(true)}>
                    <ChevronDown size={18} strokeWidth={1.8} />
                  </button>
                </div>
                <div className="name">{shownParsed?.title ?? t('right.server')}</div>
                <div className="meta">{rightMeta}</div>
                {fallbackNote && <div className="fallback-note">{fallbackNote}</div>}
              </div>
            </div>

            <div className="actions">
              <button className="primary" onClick={onPrimary} disabled={vpn.state === 'disconnecting' || (!canConnect && !busy && !connected)}>
                {primaryLabel}
              </button>
              <p className={`caption${failure ? ' error' : ''}`} aria-live="polite">
                {caption}
                {failure && failureRest.length > 0 && (
                  <button className="link" onClick={() => setShowDetails(!showDetails)}>
                    {showDetails ? t('cap.less') : t('cap.more')}
                  </button>
                )}
              </p>
              {showDetails && <pre className="details">{failureRest.join('\n')}</pre>}
              {appsNote && mode === 'apps' && <p className="caption apps-note">{appsNote}</p>}
              {mode === 'browser' && browser && connected && (browserRun === 'direct' || browserRun === 'stale') && (
                <>
                  <p className="caption apps-note">{t('browser.needRestart', { name: browser.name })}</p>
                  <button className="link" disabled={browserRestarting} onClick={() => restartBrowser(true)}>
                    {t(browserRestarting ? 'browser.restarting' : 'browser.restartVpn', { name: browser.name })}
                  </button>
                </>
              )}
              {mode === 'browser' && browser && connected && browserRun !== 'direct' && browserRun !== 'stale' && (
                <button className="link" onClick={() => backend.openBrowser(browser.id).then(setBrowserRun, (e) => setActionError(String(e)))}>
                  {t('browser.openAgain', { name: browser.name })}
                </button>
              )}
              {/* После «Отключить» браузер ещё настроен на наш прокси — без VPN сайты не откроет. */}
              {mode === 'browser' && browser && !connected && !busy && vpn.state !== 'disconnecting' && browserRun === 'stale' && (
                <>
                  <p className="caption apps-note">{t('browser.stillVpn', { name: browser.name })}</p>
                  <button className="link" disabled={browserRestarting} onClick={() => restartBrowser(false)}>
                    {t(browserRestarting ? 'browser.restarting' : 'browser.restartDirect', { name: browser.name })}
                  </button>
                </>
              )}
            </div>

            {mode === 'apps' && (
              <div className="split" role="radiogroup" aria-label={t('split.aria')}>
                <button role="radio" aria-checked={split === 'only'} disabled={locked} onClick={() => setSplit('only')}>
                  {t('split.only')}
                </button>
                <button role="radio" aria-checked={split === 'except'} disabled={locked} onClick={() => setSplit('except')}>
                  {t('split.except')}
                </button>
              </div>
            )}
            {tunMode && !locked && otherVpn && <p className="split-note warn">{t('split.otherVpn', { name: otherVpn })}</p>}
            {tunMode && !locked && !otherVpn && <p className="split-note">{t('split.tunNote')}</p>}
            {mode === 'apps' && (
              <AppsBar
                except={except}
                apps={apps}
                statuses={appStatuses}
                connected={!!connected}
                onAdd={() => setPickerOpen(true)}
                {...appActions}
              />
            )}

            <div className="modes" role="group" aria-label={t('modes.aria')}>
              {MODES.map((m) => (
                <button key={m} aria-pressed={mode === m} onClick={() => setMode(m)} disabled={locked && mode !== m}>
                  {t(`mode.${m}`)}
                </button>
              ))}
            </div>
          </section>
        ) : (
          <section className="content rules">
            <div className="card">
              <h2>{t('rules.reserve')}</h2>
              <p className="muted">{t('rules.reserveHint')}</p>
              <div className="choice" role="radiogroup" aria-label={t('rules.reserve')}>
                {RESERVE_OPTIONS.map((id) => (
                  <label key={id} className={reserve === id ? 'on' : ''}>
                    <input type="radio" name="reserve" checked={reserve === id} onChange={() => setReserve(id)} />
                    <span className="choice-label">{t(`reserve.${id}`)}</span>
                    <span className="choice-hint">{t(`reserve.${id}.hint`)}</span>
                  </label>
                ))}
              </div>
              <label className="toggle">
                <input type="checkbox" checked={autoRecover} onChange={(e) => setAutoRecover(e.target.checked)} />
                <span>
                  <b>{t('rules.recover')}</b> {t('rules.recoverHint')}
                </span>
              </label>
              {locked && <p className="muted small">{t('rules.nextTime')}</p>}
            </div>
            <div className="card">
              <h2>{t('rules.theme')}</h2>
              <p className="muted">{t('rules.themeHint')}</p>
              <div className="langs" role="radiogroup" aria-label={t('rules.theme')}>
                {THEMES.map((id) => (
                  <button key={id} role="radio" aria-checked={theme === id} onClick={() => setTheme(id)}>
                    {t(`theme.${id}`)}
                  </button>
                ))}
              </div>
            </div>
            <div className="card">
              <h2>{t('rules.language')}</h2>
              <p className="muted">{t('rules.languageHint')}</p>
              <div className="langs" role="radiogroup" aria-label={t('rules.language')}>
                {LANGS.map((l) => (
                  <button key={l.id} role="radio" aria-checked={lang === l.id} lang={l.locale} dir={l.dir} onClick={() => setLang(l.id)}>
                    {l.name}
                  </button>
                ))}
              </div>
            </div>
            <div className="card">
              <h2>{t('rules.apps')}</h2>
              <p>{t('rules.appsText')}</p>
              <p className="muted">{t('rules.appsNote')}</p>
            </div>
          </section>
        )}

        <nav className="bottom-nav" aria-label={t('nav.aria')}>
          <button aria-current={view === 'connection'} onClick={() => setView('connection')}>
            <Link2 size={22} strokeWidth={1.5} /> {t('nav.connection')}
          </button>
          <button aria-current={panelOpen} onClick={() => setPanelOpen(!panelOpen)}>
            <Layers size={22} strokeWidth={1.5} /> {t('nav.profiles')}
          </button>
          <button aria-current={view === 'rules'} onClick={() => setView('rules')}>
            <SlidersHorizontal size={22} strokeWidth={1.5} /> {t('nav.rules')}
          </button>
        </nav>
      </main>

      {pickerOpen && (
        <AppPicker apps={apps} load={backend.installedPrograms} pick={backend.pickProgram} onAdd={addApp} onClose={closePicker} />
      )}

      {panelOpen && (
        <ServerPanel
          sources={sources}
          error={sourcesError}
          selectedKey={server?.key ?? null}
          probes={probes}
          locked={locked}
          selectLocked={selectLocked}
          onSelect={selectServer}
          onClose={() => setPanelOpen(false)}
          {...sourceActions}
        />
      )}
    </div>
    </I18nProvider>
  );
}

/** Свои кнопки окна: стандартная рамка Windows отключена, чтобы стекло шло до краёв. */
function WindowControls() {
  const { t } = useI18n();
  return (
    <div className="window-controls">
      <button onClick={backend.minimize} aria-label={t('win.minimize')}>
        <Minus size={18} strokeWidth={1.4} />
      </button>
      <button onClick={backend.toggleMaximize} aria-label={t('win.maximize')}>
        <Square size={14} strokeWidth={1.4} />
      </button>
      <button className="close" onClick={backend.close} aria-label={t('win.close')}>
        <X size={18} strokeWidth={1.4} />
      </button>
    </div>
  );
}
