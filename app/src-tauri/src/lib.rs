//! Окно ninja-vpn: мост между интерфейсом (React) и мотором.
//!
//! Интерфейс вызывает команды (`#[tauri::command]`), а мотор сообщает этапы подключения
//! событием `vpn-state`. На эти события подписаны и надписи, и анимация ниндзя:
//! «Подключено» появляется только после настоящей проверки передачи данных.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use ninja_motor::apps::AppStore;
use ninja_motor::engine;
use ninja_motor::t;
use ninja_motor::tun::Tun;
use ninja_motor::subscription::Entry;
use ninja_motor::verify::mask_ip;
use ninja_motor::{
    ConnectOptions, Engines, Profile, Session, SourceInfo, SourceKind, SourceStore, State, browser, connect,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, RunEvent};

mod apps_mode;
use apps_mode::Apps;

/// Порт локального прокси, если он свободен.
const PREFERRED_PORT: u16 = 2080;

struct AppState {
    /// Где лежат ядра (`engines/`): папка проекта при разработке, папка программы после установки.
    root: PathBuf,
    /// Рабочие файлы ядра: конфиги (в них ключи), журналы, профили отдельного браузера.
    runtime: PathBuf,
    /// Источники серверов (подписки и ключи), зашифрованные DPAPI.
    store: SourceStore,
    /// Всё о подключении — под одним замком. Раньше это были четыре отдельных флажка,
    /// и «Отключить» в неудачный момент могло проиграть гонку со сторожем.
    conn: Mutex<Conn>,
    /// Режим «Приложения»: список программ, кого мы запустили, сторож.
    apps: Mutex<Apps>,
    app_store: AppStore,
    /// Страница «ключ с телефона» (QR-код), пока открыто её окно.
    pairing: Mutex<Option<ninja_motor::pair::Pairing>>,
}

/// Подключение сейчас: покой, идёт подключение или работает.
enum Link {
    Idle,
    /// Идёт подключение или переподключение; `attempt` — номер этой попытки.
    Busy { attempt: u64 },
    /// Подключено: работающее ядро и номер попытки, которая его запустила.
    Up { attempt: u64, session: Session },
}

struct Conn {
    link: Link,
    next_attempt: u64,
    /// Последнее отправленное событие — чтобы окно после перезагрузки знало, где мы.
    last: StateEvent,
    /// Порядок серверов для подключения и переподключения (выбранный + резерв).
    plan: Vec<String>,
    /// Сервер, который выбрал человек. Пока работаем на другом — это резерв, и окно так и пишет.
    selected: String,
    /// Переподключаться ли при обрыве.
    auto_recover: bool,
    /// Режим «все программы через VPN, кроме этих»: exe исключений. `None` — обычный режим.
    except: Option<Vec<PathBuf>>,
    /// Работающий TUN этого режима. Живёт и через переподключения ядра (порт тот же),
    /// чтобы Windows не спрашивала разрешение администратора на каждый обрыв.
    tun: Option<Tun>,
}

impl Conn {
    /// Начать новую попытку: у каждой свой номер. Старые попытки, увидев чужой номер,
    /// молча останавливают своё ядро и ничего не сообщают окну.
    fn start_attempt(&mut self) -> u64 {
        let attempt = self.next_attempt;
        self.next_attempt += 1;
        self.link = Link::Busy { attempt };
        attempt
    }

    /// Эта попытка всё ещё та, которую ждёт человек?
    fn is_current(&self, attempt: u64) -> bool {
        matches!(self.link, Link::Busy { attempt: a } if a == attempt)
    }
}

/// Замок подключения. Если какой-то поток упал, держа замок, данные внутри всё равно
/// целые (мы не оставляем их наполовину изменёнными) — продолжаем работать, а не падаем следом.
fn lock(state: &AppState) -> MutexGuard<'_, Conn> {
    state.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Событие для интерфейса. В JSON: `{ "state": "connected", "exitCountry": "PL", … }`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum StateEvent {
    Disconnected,
    Preparing,
    Connecting,
    Verifying,
    Connected {
        /// Ключ сервера, который на самом деле подключился.
        key: String,
        server: String,
        /// Если подключились через резерв — имя сервера, который выбирали.
        fallback_from: Option<String>,
        exit_country: Option<String>,
        exit_ip: Option<String>,
        first_ms: u64,
        bulk_ms: u64,
        port: u16,
    },
    Failed {
        message: String,
    },
    /// Канал пропал во время работы — переподключаемся.
    Reconnecting {
        reason: String,
    },
    Disconnecting,
}

impl From<&State> for StateEvent {
    fn from(state: &State) -> Self {
        match state {
            State::Disconnected => Self::Disconnected,
            State::Preparing => Self::Preparing,
            State::Connecting => Self::Connecting,
            State::Verifying => Self::Verifying,
            // Подробности «подключено» отправляем сами, после того как мотор вернул отчёт.
            State::Connected => Self::Verifying,
            State::Failed(message) => Self::Failed { message: message.clone() },
            State::Disconnecting => Self::Disconnecting,
        }
    }
}

