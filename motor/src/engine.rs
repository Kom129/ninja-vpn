//! Сетевые ядра: какое нужно профилю, где лежит, как проверить конфиг, запустить и остановить.

use std::fs::{self, File};
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::{singbox, xray};

/// Какое ядро запускаем. sing-box — основное; Xray — только для того, чего sing-box не умеет (XHTTP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    SingBox,
    Xray,
}

impl EngineKind {
    /// Имя как в engines/engines.json.
    pub fn name(self) -> &'static str {
        match self {
            EngineKind::SingBox => "sing-box",
            EngineKind::Xray => "xray",
        }
    }

    /// Версия, под которую мотор собирает конфиги.
    pub fn version(self) -> &'static str {
        match self {
            EngineKind::SingBox => singbox::SING_BOX_VERSION,
            EngineKind::Xray => xray::XRAY_VERSION,
        }
    }

    pub fn label(self) -> String {
        match self {
            EngineKind::SingBox => format!("sing-box {}", self.version()),
            EngineKind::Xray => format!("Xray {}", self.version()),
        }
    }
}

use crate::t;

/// Пути к скачанным ядрам — из engines/engines.json.
#[derive(Debug, Clone)]
pub struct Engines {
    sing_box: PathBuf,
    xray: PathBuf,
}

