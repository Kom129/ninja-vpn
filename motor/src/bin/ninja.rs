//! Командная строка мотора: те же источники и серверы, что в окне, только без окна.
//!
//! Подписки и ключи лежат в общем зашифрованном хранилище `%APPDATA%\com.ninjavpn.app`:
//! что добавил в окне — видно здесь, и наоборот.
//!
//!   ninja sources                        источники (без самих ссылок)
//!   ninja add [--name Имя]               добавить подписку или ключ: ссылку вставить и нажать Enter
//!   ninja refresh [источник]             скачать свежие списки серверов
//!   ninja rename <источник> <имя>        переименовать источник
//!   ninja remove <источник> [--yes]      удалить источник
//!   ninja list [источник]                серверы с номерами
//!   ninja probe [источник]               проверить скорость серверов
//!   ninja check <сервер>                 собрать конфиг и проверить его ядром
//!   ninja connect [сервер] [--browser]   подключиться через локальный прокси 127.0.0.1
//!   ninja programs                       программы из меню «Пуск» для режима «Приложения»
//!
//! <источник> — номер из `sources` или часть названия; <сервер> — номер из `list` или часть имени.

use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ninja_motor::subscription::{Entry, Usage};
use ninja_motor::verify::mask_ip;
use ninja_motor::{
    ConnectOptions, EngineKind, Engines, Profile, SourceInfo, SourceKind, SourceStore, State, browser, connect, engine,
    singbox, verify, xray,
};

/// Порт локального прокси, если он свободен. Постоянный порт удобнее настраивать в браузере.
const PREFERRED_PORT: u16 = 2080;

type CliResult = Result<(), String>;

/// Разобранная командная строка: слова по порядку и флаги отдельно.
struct Args {
    words: Vec<String>,
    browser: bool,
    yes: bool,
    name: Option<String>,
}

impl Args {
    fn parse(raw: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut args = Args { words: Vec::new(), browser: false, yes: false, name: None };
        let mut raw = raw.peekable();
        while let Some(arg) = raw.next() {
            match arg.as_str() {
                "--browser" => args.browser = true,
                "--yes" | "-y" => args.yes = true,
                "--name" => args.name = Some(raw.next().ok_or("после --name нужно название")?),
                flag if flag.starts_with("--") => return Err(format!("непонятный флаг {flag}")),
                _ => args.words.push(arg),
            }
        }
        Ok(args)
    }

    /// Слово после команды: `connect нидерланды` → «нидерланды». Несколько слов склеиваем:
    /// `connect Germany 92` ищет «Germany 92», кавычки не нужны.
    fn rest(&self, from: usize) -> Option<String> {
        let rest = self.words.get(from..).unwrap_or_default();
        (!rest.is_empty()).then(|| rest.join(" "))
    }
}

