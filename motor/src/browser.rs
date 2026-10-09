//! Режим «Браузер»: твой обычный браузер (закладки, пароли, расширения), запущенный
//! через локальный прокси мотора.
//!
//! Браузер на Chromium принимает прокси только при запуске: если он уже открыт, новый запуск
//! лишь покажет окно уже работающей копии, и та продолжит ходить напрямую. Поэтому сначала
//! смотрим, как он запущен ([`running`]), и, если нужно, перезапускаем ([`restart`]) — с
//! восстановлением вкладок. Если VPN отключится, браузер с нашим прокси перестанет открывать
//! сайты, а не уйдёт молча в обход — пока его не перезапустят без VPN.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::apps::{proxy_port, same_path};
use crate::winsys;

#[derive(Debug, Clone)]
pub struct Browser {
    pub id: &'static str,
    pub name: &'static str,
    pub exe: PathBuf,
}

/// Где искать exe: переменная окружения с папкой и путь внутри неё.
type Place = (&'static str, &'static str);

/// Браузеры на движке Chromium, которые понимают нужные нам ключи запуска: id, имя, где искать.
const KNOWN: &[(&str, &str, &[Place])] = &[
    ("chrome", "Chrome", &[
        ("ProgramFiles", r"Google\Chrome\Application\chrome.exe"),
        ("ProgramFiles(x86)", r"Google\Chrome\Application\chrome.exe"),
        ("LOCALAPPDATA", r"Google\Chrome\Application\chrome.exe"),
    ]),
    ("edge", "Edge", &[
        ("ProgramFiles(x86)", r"Microsoft\Edge\Application\msedge.exe"),
        ("ProgramFiles", r"Microsoft\Edge\Application\msedge.exe"),
    ]),
    ("yandex", "Яндекс Браузер", &[("LOCALAPPDATA", r"Yandex\YandexBrowser\Application\browser.exe")]),
    ("brave", "Brave", &[
        ("ProgramFiles", r"BraveSoftware\Brave-Browser\Application\brave.exe"),
        ("LOCALAPPDATA", r"BraveSoftware\Brave-Browser\Application\brave.exe"),
    ]),
];

/// Какие из известных браузеров установлены.
pub fn installed() -> Vec<Browser> {
    KNOWN
        .iter()
        .filter_map(|(id, name, places)| {
            places
                .iter()
                .filter_map(|(var, rel)| std::env::var_os(var).map(|base| PathBuf::from(base).join(rel)))
                .find(|p| p.exists())
                .map(|exe| Browser { id, name: if *id == "yandex" && crate::i18n::lang() != "ru" { "Yandex Browser" } else { name }, exe })
        })
        .collect()
}

/// Как сейчас запущен обычный профиль браузера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Running {
    /// Не запущен.
    No,
    /// Запущен через наш прокси на этом порту.
    Proxy(u16),
    /// Запущен сам по себе — ходит напрямую.
    Direct,
}

/// Процессы обычного профиля браузера и как он запущен.
///
/// Главный процесс браузера — тот, чей «родитель» не он сам; у его помощников (вкладки,
/// видео, расширения) родитель — главный. Копии с другой папкой профиля (`--user-data-dir`:
/// старые отдельные окна ninja-vpn, тестовые профили) — это не твой браузер, их не трогаем.
fn instance(browser: &Browser) -> (Running, Vec<u32>) {
    let name = browser.exe.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let procs = winsys::processes();
    let ours: HashMap<u32, u32> = procs
        .iter()
        .filter(|p| p.name.to_lowercase() == name)
        .filter(|p| winsys::image_path(p.pid).is_some_and(|path| same_path(&path, &browser.exe)))
        .map(|p| (p.pid, p.parent))
        .collect();
    // Главные процессы обычного профиля и порт прокси у каждого.
    let mut roots: HashMap<u32, Option<u16>> = HashMap::new();
    for (&pid, parent) in &ours {
        if ours.contains_key(parent) {
            continue;
        }
        let Some(line) = winsys::command_line(pid) else { continue };
        if !line.contains("--user-data-dir") {
            roots.insert(pid, proxy_port(&line));
        }
    }
    if roots.is_empty() {
        return (Running::No, Vec::new());
    }
    let root_of = |mut pid: u32| -> Option<u32> {
        for _ in 0..16 {
            if roots.contains_key(&pid) {
                return Some(pid);
            }
            pid = *ours.get(&pid)?;
        }
        None
    };
    let pids: Vec<u32> = ours.keys().copied().filter(|p| root_of(*p).is_some()).collect();
    let ports: HashSet<Option<u16>> = roots.values().copied().collect();
    let running = match ports.into_iter().collect::<Vec<_>>()[..] {
        [Some(port)] => Running::Proxy(port),
        _ => Running::Direct,
    };
    (running, pids)
}

/// Как сейчас запущен обычный профиль браузера.
pub fn running(browser: &Browser) -> Running {
    instance(browser).0
}

/// Запустить браузер с обычным профилем: через прокси `127.0.0.1:port` или (`None`) напрямую.
/// `restore` — вернуть вкладки прошлого запуска (после нашего перезапуска).
pub fn launch(browser: &Browser, port: Option<u16>, restore: bool) -> io::Result<()> {
    let mut cmd = Command::new(&browser.exe);
    if let Some(port) = port {
        cmd.arg(format!("--proxy-server=http://127.0.0.1:{port}"));
        // WebRTC (звонки в браузере) иначе может отправить UDP напрямую, мимо прокси.
        cmd.arg("--force-webrtc-ip-handling-policy=disable_non_proxied_udp");
    }
    if restore {
        cmd.arg("--restore-last-session");
    }
    cmd.spawn().map(|_| ())
}

/// Почему перезапуск не удался.
#[derive(Debug, PartialEq, Eq)]
pub enum RestartError {
    /// Браузер не закрылся за 6 с.
    DidNotClose,
    /// Не удалось запустить exe.
    Launch(String),
    /// Запустился, но не так, как нужно (или не запустился вовсе) за 8 с.
    DidNotStart,
}

/// Закрыть обычный профиль браузера и открыть его заново — через прокси или напрямую.
/// Вкладки возвращаются (`--restore-last-session`).
///
/// Закрываем сразу все процессы: так браузер сохраняет все окна как прошлый сеанс.
/// Если закрывать окна по одному, в «прошлом сеансе» останется только последнее.
pub fn restart(browser: &Browser, port: Option<u16>) -> Result<(), RestartError> {
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let (_, pids) = instance(browser);
        if pids.is_empty() {
            break;
        }
        if Instant::now() > deadline {
            return Err(RestartError::DidNotClose);
        }
        for pid in pids {
            winsys::terminate(pid);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    // Сразу после закрытия браузер ещё мгновение держит «я уже запущен» — небольшая пауза.
    std::thread::sleep(Duration::from_millis(600));
    launch(browser, port, true).map_err(|e| RestartError::Launch(e.to_string()))?;
    let want = port.map_or(Running::Direct, Running::Proxy);
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(300));
        if running(browser) == want {
            return Ok(());
        }
    }
    Err(RestartError::DidNotStart)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn missing_browser_is_not_running() {
        let none = Browser { id: "x", name: "X", exe: PathBuf::from(r"C:\nope\no-such-browser.exe") };
        assert_eq!(instance(&none), (Running::No, Vec::new()));
    }

    /// Только смотрит, ничего не закрывает: `cargo test -p ninja-motor browsers_now -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn browsers_now() {
        for b in installed() {
            let (run, pids) = instance(&b);
            println!("{}: {run:?}, процессов {}", b.name, pids.len());
        }
    }
}
