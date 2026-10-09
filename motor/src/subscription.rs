//! Подписка: ссылка, по которой VPN-сервис отдаёт список ключей.
//!
//! Ответ сервера — недоверенные данные. Ограничиваем размер и число строк,
//! каждую строку разбираем отдельно, а неподходящие показываем с причиной, но не применяем.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD_PAD_INDIFFERENT, URL_SAFE_PAD_INDIFFERENT};

use crate::profile::Profile;
use crate::secret::Secret;
use crate::vless::{self, ImportError, malformed, unsupported};

pub const MAX_BODY_BYTES: u64 = 1024 * 1024;
pub const MAX_ENTRIES: usize = 500;

/// Одна строка подписки: готовый профиль или причина, почему он не подходит.
#[derive(Debug)]
pub struct Entry {
    pub label: String,
    pub result: Result<Profile, ImportError>,
}

/// Сведения о тарифе из заголовка `subscription-userinfo` (если сервис их прислал).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub upload: u64,
    pub download: u64,
    /// 0 — без лимита.
    pub total: u64,
    /// Дата окончания в секундах Unix.
    pub expire: Option<u64>,
}

#[derive(Debug)]
pub struct Fetched {
    pub body: String,
    pub title: Option<String>,
    pub usage: Option<Usage>,
}

use crate::t;

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("{}", t!("sub.timeout"))]
    Timeout,
    #[error("{}", t!("sub.host_not_found"))]
    HostNotFound,
    #[error("{}", t!("sub.status", code = .0))]
    Status(u16),
    #[error("{}", t!("sub.too_large"))]
    TooLarge,
    #[error("{}", t!("sub.other", why = .0))]
    Other(String),
}

/// Скачать подписку напрямую (без VPN).
pub fn fetch(url: &Secret) -> Result<Fetched, FetchError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .proxy(None)
        // Только https, в том числе после перенаправлений: по обычному http список серверов
        // могли бы подменить по дороге.
        .https_only(true)
        .build()
        .into();
    let to_error = |e: ureq::Error| match e {
        ureq::Error::Timeout(_) => FetchError::Timeout,
        ureq::Error::HostNotFound => FetchError::HostNotFound,
        ureq::Error::StatusCode(code) => FetchError::Status(code),
        ureq::Error::BodyExceedsLimit(_) => FetchError::TooLarge,
        // Текст ошибки может содержать саму ссылку — а это секрет.
        other => FetchError::Other(other.to_string().replace(url.expose(), "***")),
    };
    let mut response = agent
        .get(url.expose())
        .header("User-Agent", concat!("ninja-vpn/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(to_error)?;

    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    let title = header("profile-title").map(|t| decode_title(&t));
    let usage = header("subscription-userinfo").map(|u| parse_usage(&u));
    let body = response.body_mut().with_config().limit(MAX_BODY_BYTES).read_to_string().map_err(to_error)?;
    Ok(Fetched { body, title, usage })
}

/// Разобрать тело подписки: обычный список ссылок или он же в base64.
pub fn parse(body: &str) -> Result<Vec<Entry>, ImportError> {
    let text = decode_body(body)?;
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.len() > MAX_ENTRIES {
        return Err(malformed(t!("sub.too_many_lines", max = MAX_ENTRIES)));
    }
    Ok(lines.iter().enumerate().map(|(i, line)| parse_line(line, i)).collect())
}

fn parse_line(line: &str, index: usize) -> Entry {
    let result = match line.split_once("://") {
        Some((scheme, _)) if scheme.eq_ignore_ascii_case("vless") => {
            if is_placeholder(line) {
                Err(malformed(t!("sub.placeholder")))
            } else {
                vless::parse_vless(line)
            }
        }
        Some((scheme, _)) => Err(ImportError::UnsupportedProtocol(scheme.chars().take(16).collect())),
        None => Err(malformed(t!("sub.not_a_key"))),
    };
    let label = match &result {
        Ok(profile) => profile.name.clone(),
        Err(_) => line
            .rsplit_once('#')
            .map(|(_, name)| vless::clean_name(&vless::decode(name)))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| t!("sub.line", n = index + 1)),
    };
    Entry { label, result }
}

/// Некоторые сервисы кладут в подписку информационные строки («осталось 10 дней»)
/// с адресом 0.0.0.0. Это не сервер, подключаться к нему бессмысленно.
fn is_placeholder(line: &str) -> bool {
    url::Url::parse(line).is_ok_and(|u| {
        matches!(u.host_str(), Some("0.0.0.0" | "127.0.0.1" | "[::]" | "[::1]")) || u.port().is_some_and(|p| p <= 1)
    })
}

