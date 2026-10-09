//! Сеанс подключения: профиль → конфиг → ядро → проверка передачи.
//!
//! О каждом этапе сообщаем событием [`State`]. Окно потом подпишется на эти же
//! события, и анимация ниндзя пойдёт за настоящим состоянием, а не за таймером.

use std::fs;
use std::path::PathBuf;

use crate::engine::{self, ConfigFile, EngineError, EngineKind, Engines, RunningEngine};
use crate::profile::Profile;
use crate::verify::{self, Report, VerifyError};
use crate::{singbox, xray};

/// Этапы подключения (названия как в docs/spec/IMPLEMENTATION.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Disconnected,
    /// Собираем и проверяем конфиг.
    Preparing,
    /// Запускаем ядро.
    Connecting,
    /// Гоняем настоящие данные через сервер.
    Verifying,
    /// Данные идут — проверено.
    Connected,
    Failed(String),
    Disconnecting,
}

impl State {
    pub fn title(&self) -> &str {
        match self {
            State::Disconnected => "Не подключено",
            State::Preparing => "Подготовка",
            State::Connecting => "Подключение",
            State::Verifying => "Проверка передачи данных",
            State::Connected => "Подключено",
            State::Failed(_) => "Ошибка подключения",
            State::Disconnecting => "Отключение",
        }
    }
}

pub struct ConnectOptions {
    pub engines: Engines,
    /// Папка для конфига и журнала ядра. В ней лежат секреты, в Git она не попадает.
    pub runtime_dir: PathBuf,
    pub listen_port: u16,
}

use crate::t;

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("{}", t!("session.prepare", why = .0))]
    Prepare(#[from] std::io::Error),
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// Вторая часть — последние строки журнала ядра (если есть).
    #[error("{0}{1}")]
    Verify(VerifyError, String),
}

pub struct Session {
    engine: RunningEngine,
    pub report: Report,
    pub listen_port: u16,
}

pub fn connect(
    profile: &Profile,
    options: &ConnectOptions,
    mut on_state: impl FnMut(&State),
) -> Result<Session, ConnectError> {
    let result = run(profile, options, &mut on_state);
    if let Err(e) = &result {
        on_state(&State::Failed(e.to_string()));
    }
    result
}

fn run(profile: &Profile, options: &ConnectOptions, on_state: &mut impl FnMut(&State)) -> Result<Session, ConnectError> {
    on_state(&State::Preparing);
    fs::create_dir_all(&options.runtime_dir)?;
    let kind = profile.engine();
    let exe = options.engines.path(kind);
    let config = match kind {
        EngineKind::SingBox => singbox::local_proxy_config(profile, options.listen_port),
        EngineKind::Xray => xray::local_proxy_config(profile, options.listen_port),
    };
    let config_file = ConfigFile::write(&options.runtime_dir, kind.name(), &config)?;
    engine::check_config(kind, exe, config_file.path())?;

    on_state(&State::Connecting);
    let log_path = options.runtime_dir.join(format!("{}.log", kind.name()));
    let engine = RunningEngine::start(kind, exe, config_file.path(), &log_path, options.listen_port)?;
    // Ядро открыло порт — значит, конфиг уже прочитан. Ключам на диске больше лежать незачем.
    drop(config_file);

    on_state(&State::Verifying);
    // При ошибке `engine` уничтожается при выходе из функции — ядро останавливается само.
    let report = verify::via_proxy(options.listen_port).map_err(|e| {
        let tail = engine.log_tail();
        let hint = if tail.is_empty() { String::new() } else { t!("session.log_tail", tail = tail) };
        ConnectError::Verify(e, hint)
    })?;

    on_state(&State::Connected);
    Ok(Session { engine, report, listen_port: options.listen_port })
}

impl Session {
    /// Ядро всё ещё работает?
    pub fn is_alive(&mut self) -> bool {
        self.engine.is_running()
    }

    pub fn disconnect(mut self, mut on_state: impl FnMut(&State)) {
        on_state(&State::Disconnecting);
        self.engine.stop();
        on_state(&State::Disconnected);
    }
}
