//! Профиль подключения: куда и как подключаться.
//!
//! Это наша собственная модель, а не копия конфига ядра. Из неё собирается конфиг
//! sing-box (`singbox.rs`) или Xray (`xray.rs`). Протокол, транспорт
//! и защита хранятся отдельно: «VLESS» само по себе ещё ничего не говорит о подключении.

use crate::engine::EngineKind;
use crate::secret::Secret;

#[derive(Debug, Clone)]
pub struct Profile {
    /// Название для человека: «Нидерланды», «Основной».
    pub name: String,
    /// Адрес сервера: домен или IP (IPv6 — без квадратных скобок).
    pub server: String,
    pub port: u16,
    pub protocol: Protocol,
    pub transport: Transport,
    pub security: Security,
    /// Что из ключа мы сознательно не применили. Показываем пользователю, а не прячем.
    pub warnings: Vec<String>,
}

/// Протокол «разговора» с сервером. Пока только VLESS; Hysteria 2 и другие добавятся сюда.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protocol {
    /// `uuid` — секрет пользователя; `flow` — режим Vision (если включён).
    Vless { uuid: Secret, flow: Option<Flow> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// `xtls-rprx-vision`: особая обработка TLS внутри туннеля. Работает только поверх TCP.
    Vision,
}

/// Транспорт: как байты едут до сервера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// Обычное TCP-соединение (в Xray называется RAW).
    Tcp,
    /// WebSocket; `early_data` — сколько байт отправить сразу в рукопожатии (параметр `?ed=`).
    Ws { path: String, host: Option<String>, early_data: Option<u32> },
    Grpc { service_name: String },
    HttpUpgrade { path: String, host: Option<String> },
    H2 { path: String, host: Option<String> },
    /// XHTTP (Xray): `mode` — способ отправки (stream-one, packet-up…),
    /// `extra` — дополнительные настройки XHTTP из ключа, передаём в Xray как есть.
    Xhttp { path: String, host: Option<String>, mode: Option<String>, extra: Option<serde_json::Value> },
}

/// Защита канала: настоящий TLS или REALITY (маскировка под чужой сайт).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Security {
    Tls { sni: Option<String>, alpn: Vec<String>, fingerprint: Option<String> },
    Reality { sni: String, public_key: String, short_id: String, fingerprint: String },
}

impl Profile {
    /// Короткое описание для интерфейса: «VLESS · TCP · REALITY · Vision».
    pub fn summary(&self) -> String {
        let Protocol::Vless { flow, .. } = &self.protocol;
        let mut parts = vec!["VLESS", self.transport.label(), self.security.label()];
        if *flow == Some(Flow::Vision) {
            parts.push("Vision");
        }
        parts.join(" · ")
    }

    /// Каким ядром подключаться: sing-box, если он умеет всё нужное, иначе Xray.
    pub fn engine(&self) -> EngineKind {
        match self.transport {
            Transport::Xhttp { .. } => EngineKind::Xray,
            _ => EngineKind::SingBox,
        }
    }
}

impl Transport {
    pub fn label(&self) -> &'static str {
        match self {
            Transport::Tcp => "TCP",
            Transport::Ws { .. } => "WebSocket",
            Transport::Grpc { .. } => "gRPC",
            Transport::HttpUpgrade { .. } => "HTTPUpgrade",
            Transport::H2 { .. } => "HTTP/2",
            Transport::Xhttp { .. } => "XHTTP",
        }
    }
}

impl Security {
    pub fn label(&self) -> &'static str {
        match self {
            Security::Tls { .. } => "TLS",
            Security::Reality { .. } => "REALITY",
        }
    }
}