fn main() -> ExitCode {
    let result = Args::parse(std::env::args().skip(1)).and_then(run);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("\nОшибка: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> CliResult {
    let command = args.words.first().map(String::as_str).unwrap_or("help");
    if matches!(command, "help" | "-h" | "/?") {
        print_help();
        return Ok(());
    }
    if command == "programs" {
        return programs();
    }
    // Помощник TUN: его запускает `tun-test` с правами администратора. Хранилище ему не нужно.
    if command == "tun-helper" {
        let parent: u32 = args.words.get(1).and_then(|p| p.parse().ok()).ok_or("tun-helper <номер процесса>")?;
        let root = find_root()?;
        let engines = Engines::load(&root)?;
        std::process::exit(ninja_motor::tun::helper_main(engines.path(EngineKind::SingBox), &root.join("runtime").join("tun"), parent));
    }
    let ctx = Ctx::open()?;
    match command {
        "sources" => sources(&ctx),
        "add" => add(&ctx, args.name.as_deref()),
        // `import` — старое имя: раньше подписка скачивалась из .env.
        "refresh" | "import" => refresh(&ctx, args.rest(1).as_deref()),
        "rename" => rename(&ctx, &args),
        "remove" => remove(&ctx, args.words.get(1).map(String::as_str), args.yes),
        "list" => list(&ctx, args.rest(1).as_deref()),
        "probe" => probe(&ctx, args.rest(1).as_deref()),
        "check" => check(&ctx, args.rest(1).as_deref()),
        "connect" => connect_command(&ctx, args.rest(1).as_deref(), args.browser),
        "tun-test" => tun_test(&ctx, args.rest(1).as_deref()),
        other => {
            print_help();
            Err(format!("нет команды «{other}»"))
        }
    }
}

fn print_help() {
    println!("ninja — мотор ninja-vpn без окна. Источники общие с окном.\n");
    println!("  sources                       источники: подписки и ключи (сами ссылки не показываются)");
    println!("  add [--name Имя]              добавить подписку или ключ: вставь ссылку и нажми Enter");
    println!("                                (или без экрана: Get-Clipboard | ninja add)");
    println!("  refresh [источник]            скачать свежие списки серверов — всех или одного источника");
    println!("  rename <источник> <имя>       переименовать источник");
    println!("  remove <источник> [--yes]     удалить источник (спросит подтверждение)");
    println!("  list [источник]               серверы с номерами");
    println!("  probe [источник]              проверить скорость серверов");
    println!("  check <сервер>                собрать конфиг и проверить его ядром");
    println!("  connect [сервер] [--browser]  подключиться через локальный прокси;");
    println!("                                --browser откроет твой обычный браузер через VPN");
    println!("  programs                      программы из меню «Пуск» для режима «Приложения»\n");
    println!("  <источник> — номер из sources или часть названия: refresh 1, refresh durev");
    println!("  <сервер>   — номер из list или часть имени: connect 16, connect нидерл, connect Germany 92");
}

/// Программы из меню «Пуск»: как их видит режим «Приложения».
fn programs() -> CliResult {
    use ninja_motor::apps::{self, Support};
    let list = apps::installed();
    for (i, p) in list.iter().enumerate() {
        let how = match apps::support(&p.exe) {
            Support::Chromium => "Chromium — ключ запуска",
            Support::Other => "другая — переменные окружения",
        };
        println!("{:>3}. {}  [{how}]\n     {}", i + 1, p.name, p.exe.display());
    }
    println!("\nВсего: {}", list.len());
    Ok(())
}

/// Папку проекта ищем по файлу engines/engines.json — от текущей папки вверх.
fn find_root() -> Result<PathBuf, String> {
    let start = std::env::current_dir().map_err(|e| e.to_string())?;
    Ok(start
        .ancestors()
        .chain(Path::new(env!("CARGO_MANIFEST_DIR")).ancestors())
        .find(|dir| dir.join("engines").join("engines.json").exists())
        .ok_or("не нашёл папку проекта ninja-vpn (в ней должен быть engines/engines.json)")?
        .to_path_buf())
}

/// Всё, что нужно командам: папка проекта (ядра, рабочие файлы) и хранилище источников.
struct Ctx {
    root: PathBuf,
    runtime: PathBuf,
    store: SourceStore,
}

impl Ctx {
    /// Папку проекта ищем по файлу engines/engines.json — от текущей папки вверх.
    fn open() -> Result<Self, String> {
        let root = find_root()?;
        let store = SourceStore::open(SourceStore::default_dir());
        // Старая подписка из .env (если окно ещё не перенесло её само).
        store.migrate_env(&root);
        Ok(Self { runtime: root.join("runtime"), root, store })
    }

    fn engines(&self) -> Result<Engines, String> {
        Engines::load(&self.root)
    }

    fn sources(&self) -> Result<Vec<SourceInfo>, String> {
        let list = self.store.list().map_err(|e| e.to_string())?;
        if list.is_empty() {
            return Err("источников пока нет — добавь подписку или ключ: ninja add".into());
        }
        Ok(list)
    }

    /// Все серверы всех источников с общей нумерацией, как в `list`.
    /// Номер не меняется, пока не обновишь источник.
    fn catalog(&self) -> Result<Vec<Group>, String> {
        let mut number = 0;
        let groups = self
            .sources()?
            .into_iter()
            .map(|info| {
                let servers = self
                    .store
                    .servers(&info.id)
                    .map(|list| {
                        list.into_iter()
                            .map(|(key, entry)| {
                                number += 1;
                                Server { number, key, entry }
                            })
                            .collect()
                    })
                    .map_err(|e| e.to_string());
                Group { info, servers }
            })
            .collect();
        Ok(groups)
    }
}

struct Group {
    info: SourceInfo,
    servers: Result<Vec<Server>, String>,
}

struct Server {
    number: usize,
    /// `<id источника>/<имя>` — так же, как в окне.
    key: String,
    entry: Entry,
}

/// Номер источника (с 1) — по номеру, id или части названия.
fn find_source(sources: &[SourceInfo], query: &str) -> Result<usize, String> {
    let names = sources.iter().enumerate().map(|(i, s)| (i + 1, s.name.as_str()));
    if let Some(i) = sources.iter().position(|s| s.id == query) {
        return Ok(i);
    }
    match pick_by_name(names, query) {
        Pick::One(n) => Ok(n - 1),
        Pick::None => Err(format!("нет источника «{query}». Список: ninja sources")),
        Pick::Many(found) => Err(format!(
            "под «{query}» подходят несколько источников — уточни номером:\n{}",
            found.iter().map(|&n| format!("  {n}  {}", sources[n - 1].name)).collect::<Vec<_>>().join("\n")
        )),
    }
}

/// Сервер по номеру из `list`, ключу или части имени.
fn find_server<'a>(groups: &'a [Group], query: &str) -> Result<(&'a Group, &'a Server), String> {
    let all: Vec<(&Group, &Server)> =
        groups.iter().flat_map(|g| g.servers.iter().flatten().map(move |s| (g, s))).collect();
    if let Some(found) = all.iter().find(|(_, s)| s.key == query) {
        return Ok(*found);
    }
    let by_number = |n: usize| all.iter().find(|(_, s)| s.number == n).copied();
    match pick_by_name(all.iter().map(|(_, s)| (s.number, s.entry.label.as_str())), query) {
        Pick::One(n) => Ok(by_number(n).expect("номер взят из списка")),
        Pick::None => Err(format!("нет сервера «{query}». Список: ninja list")),
        Pick::Many(found) => {
            let lines: Vec<String> = found
                .iter()
                .take(10)
                .filter_map(|&n| by_number(n))
                .map(|(g, s)| format!("  {:>3}  {}  ({})", s.number, s.entry.label, g.info.name))
                .collect();
            let more = if found.len() > 10 { format!("\n  … и ещё {}", found.len() - 10) } else { String::new() };
            Err(format!("под «{query}» подходят {} серверов — уточни номером:\n{}{more}", found.len(), lines.join("\n")))
        }
    }
}

