//! Сборка конфига sing-box из нашей модели профиля.
//!
//! Режим пока один — локальный прокси: sing-box слушает только 127.0.0.1
//! (HTTP и SOCKS5 на одном порту) и отправляет всё через профиль.
//! Сеть компьютера — маршруты, DNS, адаптеры — не трогаем.

use serde_json::{Value, json};

use crate::profile::{Flow, Profile, Protocol, Security, Transport};

/// Версия ядра, под которую собирается конфиг. Меняется вместе с engines/engines.json.
pub const SING_BOX_VERSION: &str = "1.14.2";

pub fn local_proxy_config(profile: &Profile, listen_port: u16) -> Value {
    json!({
        // warn: в журнал ядра не попадают адреса сайтов, которые ты открываешь.
        "log": { "level": "warn", "timestamp": true },
        // Обычный DNS Windows нужен только чтобы найти IP самого VPN-сервера.
        // Адреса сайтов из запросов браузера разрешает уже VPN-сервер на своей стороне.
        "dns": { "servers": [{ "type": "local", "tag": "local" }] },
        "inbounds": [{
            "type": "mixed",
            "tag": "local-proxy",
            "listen": "127.0.0.1",
            "listen_port": listen_port
        }],
        "outbounds": [outbound(profile)],
        "route": { "final": "proxy", "default_domain_resolver": "local" }
    })
}

/// Конфиг для проверки скорости: у каждого сервера свой вход на 127.0.0.1:порт,
/// правило маршрута ведёт этот вход ровно в его сервер. Один процесс проверяет все сразу.
pub fn probe_config(items: &[(&Profile, u16)]) -> Value {
    let inbounds: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(i, (_, port))| json!({ "type": "mixed", "tag": format!("in-{i}"), "listen": "127.0.0.1", "listen_port": port }))
        .collect();
    let outbounds: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(i, (p, _))| {
            let mut out = outbound(p);
            out["tag"] = json!(format!("out-{i}"));
            out
        })
        .collect();
    let rules: Vec<Value> = (0..items.len()).map(|i| json!({ "inbound": [format!("in-{i}")], "outbound": format!("out-{i}") })).collect();
    json!({
        "log": { "level": "error", "timestamp": true },
        "dns": { "servers": [{ "type": "local", "tag": "local" }] },
        "inbounds": inbounds,
        "outbounds": outbounds,
        "route": { "rules": rules, "final": "out-0", "default_domain_resolver": "local" }
    })
}

fn outbound(p: &Profile) -> Value {
    let Protocol::Vless { uuid, flow } = &p.protocol;
    let mut out = json!({
        "type": "vless",
        "tag": "proxy",
        "server": p.server,
        "server_port": p.port,
        "uuid": uuid.expose(),
        "packet_encoding": "xudp",
        "tls": tls(&p.security)
    });
    if *flow == Some(Flow::Vision) {
        out["flow"] = json!("xtls-rprx-vision");
    }
    if let Some(transport) = transport(&p.transport) {
        out["transport"] = transport;
    }
    out
}

fn tls(security: &Security) -> Value {
    match security {
        Security::Reality { sni, public_key, short_id, fingerprint } => json!({
            "enabled": true,
            "server_name": sni,
            "utls": { "enabled": true, "fingerprint": fingerprint },
            "reality": { "enabled": true, "public_key": public_key, "short_id": short_id }
        }),
        Security::Tls { sni, alpn, fingerprint } => {
            let mut tls = json!({ "enabled": true });
            if let Some(sni) = sni {
                tls["server_name"] = json!(sni);
            }
            if !alpn.is_empty() {
                tls["alpn"] = json!(alpn);
            }
            if let Some(fp) = fingerprint {
                tls["utls"] = json!({ "enabled": true, "fingerprint": fp });
            }
            tls
        }
    }
}

fn transport(transport: &Transport) -> Option<Value> {
    Some(match transport {
        Transport::Tcp => return None,
        Transport::Ws { path, host, early_data } => {
            let mut ws = json!({ "type": "ws", "path": path });
            if let Some(host) = host {
                ws["headers"] = json!({ "Host": host });
            }
            if let Some(bytes) = early_data {
                ws["max_early_data"] = json!(bytes);
                ws["early_data_header_name"] = json!("Sec-WebSocket-Protocol");
            }
            ws
        }
        Transport::Grpc { service_name } => json!({ "type": "grpc", "service_name": service_name }),
        Transport::HttpUpgrade { path, host } => {
            let mut upgrade = json!({ "type": "httpupgrade", "path": path });
            if let Some(host) = host {
                upgrade["host"] = json!(host);
            }
            upgrade
        }
        Transport::H2 { path, host } => {
            let mut h2 = json!({ "type": "http", "path": path });
            if let Some(host) = host {
                h2["host"] = json!([host]);
            }
            h2
        }
        // XHTTP всегда идёт через Xray; если попадёт сюда, sing-box сам отклонит конфиг при проверке.
        Transport::Xhttp { .. } => json!({ "type": "xhttp" }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vless::parse_vless;

    const UUID: &str = "11111111-2222-4333-8444-555555555555";

    #[test]
    fn reality_vision_outbound() {
        let link = format!(
            "vless://{UUID}@nl.example.com:443?security=reality&pbk=Z84J2IelR9ch3k8VtlVhhs5ycBUlXA7wHBWcBrjqnAw\
             &sid=ab&sni=www.microsoft.com&fp=chrome&flow=xtls-rprx-vision"
        );
        let config = local_proxy_config(&parse_vless(&link).unwrap(), 2080);
        let out = &config["outbounds"][0];
        assert_eq!(out["uuid"], UUID);
        assert_eq!(out["flow"], "xtls-rprx-vision");
        assert_eq!(out["tls"]["reality"]["short_id"], "ab");
        assert_eq!(out["tls"]["utls"]["fingerprint"], "chrome");
        assert!(out.get("transport").is_none());
        assert_eq!(config["inbounds"][0]["listen"], "127.0.0.1");
    }

    #[test]
    fn websocket_outbound() {
        let link = format!("vless://{UUID}@a.example.com:443?type=ws&security=tls&path=%2Fws%3Fed%3D2048&host=cdn.example.com");
        let out = local_proxy_config(&parse_vless(&link).unwrap(), 2080)["outbounds"][0].clone();
        assert_eq!(out["transport"]["path"], "/ws");
        assert_eq!(out["transport"]["headers"]["Host"], "cdn.example.com");
        assert_eq!(out["transport"]["max_early_data"], 2048);
        assert!(out.get("flow").is_none());
    }
}
