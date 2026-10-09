//! Разбор ключа `vless://…` в нашу модель [`Profile`].
//!
//! Ключ — недоверенные данные: его мог прислать кто угодно. Поэтому разбираем строго.
//! То, что sing-box не умеет, — понятная ошибка. То, что мы не применили, — предупреждение.
//! Молча выкинуть параметр и подключиться «как получится» нельзя.

use std::collections::BTreeMap;

use percent_encoding::percent_decode_str;
use url::{Host, Url};

use crate::profile::{Flow, Profile, Protocol, Security, Transport};
use crate::secret::Secret;

/// Настоящие ключи короче; ограничение защищает от мусора и «огромных строк».
const MAX_LINK_LEN: usize = 4096;

/// Отпечатки браузера (uTLS), которые знает sing-box.
pub const FINGERPRINTS: &[&str] =
    &["chrome", "firefox", "edge", "safari", "360", "qq", "ios", "android", "random", "randomized"];

/// Параметры, которые мы понимаем. Остальные попадут в предупреждения.
const KNOWN_PARAMS: &[&str] = &[
    "type", "security", "encryption", "flow", "sni", "fp", "pbk", "sid", "spx", "alpn", "path",
    "host", "serviceName", "mode", "headerType", "packetEncoding", "allowInsecure", "insecure", "extra",
];

use crate::t;

/// Режимы XHTTP, которые знает Xray.
const XHTTP_MODES: &[&str] = &["auto", "packet-up", "stream-up", "stream-one"];

/// `extra` — JSON с настройками XHTTP; больше не бывает, ограничиваем на всякий случай.
const MAX_EXTRA_LEN: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    #[error("{}", t!("vless.not_vless"))]
    NotVless,
    #[error("{}", t!("vless.unsupported_protocol", name = .0))]
    UnsupportedProtocol(String),
    #[error("{}", t!("vless.malformed", why = .0))]
    Malformed(String),
    #[error("{}", t!("vless.unsupported", what = .what, why = .why))]
    Unsupported { what: String, why: String },
    #[error("{}", t!("vless.insecure", why = .0))]
    Insecure(String),
}

pub(crate) fn malformed(msg: impl Into<String>) -> ImportError {
    ImportError::Malformed(msg.into())
}

pub(crate) fn unsupported(what: impl Into<String>, why: impl Into<String>) -> ImportError {
    ImportError::Unsupported { what: what.into(), why: why.into() }
}

pub fn parse_vless(link: &str) -> Result<Profile, ImportError> {
    let link = link.trim();
    if link.len() > MAX_LINK_LEN {
        return Err(malformed(t!("vless.too_long")));
    }
    if !link.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("vless://")) {
        return Err(ImportError::NotVless);
    }
    let url = Url::parse(link).map_err(|e| malformed(e.to_string()))?;

    let uuid = decode(url.username());
    if !is_uuid(&uuid) {
        return Err(malformed(t!("vless.bad_uuid")));
    }
    let server = match url.host() {
        Some(Host::Domain(d)) if is_hostname(d) => d.to_ascii_lowercase(),
        Some(Host::Ipv4(ip)) => ip.to_string(),
        Some(Host::Ipv6(ip)) => ip.to_string(),
        _ => return Err(malformed(t!("vless.bad_server"))),
    };
    let port = url.port().filter(|p| *p != 0).ok_or_else(|| malformed(t!("vless.no_port")))?;
    let params = Params::parse(url.query().unwrap_or(""))?;
    let mut warnings = Vec::new();

    // VLESS Encryption — новое собственное шифрование VLESS в Xray.
    if params.get("encryption").is_some_and(|e| e != "none") {
        return Err(unsupported("VLESS Encryption", t!("vless.encryption_why")));
    }

    let transport = parse_transport(&params, &mut warnings)?;
    let security = parse_security(&params, &mut warnings)?;
    let flow = match params.get("flow") {
        None => None,
        Some("xtls-rprx-vision") if transport == Transport::Tcp => Some(Flow::Vision),
        Some("xtls-rprx-vision") => {
            return Err(unsupported(t!("vless.vision_over", transport = transport.label()), t!("vless.vision_tcp_only")));
        }
        Some(other) => return Err(unsupported(t!("vless.flow_mode", flow = other), t!("vless.only_vision"))),
    };

    if let Some(encoding) = params.get("packetEncoding").filter(|e| *e != "xudp") {
        warnings.push(t!("vless.packet_encoding", encoding = encoding));
    }
    if params.get("spx").is_some() {
        warnings.push(t!("vless.spx"));
    }
    for key in params.0.keys().filter(|k| !KNOWN_PARAMS.contains(&k.as_str())) {
        warnings.push(t!("vless.unknown_param", key = key));
    }

    let name = url
        .fragment()
        .map(|f| clean_name(&decode(f)))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("{server}:{port}"));

    Ok(Profile {
        name,
        server,
        port,
        protocol: Protocol::Vless { uuid: Secret::new(uuid), flow },
        transport,
        security,
        warnings,
    })
}