#[derive(Debug, PartialEq)]
enum Pick {
    One(usize),
    None,
    Many(Vec<usize>),
}

/// Выбор по номеру или имени: `12` → №12; «нидерл» → все, где есть «нидерл» (без учёта регистра).
/// Если имя совпало целиком — берём его, даже когда есть более длинные похожие:
/// «Germany 🇩🇪» не путается с «Germany 92 🇩🇪 → [Белые списки]».
fn pick_by_name<'a>(items: impl Iterator<Item = (usize, &'a str)>, query: &str) -> Pick {
    let items: Vec<(usize, &str)> = items.collect();
    if let Ok(n) = query.trim().parse::<usize>() {
        return if items.iter().any(|(i, _)| *i == n) { Pick::One(n) } else { Pick::None };
    }
    let query = query.trim().to_lowercase();
    let exact: Vec<usize> = items.iter().filter(|(_, name)| name.trim().to_lowercase() == query).map(|(i, _)| *i).collect();
    if let [one] = exact.as_slice() {
        return Pick::One(*one);
    }
    let found: Vec<usize> = items.iter().filter(|(_, name)| name.to_lowercase().contains(&query)).map(|(i, _)| *i).collect();
    match found.as_slice() {
        [] => Pick::None,
        [one] => Pick::One(*one),
        _ => Pick::Many(found),
    }
}

fn sources(ctx: &Ctx) -> CliResult {
    let list = ctx.sources()?;
    println!("Источников: {} (хранилище {})\n", list.len(), SourceStore::default_dir().display());
    for (i, info) in list.iter().enumerate() {
        let count = ctx.store.servers(&info.id).map(|s| s.len()).unwrap_or(0);
        let kind = match info.kind {
            SourceKind::Subscription => "подписка",
            SourceKind::Key => "ключ",
        };
        println!("{:>3}  {}", i + 1, info.name);
        println!("       {kind} · {} · {} · обновлено {}", info.hint, servers_word(count), ago(info.updated));
        if let Some(usage) = &info.usage {
            println!("       {}", usage_line(usage));
        }
    }
    Ok(())
}

/// Ссылку читаем из ввода, а не из командной строки: так она не попадёт в историю команд.
fn add(ctx: &Ctx, name: Option<&str>) -> CliResult {
    if io::stdin().is_terminal() {
        print!("Вставь ссылку-подписку (https://…) или ключ vless://… и нажми Enter:\n> ");
        let _ = io::stdout().flush();
    }
    let mut link = String::new();
    io::stdin().lock().read_line(&mut link).map_err(|e| e.to_string())?;
    let link = link.trim();
    if link.is_empty() {
        return Err("ссылка пустая — ничего не добавлено".into());
    }
    println!("Проверяю…");
    let info = ctx.store.add(name, link).map_err(|e| e.to_string())?;
    let count = ctx.store.servers(&info.id).map(|s| s.len()).unwrap_or(0);
    let number = ctx.sources()?.iter().position(|s| s.id == info.id).map_or(0, |i| i + 1);
    println!("✓ Добавлен источник {number} «{}»: {}. Ссылка сохранена зашифрованной.", info.name, servers_word(count));
    Ok(())
}

