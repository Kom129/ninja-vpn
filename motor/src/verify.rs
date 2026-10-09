//! Проверка, что через сервер реально идут данные.
//!
//! «Подключено» показываем только после этой проверки: запущенный процесс
//! или открытый порт ещё ничего не доказывают.
//!
//! Проверочные сайты сами бывают недоступны (2026-10-03 Cloudflare отвечал через раз даже
//! без нашего туннеля). Поэтому каждый шаг спрашиваем у нескольких независимых компаний
//! одновременно и берём первый удачный ответ. Ошибка — только если не ответил никто.

use std::io::Read;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Где узнать, с какого адреса и из какой страны сайты видят запрос.
const EXIT_SOURCES: &[(&str, &str)] = &[
    ("Cloudflare", "https://cloudflare.com/cdn-cgi/trace"),
    ("ipinfo.io", "https://ipinfo.io/json"),
];
/// Просто «сайт открылся» — если сервисы адреса недоступны сами по себе.
const REACH_SOURCE: (&str, &str) = ("Google", "https://www.google.com/generate_204");
/// Пробная загрузка. Больше 16 КБ нарочно: по наблюдениям Cloudflare (июнь 2025)
/// соединение при блокировке иногда «замирает» именно после первых ~16 КБ.
/// С больших файлов читаем только первые 64 КБ.
const BULK_SOURCES: &[(&str, &str)] = &[
    ("Cloudflare", "https://speed.cloudflare.com/__down?bytes=65536"),
    ("Hetzner", "https://fsn1-speed.hetzner.com/100MB.bin"),
    ("CacheFly", "https://cachefly.cachefly.net/1mb.test"),
];
const BULK_BYTES: usize = 65_536;
const TIMEOUT_SECS: u64 = 10;
const HEALTH_TIMEOUT_SECS: u64 = 6;

/// Откуда сайты видят наши запросы.
#[derive(Debug, Clone)]
pub struct Exit {
    pub ip: String,
    /// Двухбуквенный код страны: NL, DE, RU.
    pub country: String,
}

#[derive(Debug, Clone)]
pub struct Report {
    /// `None`, если сервисы адреса не ответили, но обычный сайт открылся.
    pub exit: Option<Exit>,
    pub first_response: Duration,
    pub bulk_time: Duration,
    /// Чей сайт отдал пробные 64 КБ первым.
    pub bulk_source: &'static str,
}

use crate::t;

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("{}", t!("verify.no_response", secs = TIMEOUT_SECS))]
    NoResponse,
    #[error("{}", t!("verify.no_path", details = .0))]
    NoPath(String),
    /// Первый ответ через сервер пришёл, а большая загрузка — нет ни с одного сайта.
    /// Сколько успело прийти, помогает отличить «медленно» от «замирает после первых КБ».
    #[error("{}", t!("verify.stalled", exit = .exit, details = .details))]
    Stalled { exit: String, details: String },
}

/// Неудачная попытка: чей сайт, сколько байт успело прийти, что случилось.
struct Failure {
    source: &'static str,
    bytes: usize,
    timeout: bool,
    reason: String,
}

/// Проверить выход через локальный прокси ядра.
pub fn via_proxy(port: u16) -> Result<Report, VerifyError> {
    let proxy = ureq::Proxy::new(&format!("http://127.0.0.1:{port}")).expect("адрес прокси корректен");
    let agent = agent(Some(proxy), TIMEOUT_SECS);

    let started = Instant::now();
    let exit = first_response(&agent)?;
    let first_response = started.elapsed();

    let started = Instant::now();
    let jobs = BULK_SOURCES.iter().map(|&(source, url)| (source, move |a: &ureq::Agent| download(a, source, url))).collect();
    match race(&agent, jobs, |_| true) {
        Ok((bulk_source, _)) => Ok(Report { exit, first_response, bulk_time: started.elapsed(), bulk_source }),
        Err(failures) => Err(VerifyError::Stalled {
            exit: exit.map_or_else(|| t!("verify.exit_unknown"), |e| e.country),
            details: describe(&failures),
        }),
    }
}

