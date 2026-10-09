//! Мотор ninja-vpn: всё, что не касается окна.
//!
//! Путь данных: ключ `vless://` или подписка → [`Profile`] (наша проверенная модель)
//! → конфиг sing-box или Xray (что умеет профиль) → запущенное ядро → проверка, что данные реально идут → «подключено».

pub mod i18n;

pub mod apps;
pub mod browser;
pub mod dotenv;
pub mod engine;
pub mod lnk;
pub mod pair;
pub mod probe;
pub mod profile;
pub mod protect;
pub mod secret;
pub mod session;
pub mod singbox;
pub mod sources;
pub mod subscription;
pub mod tun;
pub mod verify;
pub mod vless;
pub mod winsys;
pub mod xray;

pub use engine::{EngineKind, Engines};
pub use profile::{Flow, Profile, Protocol, Security, Transport};
pub use secret::Secret;
pub use sources::{SourceInfo, SourceKind, SourceStore};
pub use session::{ConnectOptions, Session, State, connect};