fn refresh(ctx: &Ctx, query: Option<&str>) -> CliResult {
    let list = ctx.sources()?;
    let chosen: Vec<usize> = match query {
        Some(q) => vec![find_source(&list, q)?],
        None => (0..list.len()).collect(),
    };
    let mut failed = 0;
    for i in &chosen {
        let info = &list[*i];
        print!("{:>3}  {} — обновляю… ", i + 1, info.name);
        let _ = io::stdout().flush();
        match ctx.store.refresh(&info.id) {
            Ok(fresh) => {
                let servers = ctx.store.servers(&fresh.id).map_err(|e| e.to_string())?;
                let ok = servers.iter().filter(|(_, e)| e.result.is_ok()).count();
                println!("✓ {}, подходят {ok}", servers_word(servers.len()));
                if let Some(usage) = &fresh.usage {
                    println!("       {}", usage_line(usage));
                }
            }
            Err(e) => {
                failed += 1;
                println!("✗ {e}");
            }
        }
    }
    if failed == chosen.len() {
        return Err("ни один источник не обновился — список серверов остался прежним".into());
    }
    println!("\nНомера серверов могли поменяться — посмотри: ninja list");
    Ok(())
}

fn rename(ctx: &Ctx, args: &Args) -> CliResult {
    let query = args.words.get(1).ok_or("укажи источник и новое имя: ninja rename 1 Мой VPN")?;
    let name = args.rest(2).ok_or("укажи новое имя: ninja rename 1 Мой VPN")?;
    let list = ctx.sources()?;
    let info = &list[find_source(&list, query)?];
    let renamed = ctx.store.rename(&info.id, &name).map_err(|e| e.to_string())?;
    println!("✓ «{}» теперь называется «{}»", info.name, renamed.name);
    Ok(())
}

fn remove(ctx: &Ctx, query: Option<&str>, yes: bool) -> CliResult {
    let query = query.ok_or("укажи, какой источник удалить: ninja remove 2")?;
    let list = ctx.sources()?;
    let info = &list[find_source(&list, query)?];
    if !yes {
        print!("Удалить источник «{}» ({})? Ссылка удалится с этого компьютера. Напиши «да»: ", info.name, info.hint);
        let _ = io::stdout().flush();
        let mut answer = String::new();
        io::stdin().lock().read_line(&mut answer).map_err(|e| e.to_string())?;
        if !matches!(answer.trim().to_lowercase().as_str(), "да" | "д" | "yes" | "y") {
            println!("Не удаляю.");
            return Ok(());
        }
    }
    ctx.store.remove(&info.id).map_err(|e| e.to_string())?;
    println!("✓ Источник «{}» удалён", info.name);
    Ok(())
}

fn list(ctx: &Ctx, query: Option<&str>) -> CliResult {
    let groups = ctx.catalog()?;
    let only = query.map(|q| find_source(&groups.iter().map(|g| g.info.clone()).collect::<Vec<_>>(), q)).transpose()?;
    for (i, group) in groups.iter().enumerate().filter(|(i, _)| only.is_none_or(|o| o == *i)) {
        println!("\n=== Источник {} · {}", i + 1, group.info.name);
        let servers = match &group.servers {
            Ok(s) => s,
            Err(e) => {
                println!("     список серверов не прочитать: {e}. Попробуй: ninja refresh {}", i + 1);
                continue;
            }
        };
        let ok = servers.iter().filter(|s| s.entry.result.is_ok()).count();
        println!("    {}, подходят {ok}", servers_word(servers.len()));
        // Повторяющиеся предупреждения пишем один раз с числом серверов, а не 43 раза.
        let shared = shared_warnings(servers);
        for (w, count) in &shared {
            let whom = if *count == ok { "у всех".to_string() } else { format!("у {count} из {ok}") };
            println!("    ! {whom}: {w}");
        }
        println!();
        for server in servers {
            match &server.entry.result {
                Ok(profile) => {
                    println!("{:>4}  ✓  {}  —  {}", server.number, server.entry.label, profile.summary());
                    for w in profile.warnings.iter().filter(|w| !shared.iter().any(|(s, _)| s == *w)) {
                        println!("           ! {w}");
                    }
                }
                Err(e) => println!("{:>4}  ✗  {}  —  {e}", server.number, server.entry.label),
            }
        }
    }
    println!("\nПодключиться: ninja connect <номер или часть имени>");
    Ok(())
}