/// Сообщить окну новое состояние. Вызывается только под замком подключения, поэтому
/// события из разных потоков не перемешаются и окно не покажет устаревшее состояние.
fn emit(app: &AppHandle, conn: &mut Conn, event: StateEvent) {
    conn.last = event.clone();
    let _ = app.emit("vpn-state", event);
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ServerView {
    /// `<id источника>/<имя сервера>`: номера в подписке меняются после обновления, имена — нет,
    /// а приставка источника не даёт спутать одинаковые имена у разных сервисов.
    key: String,
    name: String,
    summary: String,
    engine: String,
    problem: Option<String>,
    warnings: Vec<String>,
}

/// Источник для окна: сведения + его серверы. Секретов здесь нет.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SourceView {
    id: String,
    name: String,
    kind: SourceKind,
    title: Option<String>,
    hint: String,
    used_bytes: Option<u64>,
    total_bytes: Option<u64>,
    expires: Option<String>,
    updated: Option<u64>,
    servers: Vec<ServerView>,
    /// Не удалось прочитать сохранённый список серверов.
    problem: Option<String>,
}

#[derive(Serialize)]
struct BrowserView {
    id: &'static str,
    name: &'static str,
}

/// При разработке — `runtime/` в папке проекта (её видит и командная строка).
/// В установленном приложении — `%LOCALAPPDATA%\com.ninjavpn.app\runtime`: в папку программы
/// писать не стоит, а конфигам с ключами место в личной папке пользователя.
fn runtime_dir(root: &Path) -> PathBuf {
    if root.join("Cargo.toml").exists() {
        return root.join("runtime");
    }
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("com.ninjavpn.app")
        .join("runtime")
}

fn server_view(key: String, entry: Entry) -> ServerView {
    match entry.result {
        Ok(p) => ServerView { key, name: entry.label, summary: p.summary(), engine: p.engine().label(), problem: None, warnings: p.warnings },
        Err(e) => ServerView { key, name: entry.label, summary: String::new(), engine: String::new(), problem: Some(e.to_string()), warnings: vec![] },
    }
}