fn parse_transport(p: &Params, warnings: &mut Vec<String>) -> Result<Transport, ImportError> {
    let kind = p.get("type").unwrap_or("tcp").to_ascii_lowercase();
    let host = p.get("host").map(String::from);
    let path = p.get("path").unwrap_or("/").to_string();
    Ok(match kind.as_str() {
        "tcp" | "raw" => match p.get("headerType") {
            None | Some("none") => Transport::Tcp,
            Some(h) => return Err(unsupported(t!("vless.tcp_header", header = h), t!("vless.singbox_cannot_it"))),
        },
        "ws" => {
            let (path, early_data) = split_early_data(&path);
            Transport::Ws { path, host, early_data }
        }
        "grpc" => {
            if p.get("mode") == Some("multi") {
                warnings.push(t!("vless.grpc_multi"));
            }
            Transport::Grpc { service_name: p.get("serviceName").unwrap_or("").to_string() }
        }
        "httpupgrade" => Transport::HttpUpgrade { path, host },
        "http" | "h2" => Transport::H2 { path, host },
        "xhttp" | "splithttp" => {
            let mode = p.get("mode").map(str::to_ascii_lowercase);
            if let Some(m) = &mode
                && !XHTTP_MODES.contains(&m.as_str())
            {
                return Err(unsupported(t!("vless.xhttp_mode", mode = m), t!("vless.xhttp_modes")));
            }
            Transport::Xhttp { path, host, mode, extra: parse_extra(p.get("extra"))? }
        }
        other => return Err(unsupported(t!("vless.transport", name = other), t!("vless.singbox_cannot"))),
    })
}

fn parse_security(p: &Params, warnings: &mut Vec<String>) -> Result<Security, ImportError> {
    let fingerprint = match p.get("fp").map(str::to_ascii_lowercase) {
        Some(fp) if !FINGERPRINTS.contains(&fp.as_str()) => {
            return Err(unsupported(t!("vless.fingerprint", fp = fp), t!("vless.singbox_unknown")));
        }
        fp => fp,
    };
    match p.get("security").unwrap_or("none").to_ascii_lowercase().as_str() {
        "reality" => {
            let sni = p.get("sni").ok_or_else(|| malformed(t!("vless.reality_sni")))?;
            let public_key = p.get("pbk").ok_or_else(|| malformed(t!("vless.reality_pbk")))?;
            if !is_reality_public_key(public_key) {
                return Err(malformed(t!("vless.reality_pbk_format")));
            }
            let short_id = p.get("sid").unwrap_or("");
            if !is_short_id(short_id) {
                return Err(malformed(t!("vless.reality_sid_format")));
            }
            let fingerprint = fingerprint.unwrap_or_else(|| {
                warnings.push(t!("vless.no_fingerprint"));
                "chrome".into()
            });
            Ok(Security::Reality { sni: sni.into(), public_key: public_key.into(), short_id: short_id.into(), fingerprint })
        }
        "tls" => {
            if matches!(p.get("allowInsecure").or(p.get("insecure")), Some("1" | "true")) {
                return Err(ImportError::Insecure(
                    t!("vless.allow_insecure"),
                ));
            }
            let alpn = p
                .get("alpn")
                .map(|a| a.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect())
                .unwrap_or_default();
            Ok(Security::Tls { sni: p.get("sni").map(String::from), alpn, fingerprint })
        }
        "none" => Err(ImportError::Insecure(t!("vless.no_tls"))),
        other => Err(unsupported(t!("vless.security", security = other), t!("vless.unknown_kind"))),
    }
}