/// Проверить скорость подходящих серверов (всех источников или одного), от быстрых к медленным.
fn probe(ctx: &Ctx, query: Option<&str>) -> CliResult {
    let groups = ctx.catalog()?;
    let only = query.map(|q| find_source(&groups.iter().map(|g| g.info.clone()).collect::<Vec<_>>(), q)).transpose()?;
    let chosen: Vec<&Server> = groups
        .iter()
        .enumerate()
        .filter(|(i, _)| only.is_none_or(|o| o == *i))
        .flat_map(|(_, g)| g.servers.iter().flatten())
        .collect();
    let items: Vec<(String, Profile)> =
        chosen.iter().filter_map(|s| s.entry.result.as_ref().ok().map(|p| (s.key.clone(), p.clone()))).collect();
    if items.is_empty() {
        return Err("нет подходящих серверов для проверки".into());
    }
    println!("Проверяю {} серверов одновременно…", items.len());
    let started = Instant::now();
    let mut results = ninja_motor::probe::probe_all(items, &ctx.engines()?, &ctx.runtime);
    results.sort_by_key(|r| r.latency.unwrap_or(Duration::MAX));
    for r in &results {
        let server = chosen.iter().find(|s| s.key == r.key).expect("ключ взят из списка");
        match r.latency {
            Some(t) => println!("{:>6} мс  {:>4}  {}", t.as_millis(), server.number, server.entry.label),
            None => println!("   нет    {:>4}  {}  ({})", server.number, server.entry.label, r.error.as_deref().unwrap_or("")),
        }
    }
    let ok = results.iter().filter(|r| r.latency.is_some()).count();
    println!("\nОтветили {ok} из {}, проверка заняла {:.1} с", results.len(), started.elapsed().as_secs_f32());
    Ok(())
}

/// Сервер по запросу, а без запроса — первый подходящий.
fn choose(groups: &[Group], query: Option<&str>) -> Result<(String, String, Profile), String> {
    let (group, server) = match query {
        Some(q) => find_server(groups, q)?,
        None => groups
            .iter()
            .flat_map(|g| g.servers.iter().flatten().map(move |s| (g, s)))
            .find(|(_, s)| s.entry.result.is_ok())
            .ok_or("ни в одном источнике нет подходящего сервера")?,
    };
    let profile = server.entry.result.as_ref().map_err(|e| format!("сервер {} не подходит: {e}", server.number))?;
    Ok((format!("{} «{}»", server.number, server.entry.label), group.info.name.clone(), profile.clone()))
}

fn check(ctx: &Ctx, query: Option<&str>) -> CliResult {
    let query = query.ok_or("укажи сервер: ninja check 16 или ninja check нидерл")?;
    let (title, source, profile) = choose(&ctx.catalog()?, Some(query))?;
    let engines = ctx.engines()?;
    fs::create_dir_all(&ctx.runtime).map_err(|e| e.to_string())?;
    let kind = profile.engine();
    let config = match kind {
        EngineKind::SingBox => singbox::local_proxy_config(&profile, PREFERRED_PORT),
        EngineKind::Xray => xray::local_proxy_config(&profile, PREFERRED_PORT),
    };
    let config_path = ctx.runtime.join(format!("{}.json", kind.name()));
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).map_err(|e| e.to_string())?;
    engine::check_config(kind, engines.path(kind), &config_path).map_err(|e| e.to_string())?;
    println!("Сервер {title} ({source}, {}): конфиг принят ядром {}", profile.summary(), kind.label());
    print_warnings(&profile);
    Ok(())
}

fn connect_command(ctx: &Ctx, query: Option<&str>, open_browser: bool) -> CliResult {
    let (title, source, profile) = choose(&ctx.catalog()?, query)?;
    println!("Сервер {title} из «{source}»: {}, ядро {}", profile.summary(), profile.engine().label());
    print_warnings(&profile);

    let home = verify::direct();
    let options = ConnectOptions { engines: ctx.engines()?, runtime_dir: ctx.runtime.clone(), listen_port: free_port() };
    let mut session = connect(&profile, &options, print_state).map_err(|e| e.to_string())?;

    let report = &session.report;
    match &report.exit {
        Some(exit) => println!("  Сайты видят тебя здесь: {} (IP {})", exit.country, mask_ip(&exit.ip)),
        None => println!("  Сервисы адреса не ответили, но запасной проверочный сайт открылся"),
    }
    match &home {
        Some(home) => println!("  До подключения:        {} (IP {})", home.country, mask_ip(&home.ip)),
        None => println!("  Откуда был выход до подключения, проверить не удалось"),
    }
    println!(
        "  Первый ответ: {} мс; 64 КБ загружены за {} мс (с сайта {})",
        report.first_response.as_millis(),
        report.bulk_time.as_millis(),
        report.bulk_source
    );
    println!("\nЛокальный прокси: 127.0.0.1:{} (HTTP и SOCKS5)", session.listen_port);
    if open_browser {
        launch_browser(session.listen_port)?;
    }
    println!("Нажми Enter, чтобы отключиться.");
    let _ = io::stdin().lock().read_line(&mut String::new());
    if !session.is_alive() {
        println!("Внимание: ядро остановилось раньше, чем ты отключился.");
    }
    session.disconnect(print_state);
    Ok(())
}