impl Engines {
    pub fn load(project_root: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(project_root.join("engines").join("engines.json"))
            .map_err(|e| t!("engine.manifest_read", why = e))?;
        let manifest: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("engines.json: {e}"))?;
        let path_of = |kind: EngineKind| -> Result<PathBuf, String> {
            let entry = manifest["engines"]
                .as_array()
                .and_then(|list| list.iter().find(|e| e["name"] == kind.name()))
                .ok_or_else(|| t!("engine.manifest_no_engine", name = kind.name()))?;
            let version = entry["version"].as_str().unwrap_or_default();
            if version != kind.version() {
                return Err(t!("engine.manifest_version", name = kind.name(), version = version, expected = kind.version()));
            }
            let exe = entry["exe"].as_str().ok_or_else(|| t!("engine.manifest_no_exe", name = kind.name()))?;
            // В engines.json путь записан через «/». Собираем его по частям, чтобы в пути были
            // только «\»: правило TUN «наше ядро — напрямую» сравнивает путь с тем, что видит
            // Windows, буква в букву, и «…amd64/sing-box.exe» с ним не совпало бы.
            let mut path = project_root.join("engines").join("bin").join(format!("{}-{version}", kind.name()));
            path.extend(exe.split(['/', '\\']).filter(|part| !part.is_empty()));
            Ok(path)
        };
        Ok(Self { sing_box: path_of(EngineKind::SingBox)?, xray: path_of(EngineKind::Xray)? })
    }

    pub fn path(&self, kind: EngineKind) -> &Path {
        match kind {
            EngineKind::SingBox => &self.sing_box,
            EngineKind::Xray => &self.xray,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{}", t!("engine.missing", path = .0.display()))]
    Missing(PathBuf),
    #[error("{}", t!("engine.bad_config", details = .0))]
    BadConfig(String),
    #[error("{}", t!("engine.exited", details = .0))]
    Exited(String),
    #[error("{}", t!("engine.port_timeout", port = .0))]
    PortTimeout(u16),
    #[error("{}", t!("engine.io", why = .0))]
    Io(#[from] io::Error),
}

/// Проверка конфига самим ядром. В сеть при этом не ходим.
pub fn check_config(kind: EngineKind, exe: &Path, config: &Path) -> Result<(), EngineError> {
    let mut cmd = command(exe, config)?;
    match kind {
        EngineKind::SingBox => cmd.args(["--disable-color", "check", "-c"]),
        EngineKind::Xray => cmd.args(["run", "-test", "-c"]),
    };
    let output = cmd.arg(config).output()?;
    if output.status.success() {
        Ok(())
    } else {
        // sing-box пишет ошибки в stderr, Xray — в stdout; берём оба.
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        Err(EngineError::BadConfig(last_lines(&text, 5)))
    }
}

/// Конфиг ядра на диске — только пока он нужен: в нём ключи.
/// У каждого запуска своё имя, поэтому окно, командная строка и проверка скорости
/// не перепишут конфиги друг друга. Файл удаляется сам, когда значение уничтожается.
pub struct ConfigFile(PathBuf);

impl ConfigFile {
    pub fn write(dir: &Path, prefix: &str, config: &serde_json::Value) -> io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        fs::create_dir_all(dir)?;
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{prefix}-{}-{n}.json", std::process::id()));
        fs::write(&path, serde_json::to_string_pretty(config).expect("JSON всегда сериализуется"))?;
        Ok(Self(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ConfigFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Запущенное ядро. Когда значение уничтожается (drop), процесс останавливается,
/// поэтому ядро не останется висеть в фоне после ошибки.
pub struct RunningEngine {
    child: Child,
    log_path: PathBuf,
}

impl RunningEngine {
    /// Запустить ядро и дождаться, пока оно откроет локальный порт.
    pub fn start(kind: EngineKind, exe: &Path, config: &Path, log_path: &Path, port: u16) -> Result<Self, EngineError> {
        // Журнал прошлого запуска не затираем, а храним как `<ядро>.prev.log`:
        // если ядро упало и окно переподключилось, по нему видно, что случилось.
        if log_path.exists() {
            let _ = fs::rename(log_path, log_path.with_extension("prev.log"));
        }
        let log = File::create(log_path)?;
        let mut cmd = command(exe, config)?;
        match kind {
            EngineKind::SingBox => cmd.args(["--disable-color", "run", "-c"]),
            EngineKind::Xray => cmd.args(["run", "-c"]),
        };
        let child = cmd
            .arg(config)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        #[cfg(windows)]
        job::attach(&child);
        let mut engine = Self { child, log_path: log_path.to_path_buf() };
        engine.wait_for_port(port)?;
        Ok(engine)
    }

    fn wait_for_port(&mut self, port: u16) -> Result<(), EngineError> {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if self.child.try_wait()?.is_some() {
                return Err(EngineError::Exited(self.log_tail()));
            }
            if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(EngineError::PortTimeout(port));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Последние строки журнала ядра — для понятного сообщения об ошибке.
    pub fn log_tail(&self) -> String {
        fs::read_to_string(&self.log_path).map(|s| last_lines(&s, 5)).unwrap_or_default()
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for RunningEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

fn command(exe: &Path, config: &Path) -> Result<Command, EngineError> {
    if !exe.exists() {
        return Err(EngineError::Missing(exe.to_path_buf()));
    }
    let mut cmd = Command::new(exe);
    // Служебные файлы ядра — рядом с конфигом, а не там, откуда запустили программу.
    if let Some(dir) = config.parent() {
        cmd.current_dir(dir);
    }
    // Без этого флага из окна приложения выскакивало бы чёрное окно консоли ядра.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    Ok(cmd)
}

/// Привязать процесс к общему «заданию» ядер: закроется наш процесс — Windows остановит и его.
pub fn attach_to_job(child: &Child) {
    #[cfg(windows)]
    job::attach(child);
    #[cfg(not(windows))]
    let _ = child;
}

/// Все ядра — в одном «задании» Windows (Job Object) с флагом «завершить при закрытии».
/// Если мотор или окно закроют, или они упадут, Windows сама остановит ядра:
/// они не останутся висеть в фоне.
#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use std::sync::OnceLock;

    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    /// Дескриптор задания живёт, пока жив наш процесс; закрывается вместе с ним.
    static JOB: OnceLock<usize> = OnceLock::new();

    pub fn attach(child: &Child) {
        let job = *JOB.get_or_init(|| unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return 0;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                std::mem::size_of_val(&info) as u32,
            );
            job as usize
        });
        if job != 0 {
            unsafe {
                AssignProcessToJobObject(job as _, child.as_raw_handle() as _);
            }
        }
    }
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Путь к ядру — только с «\» (в engines.json у sing-box он записан через «/»).
    #[test]
    #[cfg(windows)]
    fn engine_paths_use_backslashes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let engines = Engines::load(root).unwrap();
        for kind in [EngineKind::SingBox, EngineKind::Xray] {
            let path = engines.path(kind).display().to_string();
            assert!(!path.contains('/'), "{path}");
        }
        assert!(engines.path(EngineKind::SingBox).ends_with(r"sing-box-1.14.2-windows-amd64\sing-box.exe"));
    }
}