fn decode_body(body: &str) -> Result<String, ImportError> {
    let body = body.trim_start_matches('\u{feff}').trim();
    if body.starts_with('<') {
        return Err(malformed(t!("sub.html")));
    }
    if body.starts_with('{') || body.starts_with('[') {
        return Err(unsupported(t!("sub.json_what"), t!("sub.json_why")));
    }
    if body.contains("://") {
        return Ok(body.to_string());
    }
    let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = STANDARD_PAD_INDIFFERENT
        .decode(&compact)
        .or_else(|_| URL_SAFE_PAD_INDIFFERENT.decode(&compact))
        .map_err(|_| malformed(t!("sub.not_base64")))?;
    let text = String::from_utf8(bytes).map_err(|_| malformed(t!("sub.not_text")))?;
    if !text.contains("://") {
        return Err(malformed(t!("sub.no_keys")));
    }
    Ok(text)
}

/// Заголовок `profile-title` бывает в виде `base64:…`.
fn decode_title(raw: &str) -> String {
    let text = raw
        .strip_prefix("base64:")
        .and_then(|b| STANDARD_PAD_INDIFFERENT.decode(b).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| raw.to_string());
    vless::clean_name(&text)
}

/// `upload=1; download=2; total=3; expire=1700000000`. Непонятные части пропускаем.
fn parse_usage(header: &str) -> Usage {
    let mut usage = Usage::default();
    for part in header.split(';') {
        let Some((key, value)) = part.trim().split_once('=') else { continue };
        let Ok(value) = value.trim().parse::<u64>() else { continue };
        match key.trim() {
            "upload" => usage.upload = value,
            "download" => usage.download = value,
            "total" => usage.total = value,
            "expire" => usage.expire = (value > 0).then_some(value),
            _ => {}
        }
    }
    usage
}

impl Usage {
    /// Дата окончания как `2026-11-01` (по UTC).
    pub fn expire_date(&self) -> Option<String> {
        self.expire.map(date_from_unix)
    }
}

/// Перевод секунд Unix в календарную дату (алгоритм Говарда Хиннанта, без сторонних библиотек).
fn date_from_unix(secs: u64) -> String {
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "11111111-2222-4333-8444-555555555555";

    fn sample() -> String {
        [
            format!("vless://{UUID}@nl.example.com:443?security=tls&sni=nl.example.com#NL"),
            "vmess://eyJ2IjoyfQ==".to_string(),
            format!("vless://{UUID}@de.example.com:443?type=xhttp&security=tls#DE"),
            format!("vless://{UUID}@0.0.0.0:1?security=none#%D0%9E%D1%81%D1%82%D0%B0%D0%BB%D0%BE%D1%81%D1%8C%2010%20%D0%B4%D0%BD%D0%B5%D0%B9"),
        ]
        .join("\n")
    }

    #[test]
    fn base64_subscription_is_sorted_into_ok_and_rejected() {
        let entries = parse(&STANDARD_PAD_INDIFFERENT.encode(sample())).unwrap();
        assert_eq!(entries.len(), 4);
        assert!(entries[0].result.is_ok());
        assert_eq!(entries[1].result.as_ref().unwrap_err(), &ImportError::UnsupportedProtocol("vmess".into()));
        assert_eq!(entries[2].label, "DE");
        assert!(entries[2].result.is_ok(), "XHTTP теперь подключается через Xray");
        assert_eq!(entries[3].label, "Осталось 10 дней");
        assert_eq!(entries[3].result.as_ref().unwrap_err(), &malformed("служебная строка подписки, а не сервер"));
    }

    #[test]
    fn plain_text_subscription() {
        assert_eq!(parse(&sample()).unwrap().len(), 4);
    }

    #[test]
    fn html_page_is_rejected() {
        assert!(parse("<!doctype html><a href=\"https://x\">").is_err());
    }

    #[test]
    fn usage_header() {
        let u = parse_usage("upload=10; download=20; total=0; expire=1793491200; junk");
        assert_eq!(u, Usage { upload: 10, download: 20, total: 0, expire: Some(1_793_491_200) });
        assert_eq!(u.expire_date().as_deref(), Some("2026-11-01"));
    }

    #[test]
    fn title_in_base64() {
        assert_eq!(decode_title("base64:TXkgVlBO"), "My VPN");
        assert_eq!(decode_title("Plain"), "Plain");
    }

    #[test]
    fn unix_dates() {
        assert_eq!(date_from_unix(0), "1970-01-01");
        assert_eq!(date_from_unix(951_782_400), "2000-02-29");
    }
}
