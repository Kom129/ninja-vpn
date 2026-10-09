//! Маршрутизация по программам через виртуальную сетевую карту (TUN).
//!
//! Режим «все программы через VPN, кроме этих»: запуском программ с ключом прокси его не
//! сделать — остальные программы запускаем не мы. Поэтому второй sing-box создаёт сетевую
//! карту TUN и забирает себе трафик всего компьютера, а дальше по правилу «какая программа»
//! отправляет его в наш локальный прокси (VPN) или напрямую.
//!
//! Создать сетевую карту может только администратор. Поэтому sing-box с TUN запускает
//! маленький помощник — тот же exe с ключом `--tun-helper`, с разрешения Windows (UAC).
//! Помощник следит: закрылась программа или попросили остановиться (файл `stop`) — гасит TUN.
//! Даже если программа упадёт, сетевая карта не останется висеть.

use std::fs;
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::winsys;

/// Кто идёт через VPN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Split {
    /// Только эти программы через VPN, остальные напрямую.
    Only(Vec<PathBuf>),
    /// Все программы через VPN, кроме этих.
    Except(Vec<PathBuf>),
}

#[derive(Debug, Clone)]
pub struct TunOptions {
    pub split: Split,
    /// Порт нашего локального прокси (ядро с VPN-сервером).
    pub proxy_port: u16,
    /// Программы, которые всегда идут напрямую: наши ядра (иначе их трафик к VPN-серверу
    /// вернулся бы в TUN по кругу).
    pub bypass: Vec<PathBuf>,
    /// Только для проверки: забирать в TUN лишь эти адреса, остальную сеть не трогать.
    pub only_routes: Vec<String>,
    /// Имена VPN-серверов и их адреса, узнанные заранее ([`resolve_servers`]). Ядро ищет свой
    /// сервер по имени при каждом новом подключении к нему; спрашивать через VPN нельзя (VPN
    /// ещё нет — по кругу), поэтому sing-box отвечает ему сам, по этому списку.
    pub servers: Vec<(String, Vec<Ipv4Addr>)>,
    /// Подробный журнал (каждое соединение и DNS-запрос) — только для поиска неполадок.
    pub verbose: bool,
}

/// Имя сетевой карты — его видно в «Сетевых подключениях» Windows.
pub const INTERFACE: &str = "ninja-vpn";

/// Другой VPN на весь компьютер, если он включён: имя его сетевой карты («DurevVPN»).
///
/// С ним наш TUN не уживается (проверено 2026-10-06 с Durev): маршруты «весь интернет»
/// у обеих карт равны, и Windows выбирает чужую; а защита от утечек DNS у каждой запрещает
/// DNS через другую карту — имена сайтов не находятся ни у одного VPN, интернета нет.
pub fn other_vpn() -> Option<String> {
    winsys::internet_adapters().into_iter().find(|a| a.name != INTERFACE && a.looks_like_vpn()).map(|a| a.name)
}

/// Что сказать человеку, если включён другой VPN.
pub fn other_vpn_message(name: &str) -> String {
    crate::t!("tun.other_vpn", name = name)
}

/// Узнать адреса VPN-серверов обычным DNS Windows — пока TUN ещё не включён.
///
/// Почему не спросить DNS роутера уже из-под TUN: защита от утечек DNS (`strict_route`)
/// ставит фильтр Windows «DNS — только через карту TUN, кроме самого sing-box». Самого
/// sing-box фильтр узнаёт по пути к файлу, а Windows записывает путь с русскими буквами
/// не так, как потом сравнивает («Проекты» против «проекты»). Если в пути есть не латинские
/// буквы (папка «Проекты», имя пользователя «Иван»), DNS самого sing-box режется — ядро не
/// находит свой сервер, и интернета нет (проверено 2026-10-07). Заранее узнанный адрес
/// сети не требует.
pub fn resolve_servers(names: &[String]) -> Vec<(String, Vec<Ipv4Addr>)> {
    names
        .iter()
        .map(|name| {
            let mut ips: Vec<Ipv4Addr> = (name.as_str(), 443)
                .to_socket_addrs()
                .map(|list| list.filter_map(|a| if let IpAddr::V4(ip) = a.ip() { Some(ip) } else { None }).collect())
                .unwrap_or_default();
            ips.dedup();
            (name.clone(), ips)
        })
        .collect()
}