/// Параметры после `?` в ключе. Разбираем сами, а не как HTML-форму:
/// там `+` превращается в пробел и испортил бы пути вроде `/a+b`.
struct Params(BTreeMap<String, String>);

impl Params {
    fn parse(query: &str) -> Result<Self, ImportError> {
        let mut map = BTreeMap::new();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let key = decode(key);
            if map.insert(key.clone(), decode(value)).is_some() {
                return Err(malformed(t!("vless.param_twice", key = key)));
            }
        }
        Ok(Self(map))
    }

    /// Значение параметра; пустое значение считаем отсутствующим.
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str).filter(|v| !v.is_empty())
    }
}

/// Дополнительные настройки XHTTP: должны быть JSON-объектом разумного размера.
/// Выкинуть их нельзя — без них подключение может работать иначе, чем задумал сервер.
fn parse_extra(raw: Option<&str>) -> Result<Option<serde_json::Value>, ImportError> {
    let Some(raw) = raw else { return Ok(None) };
    if raw.len() > MAX_EXTRA_LEN {
        return Err(malformed(t!("vless.extra_too_big")));
    }
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) if value.is_object() => Ok(Some(value)),
        _ => Err(malformed(t!("vless.extra_not_object"))),
    }
}

/// `/ws?ed=2048` → путь `/ws` и 2048 байт ранних данных (так их записывают Xray-клиенты).
fn split_early_data(path: &str) -> (String, Option<u32>) {
    if let Some((base, query)) = path.split_once('?')
        && let Some(ed) = query.split('&').find_map(|kv| kv.strip_prefix("ed=")).and_then(|v| v.parse().ok())
    {
        return (base.to_string(), Some(ed));
    }
    (path.to_string(), None)
}

pub(crate) fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// Имя для экрана: без управляющих символов и не длиннее 64 знаков.
pub(crate) fn clean_name(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(64).collect::<String>().trim().to_string()
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
}