/// Предупреждения, которые встречаются больше чем у одного сервера, и у скольких.
fn shared_warnings(servers: &[Server]) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for profile in servers.iter().filter_map(|s| s.entry.result.as_ref().ok()) {
        for w in &profile.warnings {
            match counts.iter_mut().find(|(seen, _)| seen == w) {
                Some((_, n)) => *n += 1,
                None => counts.push((w.clone(), 1)),
            }
        }
    }
    counts.retain(|(_, n)| *n > 1);
    counts
}

fn print_warnings(profile: &Profile) {
    for w in &profile.warnings {
        println!("  ! {w}");
    }
}

fn print_state(state: &State) {
    let mark = match state {
        State::Connected | State::Disconnected => "✓",
        State::Failed(_) => "✗",
        _ => "…",
    };
    println!("{mark} {}", state.title());
}

fn usage_line(usage: &Usage) -> String {
    let gib = |bytes: u64| format!("{:.1} ГБ", bytes as f64 / 1024f64.powi(3));
    let used = gib(usage.upload + usage.download);
    let limit = if usage.total == 0 { "без лимита".to_string() } else { format!("из {}", gib(usage.total)) };
    let until = usage.expire_date().map(|d| format!(" · действует до {d}")).unwrap_or_default();
    format!("трафик: {used} {limit}{until}")
}

/// «1 сервер», «3 сервера», «43 сервера», «11 серверов».
fn servers_word(n: usize) -> String {
    let word = match (n % 10, n % 100) {
        (1, r) if r != 11 => "сервер",
        (2..=4, r) if !(12..=14).contains(&r) => "сервера",
        _ => "серверов",
    };
    format!("{n} {word}")
}

/// «только что», «5 мин назад», «3 ч назад», «2 дн. назад».
fn ago(updated: Option<u64>) -> String {
    let Some(updated) = updated else { return "никогда".into() };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    match now.saturating_sub(updated) {
        s if s < 60 => "только что".into(),
        s if s < 3600 => format!("{} мин назад", s / 60),
        s if s < 86_400 => format!("{} ч назад", s / 3600),
        s => format!("{} дн. назад", s / 86_400),
    }
}

/// Из подробного журнала sing-box: какая программа куда ушла — пары (путь программы, `vpn`/`direct`).
fn routes_from_log(log: &str) -> Vec<(String, String)> {
    use std::collections::HashMap;
    let id = |line: &str| line.split_once('[').and_then(|(_, r)| r.split_whitespace().next()).map(str::to_string);
    let (mut process, mut outbound) = (HashMap::new(), HashMap::new());
    for line in log.lines() {
        let Some(id) = id(line) else { continue };
        if let Some((_, path)) = line.split_once("found process path: ") {
            process.insert(id, path.trim().to_string());
        } else if line.contains("outbound/socks[vpn]") {
            outbound.insert(id, "vpn".to_string());
        } else if line.contains("outbound/direct[direct]") {
            outbound.insert(id, "direct".to_string());
        }
    }
    outbound.into_iter().filter_map(|(id, out)| process.get(&id).map(|p| (p.clone(), out))).collect()
}