fn paths(list: &[PathBuf]) -> Vec<String> {
    list.iter().map(|p| p.display().to_string()).collect()
}

/// Конфиг sing-box с TUN.
pub fn config(o: &TunOptions) -> Value {
    let testing = !o.only_routes.is_empty();
    let mut tun = json!({
        "type": "tun",
        "tag": "tun-in",
        "interface_name": INTERFACE,
        "address": ["172.19.0.1/30"],
        "mtu": 9000,
        "auto_route": true,
        // Windows любит спрашивать DNS через все сетевые карты сразу — строгий режим это запрещает.
        // В проверке выключен: она не должна трогать DNS остального компьютера.
        "strict_route": !testing,
        "stack": "mixed"
    });
    if testing {
        tun["route_address"] = json!(o.only_routes);
    }
    let (listed, other) = match &o.split {
        Split::Only(list) => (list, "direct"),
        Split::Except(list) => (list, "vpn"),
    };
    let listed_to = if other == "vpn" { "direct" } else { "vpn" };
    let mut rules = vec![
        json!({ "action": "sniff" }),
        json!({ "protocol": "dns", "action": "hijack-dns" }),
        // Домашняя сеть (роутер, принтер) — всегда напрямую.
        json!({ "ip_is_private": true, "outbound": "direct" }),
    ];
    if !o.bypass.is_empty() {
        rules.insert(2, json!({ "process_path": paths(&o.bypass), "outbound": "direct" }));
    }
    if !listed.is_empty() {
        rules.push(json!({ "process_path": paths(listed), "outbound": listed_to }));
    }
    // Адреса VPN-серверов: узнанные заранее — отвечает сам sing-box (`servers`),
    // не узнанные (не ответил DNS) — пробуем DNS роутера (`local`).
    let known: serde_json::Map<String, Value> = o
        .servers
        .iter()
        .filter(|(_, ips)| !ips.is_empty())
        .map(|(name, ips)| (name.clone(), json!(ips.iter().map(Ipv4Addr::to_string).collect::<Vec<_>>())))
        .collect();
    let unknown: Vec<&String> = o.servers.iter().filter(|(_, ips)| ips.is_empty()).map(|(name, _)| name).collect();
    let mut dns_servers = vec![
        json!({ "type": "https", "tag": "remote", "server": "1.1.1.1", "detour": "vpn" }),
        json!({ "type": "dhcp", "tag": "local" }),
    ];
    let mut dns_rules: Vec<Value> = Vec::new();
    if !known.is_empty() {
        dns_rules.push(json!({ "domain": known.keys().collect::<Vec<_>>(), "server": "servers" }));
        dns_servers.push(json!({ "type": "hosts", "tag": "servers", "predefined": known }));
    }
    if !unknown.is_empty() {
        dns_rules.push(json!({ "domain": unknown, "server": "local" }));
    }
    json!({
        // В проверке журнал подробный (видно каждое соединение и куда оно пошло); в работе — только ошибки.
        "log": { "level": if o.verbose { "debug" } else if testing { "info" } else { "warn" }, "timestamp": true },
        "dns": {
            // Имена сайтов спрашиваем через VPN (DNS по HTTPS у Cloudflare): провайдер их не видит.
            // `dhcp` — DNS, который выдал роутер (провайдер): только для того, что не узнали заранее.
            "servers": dns_servers,
            "rules": dns_rules,
            "final": "remote",
            "strategy": "ipv4_only"
        },
        "inbounds": [tun],
        "outbounds": [
            { "type": "socks", "tag": "vpn", "server": "127.0.0.1", "server_port": o.proxy_port, "version": "5" },
            { "type": "direct", "tag": "direct" }
        ],
        "route": {
            "auto_detect_interface": true,
            "default_domain_resolver": "local",
            "rules": rules,
            "final": other
        }
    })
}

/// Файлы помощника в рабочей папке: конфиг, журнал ядра, «готово», «стоп».
struct Files {
    config: PathBuf,
    log: PathBuf,
    status: PathBuf,
    stop: PathBuf,
}

fn files(dir: &Path) -> Files {
    Files { config: dir.join("config.json"), log: dir.join("sing-box.log"), status: dir.join("status"), stop: dir.join("stop") }
}

