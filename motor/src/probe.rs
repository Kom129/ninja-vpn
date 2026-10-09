//! Проверка скорости серверов: настоящая задержка через каждый сервер.
//!
//! Не «пинг» до сервера, а реальный запрос в интернет через него: так видно и то,
//! что сервер жив, и то, насколько быстро он отвечает. Все серверы проверяются
//! одновременно одним процессом ядра: у каждого сервера свой локальный вход (порт),
//! правило маршрута ведёт вход ровно в его сервер. Сеть компьютера не меняется.
//!
//! Меряем задержку (время до ответа), а не пропускную способность в мегабитах.

use std::net::TcpListener;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::engine::{self, ConfigFile, EngineKind, Engines, RunningEngine};
use crate::profile::Profile;
use crate::{singbox, xray};

/// Маленький ответ без тела у двух разных компаний: если одна недоступна, спросим другую.
const TARGETS: &[&str] = &["https://www.google.com/generate_204", "https://cp.cloudflare.com/generate_204"];
const TIMEOUT: Duration = Duration::from_secs(5);
/// Сколько замеров идёт одновременно. Каждый — один маленький запрос, поэтому
/// проверяем все серверы источника разом: время проверки ≈ время самого медленного.
const PARALLEL: usize = 64;

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub key: String,
    /// Лучшая из двух попыток. `None` — сервер не ответил.
    pub latency: Option<Duration>,
    pub error: Option<String>,
}

/// Проверить серверы: `items` — пары (ключ для окна, профиль).
pub fn probe_all(items: Vec<(String, Profile)>, engines: &Engines, runtime_dir: &Path) -> Vec<ProbeResult> {
    let mut results = Vec::with_capacity(items.len());
    for kind in [EngineKind::SingBox, EngineKind::Xray] {
        let group: Vec<&(String, Profile)> = items.iter().filter(|(_, p)| p.engine() == kind).collect();
        if !group.is_empty() {
            results.extend(probe_group(kind, &group, engines, runtime_dir));
        }
    }
    results
}

use crate::t;

fn probe_group(kind: EngineKind, group: &[&(String, Profile)], engines: &Engines, runtime_dir: &Path) -> Vec<ProbeResult> {
    let fail_all = |reason: String| {
        group.iter().map(|(key, _)| ProbeResult { key: key.clone(), latency: None, error: Some(reason.clone()) }).collect()
    };
    let ports = match free_ports(group.len()) {
        Ok(p) => p,
        Err(e) => return fail_all(t!("probe.no_ports", why = e)),
    };
    let pairs: Vec<(&Profile, u16)> = group.iter().map(|(_, p)| p).zip(ports.iter().copied()).collect();
    let config = match kind {
        EngineKind::SingBox => singbox::probe_config(&pairs),
        EngineKind::Xray => xray::probe_config(&pairs),
    };
    // Своё имя у каждой проверки: две проверки подряд (или окно и командная строка)
    // не подсунут ядру чужой конфиг с чужими портами.
    let config_file = match ConfigFile::write(runtime_dir, &format!("probe-{}", kind.name()), &config) {
        Ok(f) => f,
        Err(e) => return fail_all(t!("probe.config_write", why = e)),
    };
    let exe = engines.path(kind);
    if let Err(e) = engine::check_config(kind, exe, config_file.path()) {
        return fail_all(e.to_string());
    }
    let log = runtime_dir.join(format!("probe-{}.log", kind.name()));
    // Ядро живёт до конца функции; при выходе останавливается само (drop).
    let _engine = match RunningEngine::start(kind, exe, config_file.path(), &log, ports[0]) {
        Ok(e) => e,
        Err(e) => return fail_all(e.to_string()),
    };
    drop(config_file); // ядро уже прочитало конфиг
    // Остальные входы открываются одновременно с первым, но дадим им мгновение.
    std::thread::sleep(Duration::from_millis(150));

    let jobs: Vec<(String, u16)> = group.iter().map(|(k, _)| k.clone()).zip(ports).collect();
    let mut results = measure_all(&jobs);
    // Второй круг только для не ответивших: когда стартуют десятки соединений разом,
    // живой, но медленный на рукопожатии сервер может не успеть в первый раз.
    let retry: Vec<(String, u16)> =
        jobs.iter().filter(|(k, _)| results.iter().any(|r| &r.key == k && r.latency.is_none())).cloned().collect();
    for again in measure_all(&retry) {
        if again.latency.is_some()
            && let Some(r) = results.iter_mut().find(|r| r.key == again.key)
        {
            *r = again;
        }
    }
    results
}

/// Замерить все пары (ключ, порт) одновременно, не больше PARALLEL за раз.
fn measure_all(jobs: &[(String, u16)]) -> Vec<ProbeResult> {
    let mut results = Vec::with_capacity(jobs.len());
    for chunk in jobs.chunks(PARALLEL) {
        let (tx, rx) = mpsc::channel();
        for (key, port) in chunk.iter().cloned() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(measure(key, port));
            });
        }
        drop(tx);
        results.extend(rx);
    }
    results
}

/// Две попытки через прокси сервера, берём лучшую: первая часто дольше из-за рукопожатия.
fn measure(key: String, port: u16) -> ProbeResult {
    let proxy = ureq::Proxy::new(&format!("http://127.0.0.1:{port}")).expect("адрес прокси корректен");
    let agent: ureq::Agent = ureq::Agent::config_builder().proxy(Some(proxy)).timeout_global(Some(TIMEOUT)).build().into();
    let mut best: Option<Duration> = None;
    let mut last_error = None;
    for _ in 0..2 {
        match once(&agent) {
            Ok(t) => best = Some(best.map_or(t, |b| b.min(t))),
            Err(e) => {
                last_error = Some(e);
                // Сервер не ответил вовсе — вторая попытка только потратит время.
                if best.is_none() {
                    break;
                }
            }
        }
    }
    ProbeResult { key, latency: best, error: if best.is_some() { None } else { last_error } }
}

fn once(agent: &ureq::Agent) -> Result<Duration, String> {
    let mut last = String::new();
    for url in TARGETS {
        let started = Instant::now();
        match agent.get(*url).call() {
            Ok(_) => return Ok(started.elapsed()),
            // Молчит — значит, дело в сервере, а не в проверочном сайте: второй сайт не спасёт.
            Err(ureq::Error::Timeout(_)) => return Err(t!("probe.no_answer")),
            Err(e) => last = short_reason(&e.to_string()),
        }
    }
    Err(last)
}

/// Понятная причина вместо системного текста ошибки.
fn short_reason(raw: &str) -> String {
    if raw.contains("10054") || raw.contains("10053") || raw.contains("разорвал") {
        t!("probe.reset")
    } else if raw.contains("10061") {
        t!("probe.refused")
    } else {
        raw.chars().take(80).collect()
    }
}

/// Свободные порты: просим систему выдать случайные и сразу их отпускаем.
fn free_ports(n: usize) -> std::io::Result<Vec<u16>> {
    let listeners = (0..n).map(|_| TcpListener::bind(("127.0.0.1", 0))).collect::<std::io::Result<Vec<_>>>()?;
    listeners.iter().map(|l| l.local_addr().map(|a| a.port())).collect()
}