/// Безопасная проверка TUN: сетевая карта забирает только адреса 1.1.1.1 и 1.0.0.1, остальная
/// сеть компьютера не меняется. Две копии curl: «curl-vpn» должна выйти через VPN,
/// «curl-direct» стоит в исключениях и должна выйти напрямую. Нужно одно «Да» в окне Windows.
fn tun_test(ctx: &Ctx, query: Option<&str>) -> CliResult {
    use ninja_motor::tun::{self, Split, Tun, TunOptions};
    let (title, _, profile) = choose(&ctx.catalog()?, query)?;
    let engines = ctx.engines()?;
    println!("Сервер {title}");
    let options = ConnectOptions { engines: engines.clone(), runtime_dir: ctx.runtime.clone(), listen_port: free_port() };
    let session = connect(&profile, &options, print_state).map_err(|e| e.to_string())?;
    let vpn_ip = session.report.exit.as_ref().map(|e| e.ip.clone()).ok_or("сервер не сообщил адрес выхода")?;

    let apps = ctx.runtime.join("test-apps");
    fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
    let curl = PathBuf::from(std::env::var("WINDIR").unwrap_or(r"C:\Windows".into())).join(r"System32\curl.exe");
    let (via_vpn, direct) = (apps.join("curl-vpn.exe"), apps.join("curl-direct.exe"));
    for copy in [&via_vpn, &direct] {
        fs::copy(&curl, copy).map_err(|e| format!("копия curl: {e}"))?;
    }
    let trace = |exe: &Path| -> String {
        let out = std::process::Command::new(exe).args(["-s", "-m", "10", "https://1.1.1.1/cdn-cgi/trace"]).output();
        let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        text.lines().find_map(|l| l.strip_prefix("ip=")).unwrap_or("нет ответа").to_string()
    };
    let before = trace(&via_vpn);
    println!("Без TUN curl выходит с адреса {} (напрямую)", mask_ip(&before));

    let opts = TunOptions {
        split: Split::Except(vec![direct.clone()]),
        proxy_port: session.listen_port,
        bypass: vec![engines.path(EngineKind::SingBox).to_path_buf(), engines.path(EngineKind::Xray).to_path_buf()],
        only_routes: vec!["1.1.1.1/32".into(), "1.0.0.1/32".into()],
        servers: tun::resolve_servers(std::slice::from_ref(&profile.server)).into_iter().filter(|(name, _)| name.parse::<std::net::IpAddr>().is_err()).collect(),
        verbose: false,
    };
    let config = tun::config(&opts);
    let file = engine::ConfigFile::write(&ctx.runtime, "tun-check", &config).map_err(|e| e.to_string())?;
    engine::check_config(EngineKind::SingBox, engines.path(EngineKind::SingBox), file.path()).map_err(|e| e.to_string())?;
    drop(file);
    println!("sing-box принял конфиг TUN. Сейчас Windows спросит разрешение администратора…");
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut tun = Tun::start(&me, &format!("tun-helper {}", std::process::id()), &ctx.runtime.join("tun"), &config, Duration::from_secs(180), 0)?;
    std::thread::sleep(Duration::from_secs(2));
    // Что видит Windows, пока TUN работает: сетевая карта и маршрут к 1.1.1.1.
    let diag = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", "Get-NetAdapter -IncludeHidden | Where-Object InterfaceDescription -match 'sing|wintun|Tun' | Format-Table Name, InterfaceDescription, Status, ifIndex -AutoSize | Out-String -Width 160; Find-NetRoute -RemoteIPAddress 1.1.1.1 | Select-Object -Last 1 | Format-List InterfaceAlias, DestinationPrefix, NextHop, RouteMetric | Out-String; Get-NetRoute -DestinationPrefix 1.1.1.1/32 -ErrorAction SilentlyContinue | Format-Table InterfaceAlias, RouteMetric -AutoSize | Out-String"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let (got_vpn, got_direct) = (trace(&via_vpn), trace(&direct));
    tun.stop();
    println!("Пока TUN работал:\n{}", diag.trim());
    let log = fs::read_to_string(ctx.runtime.join("tun").join("sing-box.log")).unwrap_or_default();
    println!("Журнал TUN (последние строки):\n{}", log.lines().rev().take(25).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
    std::thread::sleep(Duration::from_secs(1));
    let after = trace(&via_vpn);

    let mut ok = 0;
    let mut line = |good: bool, text: String| {
        ok += good as u32;
        println!("  {} {text}", if good { "ДА " } else { "НЕТ" });
    };
    // У одного VPN-сервера бывает несколько адресов выхода, поэтому куда ушло соединение,
    // смотрим в журнале TUN: у каждого соединения свой номер, по нему видно программу и выход.
    let routes = routes_from_log(&log);
    let went = |exe: &str, out: &str| routes.iter().any(|(p, o)| p.ends_with(exe) && o == out);
    line(went("curl-vpn.exe", "vpn") && !went("curl-vpn.exe", "direct"), format!("curl-vpn ушла в VPN (выход {}, VPN {})", mask_ip(&got_vpn), mask_ip(&vpn_ip)));
    line(went("curl-direct.exe", "direct") && !went("curl-direct.exe", "vpn"), "curl-direct (исключение) ушла напрямую".into());
    // Durev (если включён) меняет свой адрес выхода почти на каждый запрос, поэтому «напрямую»
    // проверяем так: ответ есть, и это не адрес нашего VPN.
    let not_vpn = |ip: &str| ip != vpn_ip && ip != "нет ответа";
    line(not_vpn(&got_direct), format!("у curl-direct адрес не VPN: {}", mask_ip(&got_direct)));
    line(not_vpn(&after), format!("после остановки TUN — снова напрямую: {}", mask_ip(&after)));
    println!("Итог: {ok} из 4");
    drop(session);
    if ok == 4 { Ok(()) } else { Err("проверка TUN не прошла".into()) }
}
fn free_port() -> u16 {
    if TcpListener::bind(("127.0.0.1", PREFERRED_PORT)).is_ok() {
        return PREFERRED_PORT;
    }
    TcpListener::bind(("127.0.0.1", 0)).and_then(|l| l.local_addr()).map(|a| a.port()).unwrap_or(PREFERRED_PORT)
}

/// Обычный браузер (первый найденный) через VPN: с закладками и паролями.
/// Уже открытый напрямую не трогаем — прокси он примет только при новом запуске.
fn launch_browser(port: u16) -> CliResult {
    let found = browser::installed().into_iter().next().ok_or("не нашёл ни Chrome, ни Edge, ни Яндекс Браузер")?;
    match browser::running(&found) {
        browser::Running::No => {}
        browser::Running::Proxy(p) if p == port => {}
        _ => return Err(format!("{} уже открыт без VPN: закрой его и запусти команду снова", found.name)),
    }
    browser::launch(&found, Some(port), false).map_err(|e| format!("не удалось открыть {}: {e}", found.name))?;
    println!("Открыл {} через VPN (твой обычный профиль). После отключения VPN его нужно перезапустить.", found.name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: [(usize, &str); 4] = [
        (1, "Auto → [🚀 Оптимальная локация]"),
        (2, "Germany 🇩🇪"),
        (3, "Germany 92 🇩🇪 → [📃 Белые списки]"),
        (4, "Netherlands 🇳🇱"),
    ];

    #[test]
    fn picks_by_number_and_name() {
        let pick = |q: &str| pick_by_name(NAMES.iter().copied(), q);
        assert_eq!(pick("4"), Pick::One(4));
        assert_eq!(pick("9"), Pick::None);
        assert_eq!(pick("netherl"), Pick::One(4));
        assert_eq!(pick("ОПТИМАЛЬНАЯ"), Pick::One(1));
        // Целиком совпавшее имя важнее похожих длинных.
        assert_eq!(pick("germany 🇩🇪"), Pick::One(2));
        assert_eq!(pick("Germany"), Pick::Many(vec![2, 3]));
        assert_eq!(pick("germany 92"), Pick::One(3));
        assert_eq!(pick("Франция"), Pick::None);
    }

    #[test]
    fn several_words_are_one_query() {
        let args = Args::parse(["connect", "Germany", "92", "--browser"].map(String::from).into_iter()).unwrap();
        assert_eq!(args.rest(1).as_deref(), Some("Germany 92"));
        assert!(args.browser);
        assert!(Args::parse(["add", "--name"].map(String::from).into_iter()).is_err());
        assert!(Args::parse(["list", "--что-то"].map(String::from).into_iter()).is_err());
    }

    #[test]
    fn russian_plurals() {
        let words: Vec<String> = [1, 2, 5, 11, 12, 21, 22, 43, 111].map(servers_word).into();
        assert_eq!(
            words,
            ["1 сервер", "2 сервера", "5 серверов", "11 серверов", "12 серверов", "21 сервер", "22 сервера", "43 сервера", "111 серверов"]
        );
    }
    #[test]
    fn tun_log_routes() {
        let log = "+0300 2026-10-06 01:06:37 INFO [3873871006 2ms] inbound/tun[tun-in]: inbound connection to 1.1.1.1:443
+0300 2026-10-06 01:06:37 INFO [3873871006 2ms] router: found process path: C:\\x\\curl-vpn.exe
+0300 2026-10-06 01:06:37 INFO [3873871006 2ms] outbound/socks[vpn]: outbound connection to 1.1.1.1:443
+0300 2026-10-06 01:06:37 INFO [1659489045 0ms] router: found process path: C:\\x\\curl-direct.exe
+0300 2026-10-06 01:06:37 INFO [1659489045 3ms] outbound/direct[direct]: outbound connection to 1.1.1.1:443
+0300 2026-10-06 01:06:30 INFO inbound/tun[tun-in]: started at ninja-vpn";
        let mut routes = routes_from_log(log);
        routes.sort();
        assert_eq!(routes, vec![(r"C:\x\curl-direct.exe".into(), "direct".into()), (r"C:\x\curl-vpn.exe".into(), "vpn".into())]);
    }
}