fn is_hostname(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && !s.starts_with(['-', '.'])
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

/// Публичный ключ X25519 в base64url без «=»: 32 байта = 43 символа.
fn is_reality_public_key(s: &str) -> bool {
    s.len() == 43 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Short id: до 16 шестнадцатеричных символов, чётное количество (может быть пустым).
fn is_short_id(s: &str) -> bool {
    s.len() <= 16 && s.len().is_multiple_of(2) && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Выдуманные значения правильного формата — не настоящий сервер.
    const UUID: &str = "11111111-2222-4333-8444-555555555555";
    const PBK: &str = "Z84J2IelR9ch3k8VtlVhhs5ycBUlXA7wHBWcBrjqnAw";

    fn reality_link() -> String {
        format!(
            "vless://{UUID}@NL.Example.com:443?type=tcp&security=reality&pbk={PBK}&sid=6ba85179e30d4fc2\
             &sni=www.microsoft.com&fp=chrome&flow=xtls-rprx-vision&spx=%2F\
             #%D0%9D%D0%B8%D0%B4%D0%B5%D1%80%D0%BB%D0%B0%D0%BD%D0%B4%D1%8B"
        )
    }

    fn tls_link(query: &str) -> String {
        format!("vless://{UUID}@a.example.com:443?security=tls&sni=a.example.com&{query}#test")
    }

    #[test]
    fn reality_vision() {
        let p = parse_vless(&reality_link()).unwrap();
        assert_eq!(p.name, "Нидерланды");
        assert_eq!(p.server, "nl.example.com");
        assert_eq!(p.port, 443);
        assert_eq!(p.summary(), "VLESS · TCP · REALITY · Vision");
        assert_eq!(
            p.security,
            Security::Reality {
                sni: "www.microsoft.com".into(),
                public_key: PBK.into(),
                short_id: "6ba85179e30d4fc2".into(),
                fingerprint: "chrome".into(),
            }
        );
        assert_eq!(p.warnings, vec!["spx (spiderX) пока не применяется"]);
        assert_eq!(p.engine(), crate::engine::EngineKind::SingBox);
    }

    #[test]
    fn secret_is_hidden_when_printed() {
        let p = parse_vless(&reality_link()).unwrap();
        assert!(!format!("{p:?}").contains(UUID));
    }

    #[test]
    fn websocket_tls_ipv6_early_data() {
        let link = format!(
            "vless://{UUID}@[2001:db8::1]:8443?type=ws&security=tls&path=%2Fws%3Fed%3D2048\
             &host=cdn.example.com&sni=cdn.example.com&alpn=http%2F1.1&fp=Firefox#ws"
        );
        let p = parse_vless(&link).unwrap();
        assert_eq!(p.server, "2001:db8::1");
        assert_eq!(
            p.transport,
            Transport::Ws { path: "/ws".into(), host: Some("cdn.example.com".into()), early_data: Some(2048) }
        );
        assert_eq!(
            p.security,
            Security::Tls {
                sni: Some("cdn.example.com".into()),
                alpn: vec!["http/1.1".into()],
                fingerprint: Some("firefox".into()),
            }
        );
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    }

    #[test]
    fn grpc_multi_mode_is_a_warning() {
        let p = parse_vless(&tls_link("type=grpc&serviceName=svc&mode=multi")).unwrap();
        assert_eq!(p.transport, Transport::Grpc { service_name: "svc".into() });
        assert_eq!(p.warnings.len(), 1);
    }

    #[test]
    fn unknown_parameter_is_a_warning_not_silence() {
        let p = parse_vless(&tls_link("pqv=abc")).unwrap();
        assert_eq!(p.warnings, vec!["параметр «pqv» не поддерживается и не применён"]);
    }

    #[test]
    fn xhttp_reality_goes_to_xray() {
        // Как основные серверы Durev: XHTTP + REALITY + свои параметры сервиса.
        let link = format!(
            "vless://{UUID}@auto.example.com:8443?type=xhttp&security=reality&sni=cdn.example.org&pbk={PBK}\
             &sid=de3eb7d4&fp=chrome&mode=stream-one&path=%2Fxhttp&concurrency=4#Auto"
        );
        let p = parse_vless(&link).unwrap();
        assert_eq!(p.summary(), "VLESS · XHTTP · REALITY");
        assert_eq!(p.engine(), crate::engine::EngineKind::Xray);
        assert_eq!(
            p.transport,
            Transport::Xhttp { path: "/xhttp".into(), host: None, mode: Some("stream-one".into()), extra: None }
        );
        assert_eq!(p.warnings, vec!["параметр «concurrency» не поддерживается и не применён"]);
    }

    #[test]
    fn xhttp_extra_must_be_a_json_object() {
        let ok = parse_vless(&tls_link("type=xhttp&extra=%7B%22xPaddingBytes%22%3A%22100-1000%22%7D")).unwrap();
        assert!(matches!(ok.transport, Transport::Xhttp { extra: Some(ref e), .. } if e["xPaddingBytes"] == "100-1000"));
        assert!(matches!(parse_vless(&tls_link("type=xhttp&extra=oops")), Err(ImportError::Malformed(_))));
        assert!(matches!(parse_vless(&tls_link("type=xhttp&mode=turbo")), Err(ImportError::Unsupported { .. })));
    }

    #[test]
    fn rejects_vless_encryption() {
        let err = parse_vless(&tls_link("encryption=mlkem768x25519plus.native.0rtt.abc")).unwrap_err();
        assert!(matches!(err, ImportError::Unsupported { .. }), "{err}");
    }

    #[test]
    fn rejects_vision_over_websocket() {
        let err = parse_vless(&tls_link("type=ws&flow=xtls-rprx-vision")).unwrap_err();
        assert!(err.to_string().contains("только с TCP"), "{err}");
    }

    #[test]
    fn rejects_unprotected_and_insecure() {
        let none = format!("vless://{UUID}@a.example.com:443?security=none");
        assert!(matches!(parse_vless(&none), Err(ImportError::Insecure(_))));
        assert!(matches!(parse_vless(&tls_link("allowInsecure=1")), Err(ImportError::Insecure(_))));
    }

    #[test]
    fn rejects_broken_keys() {
        let cases = [
            "vless://not-a-uuid@a.example.com:443?security=tls".to_string(),
            format!("vless://{UUID}@a.example.com?security=tls"),
            format!("vless://{UUID}@a.example.com:443?security=reality&sni=x.com&pbk=short"),
            tls_link("sni=b.example.com"),
        ];
        for link in cases {
            assert!(matches!(parse_vless(&link), Err(ImportError::Malformed(_))), "{link}");
        }
    }

    #[test]
    fn rejects_other_schemes() {
        assert_eq!(parse_vless("vmess://eyJ2IjoyfQ==").unwrap_err(), ImportError::NotVless);
    }
}