/// Помощник (работает с правами администратора): запустить sing-box с TUN и сторожить.
/// `parent` — номер процесса программы: она закрылась — гасим TUN. Код выхода: 0 — остановили
/// как положено, 1 — sing-box не запустился или упал (причина — в файле status).
pub fn helper_main(sing_box: &Path, dir: &Path, parent: u32) -> i32 {
    // Программу закрыли, пока окно Windows ждало «Да»: TUN уже никому не нужен. Выходим, ничего
    // не трогая — в папке может лежать конфиг уже следующего запуска программы.
    if !winsys::process_alive(parent) {
        return 0;
    }
    let f = files(dir);
    let say = |text: &str| {
        let _ = fs::write(&f.status, text);
    };
    let log = match fs::File::create(&f.log) {
        Ok(l) => l,
        Err(e) => {
            say(&format!("error: {}", crate::t!("tun.helper_log", why = e)));
            return 1;
        }
    };
    let mut cmd = Command::new(sing_box);
    cmd.args(["--disable-color", "run", "-c"]).arg(&f.config).current_dir(dir).stdin(Stdio::null());
    if let Ok(copy) = log.try_clone() {
        cmd.stdout(copy).stderr(log);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // без окна консоли
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            say(&format!("error: {}", crate::t!("tun.helper_start", why = e)));
            return 1;
        }
    };
    crate::engine::attach_to_job(&child);
    // Секунда на подъём сетевой карты; потом конфиг (в нём порт прокси) больше не нужен.
    std::thread::sleep(Duration::from_millis(1500));
    let _ = fs::remove_file(&f.config);
    if let Ok(Some(_)) = child.try_wait() {
        say(&format!("error: {}", crate::t!("tun.helper_exited_now", tail = tail(&f.log))));
        return 1;
    }
    say("started");
    loop {
        std::thread::sleep(Duration::from_millis(300));
        if f.stop.exists() || !winsys::process_alive(parent) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&f.stop);
            say("stopped");
            return 0;
        }
        if let Ok(Some(_)) = child.try_wait() {
            say(&format!("error: {}", crate::t!("tun.helper_exited", tail = tail(&f.log))));
            return 1;
        }
    }
}

fn tail(log: &Path) -> String {
    let text = fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(5)..].join("\n")
}

/// Работающий TUN — со стороны обычной программы (без прав администратора).
pub struct Tun {
    dir: PathBuf,
    helper: winsys::Elevated,
}

