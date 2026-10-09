//! Сверяем наши конфиги с настоящими ядрами: `sing-box check` и `xray run -test`
//! проверяют конфиг без выхода в сеть. Если ядра ещё не скачаны (scripts\fetch-engines.ps1),
//! тест пишет об этом и пропускается.

use std::path::{Path, PathBuf};

use ninja_motor::{EngineKind, Engines, engine, singbox, vless, xray};

const UUID: &str = "11111111-2222-4333-8444-555555555555";
const PBK: &str = "Z84J2IelR9ch3k8VtlVhhs5ycBUlXA7wHBWcBrjqnAw";

fn engines() -> Option<Engines> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let engines = Engines::load(&root).ok()?;
    let all_present = [EngineKind::SingBox, EngineKind::Xray].iter().all(|k| engines.path(*k).exists());
    all_present.then_some(engines)
}

fn link(query: &str) -> String {
    format!("vless://{UUID}@a.example.com:443?{query}#test")
}

/// Все варианты, которые мотор умеет разбирать.
fn variants() -> Vec<String> {
    vec![
        link(&format!("security=reality&pbk={PBK}&sid=6ba85179e30d4fc2&sni=www.microsoft.com&fp=chrome&flow=xtls-rprx-vision")),
        format!("vless://{UUID}@[2001:db8::1]:443?security=reality&pbk={PBK}&sni=www.microsoft.com#reality-ipv6-no-sid"),
        link("security=tls&sni=a.example.com&fp=firefox"),
        link("type=ws&security=tls&path=%2Fws%3Fed%3D2048&host=cdn.example.com&alpn=http%2F1.1"),
        link("type=grpc&security=tls&serviceName=svc"),
        link("type=httpupgrade&security=tls&path=%2Fup&host=a.example.com"),
        link("type=http&security=tls&path=%2Fh2&host=a.example.com"),
        link(&format!("type=xhttp&security=reality&sni=cdn.example.org&pbk={PBK}&sid=de3eb7d4&fp=chrome&mode=stream-one&path=%2Fxhttp")),
        link("type=xhttp&security=tls&sni=a.example.com&host=a.example.com&mode=packet-up&path=%2Fx&extra=%7B%22xPaddingBytes%22%3A%22100-1000%22%7D"),
    ]
}

fn check(kind: EngineKind, engines: &Engines, dir: &Path, i: usize, config: serde_json::Value, what: &str) {
    let path = dir.join(format!("{}-{i}.json", kind.name()));
    std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
    if let Err(e) = engine::check_config(kind, engines.path(kind), &path) {
        panic!("{} отклонил конфиг «{what}»: {e}", kind.label());
    }
}

#[test]
fn real_engines_accept_our_configs() {
    let Some(engines) = engines() else {
        eprintln!("ядра не скачаны — тест пропущен (это НЕ значит, что он пройден)");
        return;
    };
    let dir = std::env::temp_dir().join(format!("ninja-motor-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (i, link) in variants().iter().enumerate() {
        let profile = vless::parse_vless(link).unwrap_or_else(|e| panic!("{link}: {e}"));
        let what = profile.summary();
        // Ядро, которое выберет мотор, обязано принять конфиг.
        match profile.engine() {
            EngineKind::SingBox => check(EngineKind::SingBox, &engines, &dir, i, singbox::local_proxy_config(&profile, 2080), &what),
            EngineKind::Xray => check(EngineKind::Xray, &engines, &dir, i, xray::local_proxy_config(&profile, 2080), &what),
        }
        // Xray как запасное ядро должен понимать и остальные варианты (кроме HTTP/2 — его в Xray больше нет).
        if !what.contains("HTTP/2") {
            check(EngineKind::Xray, &engines, &dir, i, xray::local_proxy_config(&profile, 2080), &what);
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn real_engines_accept_probe_configs() {
    let Some(engines) = engines() else { return };
    let profiles: Vec<_> = variants().iter().map(|l| vless::parse_vless(l).unwrap()).collect();
    let dir = std::env::temp_dir().join(format!("ninja-probe-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for kind in [EngineKind::SingBox, EngineKind::Xray] {
        let pairs: Vec<_> = profiles.iter().filter(|p| p.engine() == kind).zip(20000u16..).collect();
        let config = match kind {
            EngineKind::SingBox => singbox::probe_config(&pairs),
            EngineKind::Xray => xray::probe_config(&pairs),
        };
        check(kind, &engines, &dir, 99, config, "проверка скорости");
    }
    std::fs::remove_dir_all(&dir).ok();
}