/// Первый ответ через сервер: адрес выхода, а если сервисы адреса недоступны — хотя бы «сайт открылся».
fn first_response(agent: &ureq::Agent) -> Result<Option<Exit>, VerifyError> {
    type Job = Box<dyn FnOnce(&ureq::Agent) -> Result<Option<Exit>, Failure> + Send>;
    let mut jobs: Vec<(&'static str, Job)> = EXIT_SOURCES
        .iter()
        .map(|&(source, url)| (source, Box::new(move |a: &ureq::Agent| exit_from(a, source, url).map(Some)) as Job))
        .collect();
    let (source, url) = REACH_SOURCE;
    jobs.push((source, Box::new(move |a: &ureq::Agent| reach(a, source, url).map(|_| None))));
    // Ждём ответ с адресом; «сайт открылся» засчитываем, только если адрес не узнали ни у кого.
    match race(agent, jobs, Option::is_some) {
        Ok((_, exit)) => Ok(exit),
        Err(failures) if failures.iter().all(|f| f.timeout) => Err(VerifyError::NoResponse),
        Err(failures) => Err(VerifyError::NoPath(describe(&failures))),
    }
}

/// Запустить все попытки одновременно. Возвращает первую, что прошла `preferred`;
/// если таких нет — первую удачную; если удачных нет — список неудач.
fn race<T: Send + 'static, F>(
    agent: &ureq::Agent,
    jobs: Vec<(&'static str, F)>,
    preferred: impl Fn(&T) -> bool,
) -> Result<(&'static str, T), Vec<Failure>>
where
    F: FnOnce(&ureq::Agent) -> Result<T, Failure> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    for (source, job) in jobs {
        let (agent, tx) = (agent.clone(), tx.clone());
        std::thread::spawn(move || {
            let _ = tx.send((source, job(&agent)));
        });
    }
    drop(tx);
    let mut fallback = None;
    let mut failures = Vec::new();
    // Цикл заканчивается, когда ответили все (каждая попытка ограничена таймаутом агента).
    for (source, result) in rx {
        match result {
            Ok(value) if preferred(&value) => return Ok((source, value)),
            Ok(value) => fallback = fallback.or(Some((source, value))),
            Err(failure) => failures.push(failure),
        }
    }
    fallback.ok_or(failures)
}

/// Скачать первые 64 КБ, считая байты: при обрыве видно, сколько успело прийти.
fn download(agent: &ureq::Agent, source: &'static str, url: &str) -> Result<usize, Failure> {
    let fail = |bytes, e: &dyn std::fmt::Display, timeout| Failure { source, bytes, timeout, reason: short(e) };
    let mut response = agent.get(url).call().map_err(|e| fail(0, &e, is_timeout(&e)))?;
    let mut reader = response.body_mut().as_reader();
    let mut buffer = [0u8; 8192];
    let mut total = 0;
    while total < BULK_BYTES {
        match reader.read(&mut buffer) {
            Ok(0) => return Err(fail(total, &t!("verify.cut_off"), false)),
            Ok(n) => total += n,
            Err(e) => return Err(fail(total, &e, e.kind() == std::io::ErrorKind::TimedOut || e.to_string().contains("timeout"))),
        }
    }
    Ok(total)
}

fn exit_from(agent: &ureq::Agent, source: &'static str, url: &str) -> Result<Exit, Failure> {
    let fail = |e: &dyn std::fmt::Display, timeout| Failure { source, bytes: 0, timeout, reason: short(e) };
    let mut response = agent.get(url).call().map_err(|e| fail(&e, is_timeout(&e)))?;
    let text = response.body_mut().with_config().limit(16 * 1024).read_to_string().map_err(|e| fail(&e, is_timeout(&e)))?;
    parse_exit(&text).ok_or_else(|| fail(&t!("verify.bad_format"), false))
}

fn reach(agent: &ureq::Agent, source: &'static str, url: &str) -> Result<(), Failure> {
    agent
        .get(url)
        .call()
        .map(|_| ())
        .map_err(|e| Failure { source, bytes: 0, timeout: is_timeout(&e), reason: short(&e) })
}

/// Адрес и страна из ответа Cloudflare (`ip=…`, `loc=…`) или ipinfo.io (JSON `ip`, `country`).
fn parse_exit(text: &str) -> Option<Exit> {
    let field = |name: &str| text.lines().find_map(|l| l.strip_prefix(name)).map(str::to_string);
    if let (Some(ip), Some(country)) = (field("ip="), field("loc=")) {
        return Some(Exit { ip, country });
    }
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(Exit { ip: json["ip"].as_str()?.to_string(), country: json["country"].as_str()?.to_string() })
}

/// По строке на сайт: «Cloudflare: нет ответа за 10 с», «Hetzner: 12 КБ, ответ оборвался».
fn describe(failures: &[Failure]) -> String {
    failures
        .iter()
        .map(|f| {
            let what = if f.timeout { t!("verify.no_answer") } else { f.reason.clone() };
            if f.bytes > 0 { t!("verify.partial", source = f.source, kb = f.bytes / 1024, what = what) } else { format!("{}: {what}", f.source) }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Быстрая проверка «канал жив» во время подключения: маленький ответ без тела
/// через прокси, у двух разных компаний одновременно. Хватает ответа любой из них.
/// Дёшево — можно звать раз в 20 секунд.
pub fn health(port: u16) -> bool {
    let proxy = ureq::Proxy::new(&format!("http://127.0.0.1:{port}")).expect("адрес прокси корректен");
    let agent = agent(Some(proxy), HEALTH_TIMEOUT_SECS);
    let (_, google) = REACH_SOURCE;
    let (tx, rx) = mpsc::channel();
    for url in [google, "https://cp.cloudflare.com/generate_204"] {
        let (agent, tx) = (agent.clone(), tx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(agent.get(url).call().is_ok());
        });
    }
    drop(tx);
    // Первый «да» — сразу ответ; «нет» — только когда не ответил никто.
    rx.iter().any(|ok| ok)
}

/// Проверка с повтором: у серверов-балансировщиков («Оптимальная локация») отдельные
/// соединения иногда попадают на неработающий выход. Поэтому одна неудача — ещё не обрыв:
/// через 5 с проверяем ещё раз на новых соединениях. «Нет» — только если обе попытки не прошли.
pub fn health_confirmed(port: u16) -> bool {
    health(port) || {
        std::thread::sleep(Duration::from_secs(5));
        health(port)
    }
}

/// Откуда видны запросы до подключения — для сравнения «было / стало».
/// Если включён другой VPN, это будет его выход, а не твой настоящий адрес.
pub fn direct() -> Option<Exit> {
    let agent = agent(None, 5);
    EXIT_SOURCES.iter().find_map(|&(source, url)| exit_from(&agent, source, url).ok())
}

fn agent(proxy: Option<ureq::Proxy>, timeout_secs: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .proxy(proxy)
        .timeout_global(Some(Duration::from_secs(timeout_secs)))
        .build()
        .into()
}

/// IP показываем не целиком: этого хватает, чтобы увидеть смену адреса.
pub fn mask_ip(ip: &str) -> String {
    match ip.split_once('.') {
        Some((first, rest)) => format!("{first}.*.*.{}", rest.rsplit('.').next().unwrap_or("")),
        None => format!("{}:…", ip.split(':').next().unwrap_or("")),
    }
}

fn is_timeout(e: &ureq::Error) -> bool {
    matches!(e, ureq::Error::Timeout(_))
}

fn short(e: &dyn std::fmt::Display) -> String {
    e.to_string().chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_from_cloudflare_and_ipinfo() {
        let cf = parse_exit("fl=1\nip=89.1.2.149\nts=1\nloc=NL\n").unwrap();
        assert_eq!((cf.ip.as_str(), cf.country.as_str()), ("89.1.2.149", "NL"));
        let info = parse_exit(r#"{"ip": "79.1.2.36", "country": "CA", "city": "Toronto"}"#).unwrap();
        assert_eq!((info.ip.as_str(), info.country.as_str()), ("79.1.2.36", "CA"));
        assert!(parse_exit("<html>").is_none());
    }

    #[test]
    fn failures_are_described_per_site() {
        let failures = [
            Failure { source: "Cloudflare", bytes: 0, timeout: true, reason: String::new() },
            Failure { source: "Hetzner", bytes: 12 * 1024, timeout: false, reason: "ответ оборвался".into() },
        ];
        assert_eq!(describe(&failures), "Cloudflare: нет ответа за 10 с\nHetzner: 12 КБ, ответ оборвался");
    }
}