impl Tun {
    /// Записать конфиг и запустить помощника. Windows спросит разрешение администратора —
    /// пока человек думает, ждём (до `wait`). `helper` и `args` — что запускать помощником,
    /// `owner` — окно программы (чтобы запрос Windows появился поверх него; 0 — нет окна).
    pub fn start(helper: &Path, args: &str, dir: &Path, config: &Value, wait: Duration, owner: isize) -> Result<Tun, String> {
        fs::create_dir_all(dir).map_err(|e| crate::t!("tun.dir", why = e))?;
        let f = files(dir);
        let _ = fs::remove_file(&f.stop);
        let _ = fs::remove_file(&f.status);
        fs::write(&f.config, serde_json::to_string_pretty(config).expect("JSON всегда сериализуется"))
            .map_err(|e| crate::t!("tun.config", why = e))?;
        let process = winsys::spawn_elevated(helper, args, owner);
        let helper = match process {
            Ok(h) => h,
            Err(e) => {
                let _ = fs::remove_file(&f.config);
                return Err(e);
            }
        };
        let tun = Tun { dir: dir.to_path_buf(), helper };
        let deadline = Instant::now() + wait;
        loop {
            match fs::read_to_string(&f.status).unwrap_or_default().as_str() {
                "started" => return Ok(tun),
                s if s.starts_with("error: ") => return Err(s["error: ".len()..].to_string()),
                _ => {}
            }
            if !tun.helper.is_running() {
                return Err(crate::t!("tun.helper_silent"));
            }
            if Instant::now() > deadline {
                return Err(crate::t!("tun.timeout"));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    pub fn is_running(&self) -> bool {
        self.helper.is_running() && fs::read_to_string(files(&self.dir).status).is_ok_and(|s| s == "started")
    }

    /// Что сломалось, если TUN остановился сам.
    pub fn problem(&self) -> Option<String> {
        let s = fs::read_to_string(files(&self.dir).status).unwrap_or_default();
        s.strip_prefix("error: ").map(str::to_string)
    }

    /// Остановить: помощник видит файл «стоп» и гасит sing-box. Ждём до 5 с.
    pub fn stop(&mut self) {
        let _ = fs::write(files(&self.dir).stop, b"stop");
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.helper.is_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        self.helper.terminate();
    }
}

impl Drop for Tun {
    fn drop(&mut self) {
        if self.helper.is_running() {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(split: Split) -> TunOptions {
        TunOptions {
            split,
            proxy_port: 2080,
            bypass: vec![PathBuf::from(r"C:\ninja\engines\xray.exe")],
            only_routes: vec![],
            servers: vec![("nl.example.com".into(), vec![Ipv4Addr::new(203, 0, 113, 7)]), ("de.example.com".into(), vec![])],
            verbose: false,
        }
    }

    #[test]
    fn except_mode_sends_rest_to_vpn() {
        let c = config(&opts(Split::Except(vec![PathBuf::from(r"C:\Games\game.exe")])));
        assert_eq!(c["route"]["final"], "vpn");
        let rules = c["route"]["rules"].as_array().unwrap();
        let listed = rules.iter().find(|r| r["process_path"][0] == r"C:\Games\game.exe").unwrap();
        assert_eq!(listed["outbound"], "direct");
        // Наши ядра — всегда напрямую, и это правило раньше правила списка.
        let bypass = rules.iter().position(|r| r["process_path"][0] == r"C:\ninja\engines\xray.exe").unwrap();
        let list = rules.iter().position(|r| r["process_path"][0] == r"C:\Games\game.exe").unwrap();
        assert!(bypass < list);
        assert_eq!(c["outbounds"][0]["server_port"], 2080);
        assert_eq!(c["inbounds"][0]["strict_route"], true);
        // Узнанный заранее сервер — отвечает сам sing-box, не узнанный — DNS роутера.
        assert_eq!(c["dns"]["rules"][0]["domain"][0], "nl.example.com");
        assert_eq!(c["dns"]["rules"][0]["server"], "servers");
        let hosts = c["dns"]["servers"].as_array().unwrap().iter().find(|s| s["type"] == "hosts").unwrap();
        assert_eq!(hosts["predefined"]["nl.example.com"][0], "203.0.113.7");
        assert_eq!(c["dns"]["rules"][1]["domain"][0], "de.example.com");
        assert_eq!(c["dns"]["rules"][1]["server"], "local");
        assert!(c["inbounds"][0].get("route_address").is_none());
    }

    /// Настоящее ядро sing-box принимает оба варианта конфига (если ядра скачаны).
    #[test]
    fn real_sing_box_accepts_config() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let Ok(engines) = crate::Engines::load(root) else { return };
        let exe = engines.path(crate::EngineKind::SingBox);
        if !exe.exists() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("ninja-tun-check-{}", std::process::id()));
        for split in [Split::Except(vec![PathBuf::from(r"C:\Games\game.exe")]), Split::Only(vec![])] {
            let mut o = opts(split);
            o.only_routes = vec!["1.1.1.1/32".into()];
            for routes in [vec![], o.only_routes.clone()] {
                o.only_routes = routes;
                let file = crate::engine::ConfigFile::write(&dir, "tun", &config(&o)).unwrap();
                crate::engine::check_config(crate::EngineKind::SingBox, exe, file.path()).unwrap();
            }
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn only_mode_sends_rest_direct() {
        let c = config(&opts(Split::Only(vec![PathBuf::from(r"C:\a.exe")])));
        assert_eq!(c["route"]["final"], "direct");
        let rules = c["route"]["rules"].as_array().unwrap();
        assert!(rules.iter().any(|r| r["process_path"][0] == r"C:\a.exe" && r["outbound"] == "vpn"));
    }

    #[test]
    fn test_config_touches_only_listed_addresses() {
        let mut o = opts(Split::Except(vec![]));
        o.only_routes = vec!["1.1.1.1/32".into()];
        let c = config(&o);
        assert_eq!(c["inbounds"][0]["route_address"][0], "1.1.1.1/32");
        assert_eq!(c["inbounds"][0]["strict_route"], false);
        // Пустой список — правила по программам нет вовсе (пустой process_path ядро не примет).
        assert!(!c["route"]["rules"].as_array().unwrap().iter().any(|r| r["process_path"].as_array().is_some_and(|a| a.is_empty())));
    }
}
