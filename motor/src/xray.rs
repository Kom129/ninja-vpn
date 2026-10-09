//! Сборка конфига Xray из нашей модели профиля.
//!
//! Xray подключаем только там, где sing-box не справляется (сейчас — XHTTP).
//! Режим тот же, что у sing-box: локальный прокси на 127.0.0.1, сеть компьютера не трогаем.

use serde_json::{Value, json};

use crate::profile::{Flow, Profile, Protocol, Security, Transport};

/// Версия ядра, под которую собирается конфиг. Меняется вместе с engines/engines.json.
pub const XRAY_VERSION: &str = "26.3.27";

pub fn local_proxy_config(profile: &Profile, listen_port: u16) -> Value {
    json!({
        // access: none — адреса сайтов, которые ты открываешь, в журнал не пишутся.
        "log": { "loglevel": "warning", "access": "none" },
        "inbounds": [{
            "tag": "local-proxy",
            "listen": "127.0.0.1",
            "port": listen_port,
            // Вход socks в Xray понимает и SOCKS5, и HTTP-прокси (проверено на 26.3.27).
            "protocol": "socks",
            "settings": { "udp": true }
        }],
        "outbounds": [outbound(profile)]
    })
}

/// Конфиг для проверки скорости: у каждого сервера свой вход на 127.0.0.1:порт,
/// правило маршрута ведёт этот вход ровно в его сервер. Один процесс проверяет все сразу.
pub fn probe_config(items: &[(&Profile, u16)]) -> Value {
    let inbounds: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(i, (_, port))| json!({ "tag": format!("in-{i}"), "listen": "127.0.0.1", "port": port, "protocol": "socks", "settings": { "udp": false } }))
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
    let rules: Vec<Value> = (0..items.len())
        .map(|i| json!({ "type": "field", "inboundTag": [format!("in-{i}")], "outboundTag": format!("out-{i}") }))
        .collect();
    json!({
        "log": { "loglevel": "error", "access": "none" },
        "inbounds": inbounds,
        "outbounds": outbounds,
        "routing": { "rules": rules }
    })
}

fn outbound(p: &Profile) -> Value {
    let Protocol::Vless { uuid, flow } = &p.protocol;
    let mut user = json!({ "id": uuid.expose(), "encryption": "none" });
    if *flow == Some(Flow::Vision) {
        user["flow"] = json!("xtls-rprx-vision");
    }
    let mut stream = json!({ "network": network(&p.transport) });
    match &p.security {
        Security::Reality { sni, public_key, short_id, fingerprint } => {
            stream["security"] = json!("reality");
            stream["realitySettings"] = json!({
                "serverName": sni,
                "fingerprint": fingerprint,
                "publicKey": public_key,
                "shortId": short_id
            });
        }
        Security::Tls { sni, alpn, fingerprint } => {
            let mut tls = json!({});
            if let Some(sni) = sni {
                tls["serverName"] = json!(sni);
            }
            if !alpn.is_empty() {
                tls["alpn"] = json!(alpn);
            }
            if let Some(fp) = fingerprint {
                tls["fingerprint"] = json!(fp);
            }
            stream["security"] = json!("tls");
            stream["tlsSettings"] = tls;
        }
    }
    if let Some((key, settings)) = transport_settings(&p.transport) {
        stream[key] = settings;
    }
    json!({
        "tag": "proxy",
        "protocol": "vless",
        "settings": { "vnext": [{ "address": p.server, "port": p.port, "users": [user] }] },
        "streamSettings": stream
    })
}

fn network(transport: &Transport) -> &'static str {
    match transport {
        Transport::Tcp => "raw",
        Transport::Ws { .. } => "ws",
        Transport::Grpc { .. } => "grpc",
        Transport::HttpUpgrade { .. } => "httpupgrade",
        // HTTP/2 всегда идёт через sing-box; если попадёт сюда, Xray сам отклонит конфиг при проверке.
        Transport::H2 { .. } => "http",
        Transport::Xhttp { .. } => "xhttp",
    }
}

fn transport_settings(transport: &Transport) -> Option<(&'static str, Value)> {
    let with_host = |mut v: Value, host: &Option<String>| {
        if let Some(host) = host {
            v["host"] = json!(host);
        }
        v
    };
    Some(match transport {
        Transport::Tcp | Transport::H2 { .. } => return None,
        Transport::Ws { path, host, early_data } => {
            // Xray берёт ранние данные прямо из пути: /ws?ed=2048.
            let path = early_data.map_or_else(|| path.clone(), |ed| format!("{path}?ed={ed}"));
            ("wsSettings", with_host(json!({ "path": path }), host))
        }
        Transport::Grpc { service_name } => ("grpcSettings", json!({ "serviceName": service_name })),
        Transport::HttpUpgrade { path, host } => ("httpupgradeSettings", with_host(json!({ "path": path }), host)),
        Transport::Xhttp { path, host, mode, extra } => {
            let mut x = with_host(json!({ "path": path }), host);
            if let Some(mode) = mode {
                x["mode"] = json!(mode);
            }
            if let Some(extra) = extra {
                x["extra"] = extra.clone();
            }
            ("xhttpSettings", x)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vless::parse_vless;

    const UUID: &str = "11111111-2222-4333-8444-555555555555";

    #[test]
    fn xhttp_reality_outbound() {
        let link = format!(
            "vless://{UUID}@auto.example.com:8443?type=xhttp&security=reality&sni=cdn.example.org\
             &pbk=Z84J2IelR9ch3k8VtlVhhs5ycBUlXA7wHBWcBrjqnAw&sid=de3eb7d4&fp=chrome&mode=stream-one&path=%2Fxhttp"
        );
        let config = local_proxy_config(&parse_vless(&link).unwrap(), 2080);
        let out = &config["outbounds"][0];
        assert_eq!(out["settings"]["vnext"][0]["users"][0]["id"], UUID);
        assert_eq!(out["settings"]["vnext"][0]["port"], 8443);
        let stream = &out["streamSettings"];
        assert_eq!(stream["network"], "xhttp");
        assert_eq!(stream["xhttpSettings"]["mode"], "stream-one");
        assert_eq!(stream["xhttpSettings"]["path"], "/xhttp");
        assert_eq!(stream["realitySettings"]["serverName"], "cdn.example.org");
        assert_eq!(stream["realitySettings"]["shortId"], "de3eb7d4");
        assert_eq!(config["inbounds"][0]["listen"], "127.0.0.1");
    }

    #[test]
    fn websocket_early_data_goes_back_into_path() {
        let link = format!("vless://{UUID}@a.example.com:443?type=ws&security=tls&path=%2Fws%3Fed%3D2048");
        let config = local_proxy_config(&parse_vless(&link).unwrap(), 2080);
        assert_eq!(config["outbounds"][0]["streamSettings"]["wsSettings"]["path"], "/ws?ed=2048");
    }
}