fn source_view(store: &SourceStore, info: SourceInfo) -> SourceView {
    let (servers, problem) = match store.servers(&info.id) {
        Ok(list) => (list.into_iter().map(|(k, e)| server_view(k, e)).collect(), None),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    let usage = info.usage.as_ref();
    SourceView {
        used_bytes: usage.map(|u| u.upload + u.download),
        total_bytes: usage.map(|u| u.total).filter(|t| *t > 0),
        expires: usage.and_then(|u| u.expire_date()),
        id: info.id,
        name: info.name,
        kind: info.kind,
        title: info.title,
        hint: info.hint,
        updated: info.updated,
        servers,
        problem,
    }
}

#[tauri::command]
fn list_sources(state: tauri::State<AppState>) -> Result<Vec<SourceView>, String> {
    state.store.migrate_env(&state.root);
    let list = state.store.list().map_err(|e| e.to_string())?;
    Ok(list.into_iter().map(|info| source_view(&state.store, info)).collect())
}

/// Добавить подписку или ключ. Проверка ссылки ходит в сеть — делаем это в отдельном потоке.
#[tauri::command]
async fn add_source(name: Option<String>, link: String) -> Result<SourceView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let store = SourceStore::open(SourceStore::default_dir());
        let info = store.add(name.as_deref(), &link).map_err(|e| e.to_string())?;
        Ok(source_view(&store, info))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Страница для телефона: адрес и QR-код.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PairView {
    url: String,
    qr_svg: String,
}

/// Открыть страницу «ключ с телефона» в домашней сети. Каждый присланный ключ или подписка
/// добавляется как обычный источник, а окно узнаёт о нём событием `pair-added`.
#[tauri::command]
fn pair_start(app: AppHandle, state: tauri::State<AppState>) -> Result<PairView, String> {
    let mut slot = state.pairing.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(old) = slot.as_mut() {
        old.stop();
    }
    let events = app.clone();
    let pairing = ninja_motor::pair::Pairing::start(Box::new(move |link: &str| {
        let store = SourceStore::open(SourceStore::default_dir());
        let info = store.add(None, link).map_err(|e| e.to_string())?;
        let name = info.name.clone();
        let _ = events.emit("pair-added", source_view(&store, info));
        Ok(name)
    }))?;
    let view = PairView { url: pairing.url.clone(), qr_svg: pairing.qr_svg.clone() };
    *slot = Some(pairing);
    Ok(view)
}

/// Закрыть страницу для телефона (окно с QR-кодом закрыли).
#[tauri::command]
fn pair_stop(state: tauri::State<AppState>) {
    let old = state.pairing.lock().unwrap_or_else(|p| p.into_inner()).take();
    drop(old); // остановка — без замка: поток страницы заканчивает текущий запрос
}

#[tauri::command]
async fn refresh_source(id: String) -> Result<SourceView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let store = SourceStore::open(SourceStore::default_dir());
        let info = store.refresh(&id).map_err(|e| e.to_string())?;
        Ok(source_view(&store, info))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn rename_source(state: tauri::State<AppState>, id: String, name: String) -> Result<SourceView, String> {
    let info = state.store.rename(&id, &name).map_err(|e| e.to_string())?;
    Ok(source_view(&state.store, info))
}

#[tauri::command]
fn remove_source(state: tauri::State<AppState>, id: String) -> Result<(), String> {
    state.store.remove(&id).map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeView {
    key: String,
    /// Задержка через сервер, мс; `None` — не ответил.
    ms: Option<u64>,
    error: Option<String>,
}

/// Проверить скорость всех подходящих серверов источника (одновременно, ~10 с).
#[tauri::command]
async fn probe_source(state: tauri::State<'_, AppState>, id: String) -> Result<Vec<ProbeView>, String> {
    let (root, runtime) = (state.root.clone(), state.runtime.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let store = SourceStore::open(SourceStore::default_dir());
        let items: Vec<(String, Profile)> = store
            .servers(&id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter_map(|(k, e)| e.result.ok().map(|p| (k, p)))
            .collect();
        let engines = Engines::load(&root)?;
        let results = ninja_motor::probe::probe_all(items, &engines, &runtime);
        Ok(results
            .into_iter()
            .map(|r| ProbeView { key: r.key, ms: r.latency.map(|d| d.as_millis() as u64), error: r.error })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn browsers() -> Vec<BrowserView> {
    browser::installed().into_iter().map(|b| BrowserView { id: b.id, name: b.name }).collect()
}

#[tauri::command]
fn status(state: tauri::State<AppState>) -> StateEvent {
    lock(&state).last.clone()
}

/// Язык сообщений мотора (ошибки, предупреждения о серверах): ru, en, es, pt, tr, zh, fa.
///
/// Последняя ошибка подключения уже составлена на прежнем языке (в ней бывают и куски от ядра
/// и сервера — заново не перевести). Сменили язык — убираем её, как при выборе другого сервера:
/// она про прошлую попытку, а нужное предупреждение (другой VPN и т. п.) окно покажет на новом.
#[tauri::command]
fn set_language(app: AppHandle, state: tauri::State<AppState>, lang: String) -> Result<(), String> {
    let changed = ninja_motor::i18n::lang() != lang;
    if !ninja_motor::i18n::set_lang(&lang) {
        return Err(t!("app.bad_language", lang = lang));
    }
    let mut conn = lock(&state);
    if changed && matches!(conn.link, Link::Idle) && matches!(conn.last, StateEvent::Failed { .. }) {
        emit(&app, &mut conn, StateEvent::Disconnected);
    }
    Ok(())
}

/// Включён ли другой VPN на весь компьютер (имя его сетевой карты): окно предупреждает
/// ещё до «Подключить», что режимам с TUN он помешает.
#[tauri::command]
fn other_vpn() -> Option<String> {
    ninja_motor::tun::other_vpn()
}

/// Сколько серверов пробуем за одно подключение (выбранный + резервные).
const MAX_ATTEMPTS: usize = 4;
/// Как часто сторож проверяет, что данные идут (в тактах по 2 с): 10 × 2 = 20 с.
const HEALTH_EVERY_TICKS: u32 = 10;
/// Сколько неудачных проверок подряд считаем обрывом.
const HEALTH_MISSES: u32 = 2;

/// «Сервер не ответил — пробую следующий»: для подписи под кнопкой.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AttemptEvent {
    attempt: usize,
    total: usize,
    server: String,
    /// Ядро этого сервера: «Xray 26.3.27» — для подписи «Запускаю ядро …».
    engine: String,
    previous_server: Option<String>,
    previous_reason: Option<String>,
}

/// Подключиться. `keys` — выбранный сервер и разрешённые резервные (порядок задаёт окно:
/// сначала самые быстрые по последней проверке). `auto_recover` — переподключаться при обрыве.
#[tauri::command]
fn connect_vpn(
    app: AppHandle,
    state: tauri::State<AppState>,
    keys: Vec<String>,
    auto_recover: bool,
    except: Option<Vec<String>>,
) -> Result<(), String> {
    if keys.is_empty() {
        return Err(t!("store.no_server_selected"));
    }
    // Исключения — программы из списка режима «Приложения»: их exe на сегодня
    // (у Discord и приложений из Microsoft Store путь меняется с обновлениями).
    let except = except.map(|ids| {
        let apps = apps_mode::apps_lock(&state);
        apps.list.iter().filter(|a| ids.contains(&a.id)).filter_map(ninja_motor::apps::resolve_app).map(|f| f.exe).collect()
    });
    // Весь компьютер через VPN (TUN) — только если другой VPN на весь компьютер выключен.
    if except.is_some()
        && let Some(name) = ninja_motor::tun::other_vpn()
    {
        return Err(ninja_motor::tun::other_vpn_message(&name));
    }
    let attempt = {
        let mut conn = lock(&state);
        match conn.link {
            Link::Idle => {}
            Link::Busy { .. } => return Err(t!("app.already_connecting")),
            Link::Up { .. } => return Err(t!("app.already_connected")),
        }
        conn.plan = keys.clone();
        conn.selected = keys[0].clone();
        conn.auto_recover = auto_recover;
        conn.except = except;
        conn.start_attempt()
    };
    spawn_chain(app, attempt, keys, None, None);
    Ok(())
}

/// Переключиться на другой сервер, не отключаясь: как переподключение при обрыве — новое ядро
/// встаёт на тот же порт, TUN режима «Устройство» остаётся (Windows не спросит разрешение
/// заново), а браузер и программы, настроенные на этот порт, перезапускать не нужно.
/// Пока новое ядро не готово, трафик никуда не идёт — мимо VPN он не утечёт.
#[tauri::command]
fn switch_server(app: AppHandle, state: tauri::State<AppState>, keys: Vec<String>) -> Result<(), String> {
    if keys.is_empty() {
        return Err(t!("store.no_server_selected"));
    }
    let mut conn = lock(&state);
    let port = match &conn.link {
        Link::Up { session, .. } => session.listen_port,
        Link::Busy { .. } => return Err(t!("app.already_connecting")),
        Link::Idle => return Err(t!("app.connect_first")),
    };
    // Старое ядро останавливается здесь (drop) и освобождает порт для нового.
    conn.link = Link::Idle;
    conn.plan = keys.clone();
    conn.selected = keys[0].clone();
    let attempt = conn.start_attempt();
    drop(conn);
    spawn_chain(app, attempt, keys, Some(port), None);
    Ok(())
}

/// Цепочку подключения запускаем в своём потоке. Если в ней что-то сломается (паника),
/// подключение не должно навсегда застрять в «Подключение…»: возвращаемся в покой с ошибкой.
fn spawn_chain(app: AppHandle, attempt: u64, keys: Vec<String>, port: Option<u16>, recovering: Option<String>) {
    std::thread::spawn(move || {
        let run = std::panic::AssertUnwindSafe(|| connect_chain(&app, attempt, &keys, port, recovering.as_deref()));
        if std::panic::catch_unwind(run).is_err() {
            let state = app.state::<AppState>();
            let mut conn = lock(&state);
            if conn.is_current(attempt) {
                conn.link = Link::Idle;
                emit(&app, &mut conn, StateEvent::Failed { message: t!("app.internal") });
            }
        }
    });
}

/// Пробуем серверы по очереди, пока один не пройдёт настоящую проверку передачи.
/// Промежуточные неудачи — не «ошибка»: ниндзя продолжает искать путь.
/// `recovering` — причина обрыва, если это переподключение: тогда до самого конца
/// окно видит «Переподключение…», а не обычное «Подключение…» (иначе обрыв незаметен).
///
/// Всё, что цепочка сообщает окну, она сообщает только пока её попытка текущая.
/// Если человек нажал «Отключить», попытка устарела: цепочка молча доделывает шаг
/// и останавливает своё ядро, ничего не показывая.
fn connect_chain(app: &AppHandle, attempt: u64, keys: &[String], port: Option<u16>, recovering: Option<&str>) {
    let state = app.state::<AppState>();
    // Сообщить окну — только если попытка ещё текущая.
    let say = |event: StateEvent| {
        let mut conn = lock(&state);
        if conn.is_current(attempt) {
            emit(app, &mut conn, event);
        }
    };
    let engines = match Engines::load(&state.root) {
        Ok(e) => e,
        Err(e) => return give_up(app, attempt, e),
    };
    let listen_port = port.unwrap_or_else(free_port);
    let total = keys.len().min(MAX_ATTEMPTS);
    let mut failures: Vec<(String, String)> = Vec::new();

    for (i, key) in keys.iter().take(MAX_ATTEMPTS).enumerate() {
        if !lock(&state).is_current(attempt) {
            return; // человек отключился, пока мы пробовали предыдущий сервер
        }
        let profile = match state.store.find(key) {
            Ok(p) => p,
            Err(e) => {
                failures.push((server_name(key).to_string(), e));
                continue;
            }
        };
        let previous = failures.last().cloned();
        let info = AttemptEvent {
            attempt: i + 1,
            total,
            server: profile.name.clone(),
            engine: profile.engine().label(),
            previous_server: previous.as_ref().map(|(name, _)| name.clone()),
            previous_reason: previous.map(|(_, reason)| first_line(&reason)),
        };
        if lock(&state).is_current(attempt) {
            let _ = app.emit("vpn-attempt", info);
        }
        let options = ConnectOptions { engines: engines.clone(), runtime_dir: state.runtime.clone(), listen_port };
        // Итоговую ошибку сообщим сами — после всех попыток.
        let result = connect(&profile, &options, |s| match (s, recovering) {
            (State::Failed(_), _) => {}
            (_, Some(reason)) => say(StateEvent::Reconnecting { reason: reason.to_string() }),
            (_, None) => say(s.into()),
        });
        let session = match result {
            Ok(session) => session,
            Err(e) => {
                failures.push((profile.name.clone(), e.to_string()));
                continue;
            }
        };
        // Режим «все через VPN, кроме этих»: поверх ядра — TUN (один раз, не на каждое переподключение).
        let need_tun = {
            let conn = lock(&state);
            conn.except.clone().filter(|_| conn.tun.is_none() && conn.is_current(attempt))
        };
        if let Some(except) = need_tun {
            // Пока подключались, могли включить другой VPN — тогда TUN не поднимаем.
            if let Some(name) = ninja_motor::tun::other_vpn() {
                drop(session);
                return give_up(app, attempt, ninja_motor::tun::other_vpn_message(&name));
            }
            let _ = app.emit("vpn-elevation", true);
            // Окно — вперёд: запрос Windows появится поверх него, а не замигает в панели задач.
            let window = app.get_webview_window("main");
            let owner = window.as_ref().and_then(|w| {
                let _ = w.unminimize();
                let _ = w.set_focus();
                w.hwnd().ok()
            });
            let owner = owner.map_or(0, |h| h.0 as isize);
            let started = start_tun(&state, &engines, session.listen_port, except, owner);
            let _ = app.emit("vpn-elevation", false);
            match started {
                Ok(tun) => {
                    let mut conn = lock(&state);
                    if !conn.is_current(attempt) {
                        return; // отключились, пока ждали разрешения: TUN и ядро остановятся сами (drop)
                    }
                    conn.tun = Some(tun);
                }
                Err(e) => {
                    drop(session);
                    return give_up(app, attempt, t!("app.tun_failed", why = e));
                }
            }
        }
        let mut conn = lock(&state);
        if !conn.is_current(attempt) {
            drop(conn);
            drop(session); // отключились, пока шла проверка: ядро останавливается, окно уже знает
            return;
        }
        let report = &session.report;
        // Резерв — если работаем не на том сервере, который выбрал человек
        // (в том числе после переподключения, когда первым в плане стоит уже резервный).
        let event = StateEvent::Connected {
            key: key.clone(),
            server: profile.name.clone(),
            fallback_from: (*key != conn.selected).then(|| server_name(&conn.selected).to_string()),
            exit_country: report.exit.as_ref().map(|e| e.country.clone()),
            exit_ip: report.exit.as_ref().map(|e| mask_ip(&e.ip)),
            first_ms: report.first_response.as_millis() as u64,
            bulk_ms: report.bulk_time.as_millis() as u64,
            port: session.listen_port,
        };
        // Для переподключения первым пробуем сервер, который сейчас сработал.
        let mut plan = vec![key.clone()];
        plan.extend(keys.iter().filter(|k| *k != key).cloned());
        conn.plan = plan;
        conn.link = Link::Up { attempt, session };
        emit(app, &mut conn, event);
        drop(conn);
        watch(app.clone(), attempt, listen_port);
        return;
    }

    let message = match failures.as_slice() {
        [] => t!("app.no_servers"),
        [(_, only)] => only.clone(),
        many => t!(
            "app.all_failed",
            n = many.len(),
            details = many.iter().map(|(name, reason)| format!("{name}: {}", first_line(reason))).collect::<Vec<_>>().join("\n")
        ),
    };
    give_up(app, attempt, message);
}

/// Попытка закончилась неудачей: в покой и честная ошибка (если попытка ещё текущая).
fn give_up(app: &AppHandle, attempt: u64, message: String) {
    let state = app.state::<AppState>();
    let mut conn = lock(&state);
    if conn.is_current(attempt) {
        conn.link = Link::Idle;
        let tun = conn.tun.take();
        emit(app, &mut conn, StateEvent::Failed { message });
        drop(conn);
        drop(tun); // TUN гасим уже без замка: это до пары секунд
    }
}

/// Запустить TUN режима «все через VPN, кроме этих» поверх работающего ядра на `port`.
/// Windows спросит разрешение администратора; пока человек думает, ждём до двух минут.
fn start_tun(state: &AppState, engines: &Engines, port: u16, except: Vec<PathBuf>, owner: isize) -> Result<Tun, String> {
    use ninja_motor::EngineKind;
    use ninja_motor::tun::{self, Split, TunOptions};
    // Имена всех серверов плана (выбранный и резервные): их адреса — напрямую у провайдера.
    let plan = lock(state).plan.clone();
    let mut server_names: Vec<String> = plan
        .iter()
        .filter_map(|key| state.store.find(key).ok())
        .map(|p| p.server)
        .filter(|host| host.parse::<std::net::IpAddr>().is_err())
        .collect();
    server_names.sort();
    server_names.dedup();
    let options = TunOptions {
        servers: tun::resolve_servers(&server_names),
        split: Split::Except(except),
        proxy_port: port,
        // Наши ядра — всегда напрямую: иначе их трафик к VPN-серверу вернулся бы в TUN по кругу.
        bypass: vec![engines.path(EngineKind::SingBox).to_path_buf(), engines.path(EngineKind::Xray).to_path_buf()],
        // Только для проверки при разработке: забирать в TUN лишь эти адреса (через запятую).
        only_routes: std::env::var("NINJA_TUN_TEST_ROUTES")
            .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default(),
        // Для поиска неполадок: NINJA_TUN_DEBUG=1 — подробный журнал в runtime\tun\sing-box.log.
        verbose: std::env::var_os("NINJA_TUN_DEBUG").is_some(),
    };
    let config = tun::config(&options);
    // Сначала проверяем конфиг без прав администратора: ошибка — без лишнего окна Windows.
    let file = engine::ConfigFile::write(&state.runtime, "tun-check", &config).map_err(|e| e.to_string())?;
    engine::check_config(EngineKind::SingBox, engines.path(EngineKind::SingBox), file.path()).map_err(|e| e.to_string())?;
    drop(file);
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let args = format!("{HELPER_FLAG} {} {}", std::process::id(), ninja_motor::i18n::lang());
    Tun::start(&me, &args, &state.runtime.join("tun"), &config, Duration::from_secs(120), owner)
}

/// Ключ запуска помощника TUN (он работает с правами администратора, без окна).
const HELPER_FLAG: &str = "--tun-helper";

/// Если программу запустили помощником TUN — сделать его работу и вернуть код выхода.
/// Вызывается из `main` до запуска окна (и до «только одна копия программы»).
pub fn helper_mode() -> Option<i32> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some(HELPER_FLAG) {
        return None;
    }
    let parent: u32 = args.get(2).and_then(|p| p.parse().ok())?;
    if let Some(lang) = args.get(3) {
        ninja_motor::i18n::set_lang(lang);
    }
    let root = project_root();
    let engines = match Engines::load(&root) {
        Ok(e) => e,
        Err(_) => return Some(1),
    };
    let dir = runtime_dir(&root).join("tun");
    Some(ninja_motor::tun::helper_main(engines.path(ninja_motor::EngineKind::SingBox), &dir, parent))
}

/// Имя сервера из ключа `<источник>/<имя>`.
fn server_name(key: &str) -> &str {
    key.split_once('/').map_or(key, |(_, name)| name)
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().to_string()
}

/// «Отключить» срабатывает сразу, в любой момент: во время подключения, переподключения или работы.
#[tauri::command]
fn disconnect_vpn(app: AppHandle, state: tauri::State<AppState>) {
    let mut conn = lock(&state);
    // TUN — первым: трафик компьютера сразу возвращается на обычный путь.
    if let Some(mut tun) = conn.tun.take() {
        emit(&app, &mut conn, StateEvent::Disconnecting);
        tun.stop();
    }
    match std::mem::replace(&mut conn.link, Link::Idle) {
        // Работающее ядро останавливаем здесь же (это доли секунды).
        Link::Up { session, .. } => {
            emit(&app, &mut conn, StateEvent::Disconnecting);
            session.disconnect(|_| {});
        }
        // Подключение ещё идёт: его попытка устарела. Цепочка увидит это, остановит своё ядро
        // и ничего не покажет, а окно сразу свободно.
        Link::Busy { .. } | Link::Idle => {}
    }
    emit(&app, &mut conn, StateEvent::Disconnected);
}

/// Порт работающего подключения (`None` — VPN выключен или ещё подключается).
fn current_port(state: &AppState) -> Option<u16> {
    match &lock(state).link {
        Link::Up { session, .. } => Some(session.listen_port),
        _ => None,
    }
}

fn find_browser(id: &str) -> Result<browser::Browser, String> {
    browser::installed().into_iter().find(|b| b.id == id).ok_or_else(|| t!("app.browser_missing"))
}

/// Как сейчас запущен браузер — для окна: `off` (закрыт), `vpn` (через это подключение),
/// `stale` (настроен на наш прокси, но VPN выключен или на другом порту — сайты не откроет),
/// `direct` (открыт сам по себе и ходит напрямую).
fn browser_run(state: &AppState, found: &browser::Browser) -> &'static str {
    match browser::running(found) {
        browser::Running::No => "off",
        browser::Running::Proxy(port) if Some(port) == current_port(state) => "vpn",
        browser::Running::Proxy(_) => "stale",
        browser::Running::Direct => "direct",
    }
}

/// Как сейчас запущен браузер (см. [`browser_run`]). Окно спрашивает раз в пару секунд.
#[tauri::command]
async fn browser_status(app: AppHandle, id: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let found = find_browser(&id)?;
        Ok(browser_run(&app.state::<AppState>(), &found).to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Открыть твой обычный браузер через VPN. Уже открытый напрямую не трогаем без спроса:
/// возвращаем `direct` / `stale`, и окно предлагает перезапустить его (вкладки вернутся).
#[tauri::command]
async fn open_browser(app: AppHandle, id: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let port = current_port(&state).ok_or_else(|| t!("app.connect_first"))?;
        let found = find_browser(&id)?;
        match browser_run(&state, &found) {
            // Закрыт — запускаем; уже через VPN — новый запуск откроет ещё одно его окно.
            "off" | "vpn" => {
                browser::launch(&found, Some(port), false).map_err(|e| t!("app.browser_failed", name = found.name, why = e))?;
                Ok("vpn".to_string())
            }
            other => Ok(other.to_string()),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Перезапустить браузер: через VPN (`vpn`) или обратно напрямую, после отключения.
#[tauri::command]
async fn restart_browser(app: AppHandle, id: String, vpn: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let port = if vpn { Some(current_port(&state).ok_or_else(|| t!("app.connect_first"))?) } else { None };
        let found = find_browser(&id)?;
        browser::restart(&found, port).map_err(|e| match e {
            browser::RestartError::DidNotClose => t!("app.browser_did_not_close", name = found.name),
            browser::RestartError::Launch(why) => t!("app.browser_failed", name = found.name, why = why),
            browser::RestartError::DidNotStart => t!("app.browser_did_not_start", name = found.name),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn free_port() -> u16 {
    if TcpListener::bind(("127.0.0.1", PREFERRED_PORT)).is_ok() {
        return PREFERRED_PORT;
    }
    TcpListener::bind(("127.0.0.1", 0)).and_then(|l| l.local_addr()).map(|a| a.port()).unwrap_or(PREFERRED_PORT)
}

/// Сторож подключения: раз в 2 с — живо ли ядро, раз в 20 с — идут ли данные.
/// При обрыве не делаем вид, что всё хорошо: либо переподключаемся, либо честно сообщаем.
fn watch(app: AppHandle, attempt: u64, port: u16) {
    std::thread::spawn(move || {
        let mut tick = 0u32;
        let mut misses = 0u32;
        loop {
            std::thread::sleep(Duration::from_secs(2));
            tick += 1;
            let state = app.state::<AppState>();
            let (engine_alive, tun_on, tun_problem) = {
                let mut conn = lock(&state);
                let tun_on = conn.tun.is_some();
                let tun_problem = conn.tun.as_ref().filter(|t| !t.is_running()).map(|t| t.problem().unwrap_or_default());
                match &mut conn.link {
                    Link::Up { attempt: a, session } if *a == attempt => (session.is_alive(), tun_on, tun_problem),
                    _ => return, // отключились или подключились заново — у нового подключения свой сторож
                }
            };
            // TUN остановился сам — переподключением ядра это не лечится: честная ошибка.
            if let Some(problem) = tun_problem {
                let detail = if problem.is_empty() { String::new() } else { format!("\n{problem}") };
                return tun_lost(&app, attempt, t!("app.tun_stopped", detail = detail));
            }
            // Пока работаем на весь компьютер, включили другой VPN: два TUN оставят компьютер
            // без интернета, и переподключение не поможет. Уступаем ему и говорим почему.
            if tun_on && let Some(name) = ninja_motor::tun::other_vpn() {
                return tun_lost(&app, attempt, t!("tun.other_vpn_started", name = name));
            }
            let reason = if !engine_alive {
                Some(t!("app.reason_core_stopped"))
            } else if tick.is_multiple_of(HEALTH_EVERY_TICKS) {
                // Проверка идёт без замка (до ~17 с), чтобы «Отключить» не ждало её.
                // Внутри — повтор через 5 с: случайная неудача у балансировщика не считается промахом.
                misses = if ninja_motor::verify::health_confirmed(port) { 0 } else { misses + 1 };
                (misses >= HEALTH_MISSES).then(|| t!("app.reason_no_data"))
            } else {
                None
            };
            let Some(reason) = reason else { continue };
            return recover(&app, attempt, port, &reason);
        }
    });
}

/// Обрыв во время работы. Под одним замком: убеждаемся, что это всё то же подключение,
/// останавливаем ядро и сразу переходим в «Переподключение…» — нажатие «Отключить»
/// в этот момент не может потеряться. Если автовосстановление выключено — честная ошибка.
fn recover(app: &AppHandle, attempt: u64, port: u16, reason: &str) {
    let state = app.state::<AppState>();
    let mut conn = lock(&state);
    if !matches!(&conn.link, Link::Up { attempt: a, .. } if *a == attempt) {
        return; // пока сторож проверял, человек отключился или подключился заново
    }
    // Старое ядро останавливается здесь (drop), порт освобождается для нового.
    // TUN остаётся: новое ядро встанет на тот же порт, и Windows не спросит разрешение заново.
    conn.link = Link::Idle;
    if !conn.auto_recover {
        let tun = conn.tun.take();
        emit(app, &mut conn, StateEvent::Failed { message: t!("app.connection_lost", reason = reason) });
        drop(conn);
        drop(tun);
        return;
    }
    let next = conn.start_attempt();
    let keys = conn.plan.clone();
    emit(app, &mut conn, StateEvent::Reconnecting { reason: reason.to_string() });
    drop(conn);
    connect_chain(app, next, &keys, Some(port), Some(reason));
}

/// Режим «весь компьютер через VPN» дальше работать не может (TUN остановился сам или
/// включился другой VPN): гасим всё и честно говорим, что случилось.
fn tun_lost(app: &AppHandle, attempt: u64, message: String) {
    let state = app.state::<AppState>();
    let mut conn = lock(&state);
    if !matches!(&conn.link, Link::Up { attempt: a, .. } if *a == attempt) {
        return;
    }
    let tun = conn.tun.take();
    conn.link = Link::Idle;
    emit(app, &mut conn, StateEvent::Failed { message });
    drop(conn);
    drop(tun);
}

/// Папка с ядрами — та, где лежит engines/engines.json.
/// Выпускная сборка ищет сначала рядом с exe: после установки ядра лежат в папке программы.
/// Сборка для разработки — сначала в папке проекта: Tauri копирует ядра и в target\debug,
/// но рабочие файлы удобнее держать в проекте, где их видит и командная строка.
fn project_root() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let near_exe = exe.ancestors();
    let in_sources = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors();
    let order: Vec<&Path> =
        if cfg!(debug_assertions) { in_sources.chain(near_exe).collect() } else { near_exe.chain(in_sources).collect() };
    order
        .into_iter()
        .find(|dir| dir.join("engines").join("engines.json").exists())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let root = project_root();
    let app_store = AppStore::open(&SourceStore::default_dir());
    let app = tauri::Builder::default()
        // Только одна копия программы: два окна с двумя ядрами спорили бы за порт и подключение.
        // Повторный запуск (второй щелчок по ярлыку) просто показывает уже открытое окно.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .manage(AppState {
            runtime: runtime_dir(&root),
            root,
            store: SourceStore::open(SourceStore::default_dir()),
            conn: Mutex::new(Conn {
                link: Link::Idle,
                next_attempt: 1,
                last: StateEvent::Disconnected,
                plan: Vec::new(),
                selected: String::new(),
                auto_recover: true,
                except: None,
                tun: None,
            }),
            apps: Mutex::new(Apps::new(app_store.load())),
            app_store,
            pairing: Mutex::new(None),
        })
        .setup(|app| {
            apps_mode::spawn_guard(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_sources,
            add_source,
            refresh_source,
            rename_source,
            remove_source,
            probe_source,
            browsers,
            status,
            other_vpn,
            set_language,
            connect_vpn,
            switch_server,
            disconnect_vpn,
            open_browser,
            pair_start,
            pair_stop,
            browser_status,
            restart_browser,
            apps_mode::list_apps,
            apps_mode::installed_programs,
            apps_mode::pick_program,
            apps_mode::add_app,
            apps_mode::remove_app,
            apps_mode::set_app_autostart,
            apps_mode::set_apps_guard,
            apps_mode::apps_status,
            apps_mode::launch_apps,
            apps_mode::restart_app,
            apps_mode::close_app
        ])
        .build(tauri::generate_context!())
        .expect("не удалось запустить окно ninja-vpn");

    app.run(|app, event| {
        // Закрыли окно — останавливаем ядро. (Даже если не успеем, его остановит задание Windows.)
        if let RunEvent::Exit = event {
            let state = app.state::<AppState>();
            let mut conn = lock(&state);
            conn.link = Link::Idle; // ядро останавливается вместе с сеансом
            let tun = conn.tun.take();
            drop(conn);
            drop(tun); // и TUN (помощник погасил бы его и сам, увидев, что программа закрылась)
        }
    });
}
