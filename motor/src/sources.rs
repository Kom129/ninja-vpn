//! Источники серверов: подписки (`https://…`) и одиночные ключи (`vless://…`).
//!
//! У человека может быть несколько VPN: подписка сервиса, свой сервер, рабочий доступ.
//! Каждый такой источник хранится отдельно, с его собственным названием.
//!
//! Где лежит: `%APPDATA%\com.ninjavpn.app\` (общая папка для окна и командной строки).
//! - `sources.json` — названия и сведения об источниках; сами ссылки и ключи — зашифрованы DPAPI;
//! - `cache\<id>.bin` — последняя скачанная копия списка серверов, тоже зашифрована
//!   (в ней идентификаторы пользователя). По ней окно работает и без доступа к сайту подписки.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use crate::dotenv;
use crate::profile::Profile;
use crate::protect::{self, ProtectError};
use crate::secret::Secret;
use crate::subscription::{self, Entry, FetchError, Usage};
use crate::vless::{self, ImportError};

pub const MAX_SOURCES: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// Ссылка, по которой сервис отдаёт список серверов.
    Subscription,
    /// Один ключ `vless://…` — один сервер.
    Key,
}

/// Всё об источнике, кроме секрета: это можно показывать в окне.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
    /// Название, которое прислал сам сервис (заголовок `profile-title`).
    pub title: Option<String>,
    pub usage: Option<Usage>,
    /// Когда список серверов обновлялся, секунды Unix.
    pub updated: Option<u64>,
    /// Узнаваемая, но не секретная часть: `your-durev.com` или `vless · nl.example.com`.
    pub hint: String,
}

#[derive(Serialize, Deserialize)]
struct Record {
    #[serde(flatten)]
    info: SourceInfo,
    /// Ссылка или ключ, зашифрованные DPAPI, в base64.
    secret: String,
}

#[derive(Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    sources: Vec<Record>,
}

use crate::t;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{}", t!("store.unknown_link"))]
    UnknownLink,
    #[error("{}", t!("store.plain_http"))]
    PlainHttp,
    #[error("{}", t!("store.duplicate", name = .0))]
    Duplicate(String),
    #[error("{}", t!("store.too_many", max = MAX_SOURCES))]
    TooMany,
    #[error("{}", t!("store.not_found"))]
    NotFound,
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error("{0}")]
    Import(#[from] ImportError),
    #[error(transparent)]
    Protect(#[from] ProtectError),
    #[error("{}", t!("store.io", why = .0))]
    Io(#[from] std::io::Error),
    #[error("{}", t!("store.corrupt", why = .0))]
    Corrupt(String),
    #[error("{}", t!("store.busy"))]
    Busy,
}

pub struct SourceStore {
    dir: PathBuf,
}

/// Пока жив — хранилище меняет только этот поток этой программы.
struct WriteLock {
    _in_process: MutexGuard<'static, ()>,
    _file: fs::File,
}

/// Что получили по ссылке: тело списка серверов и сведения о нём.
struct Fetched {
    body: String,
    title: Option<String>,
    usage: Option<Usage>,
    /// Для одиночного ключа — его имя из `#…`.
    key_name: Option<String>,
}

impl SourceStore {
    pub fn open(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `%APPDATA%\com.ninjavpn.app` — общая папка окна и командной строки.
    pub fn default_dir() -> PathBuf {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join("com.ninjavpn.app")
    }

    pub fn list(&self) -> Result<Vec<SourceInfo>, StoreError> {
        Ok(self.load()?.sources.into_iter().map(|r| r.info).collect())
    }

    /// Серверы источника из сохранённой копии — без обращения к сети.
    pub fn entries(&self, id: &str) -> Result<Vec<Entry>, StoreError> {
        let sealed = fs::read(self.cache_path(id)).map_err(|_| StoreError::NotFound)?;
        let body = String::from_utf8_lossy(&protect::unprotect(&sealed)?).into_owned();
        Ok(subscription::parse(&body)?)
    }

    /// Серверы источника с постоянными ключами `<id источника>/<имя>`.
    /// Номера в подписке меняются после обновления, имена — нет, а приставка источника
    /// не даёт спутать одинаковые имена у разных сервисов. Окно и командная строка
    /// пользуются одними и теми же ключами.
    pub fn servers(&self, id: &str) -> Result<Vec<(String, Entry)>, StoreError> {
        Ok(keyed(self.entries(id)?).into_iter().map(|(name, entry)| (format!("{id}/{name}"), entry)).collect())
    }

    /// Сервер по ключу `<id источника>/<имя>`. Ошибка — готовая фраза для человека.
    pub fn find(&self, key: &str) -> Result<Profile, String> {
        let (source, name) = key.split_once('/').ok_or_else(|| t!("store.no_server_selected"))?;
        let entries = self.entries(source).map_err(|e| e.to_string())?;
        let (_, entry) = keyed(entries)
            .into_iter()
            .find(|(k, _)| k == name)
            .ok_or_else(|| t!("store.server_missing", name = name))?;
        entry.result.map_err(|e| t!("store.server_unsuitable", name = name, why = e))
    }

    /// Один раз переносим старую подписку из `.env` папки проекта в хранилище.
    /// Берём уже скачанную копию (`runtime/subscription.txt`), поэтому сеть не нужна.
    /// Метка `migrated-env` не даёт вернуть источник, если его потом удалили.
    pub fn migrate_env(&self, project_root: &Path) {
        let marker = self.dir.join("migrated-env");
        if marker.exists() || !self.list().map(|l| l.is_empty()).unwrap_or(false) {
            return;
        }
        let env = dotenv::read(&project_root.join(".env"));
        let runtime = project_root.join("runtime");
        if let Some(url) = env.get("NINJA_SUBSCRIPTION_URL") {
            let cached = fs::read_to_string(runtime.join("subscription.txt"));
            let title = fs::read_to_string(runtime.join("subscription-info.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| v["title"].as_str().map(str::to_string));
            let _ = match cached {
                Ok(body) => self.import_cached(None, url, &body, title, None).map(|_| ()),
                Err(_) => self.add(None, url).map(|_| ()),
            };
        }
        if let Some(key) = env.get("NINJA_VLESS_KEY") {
            let _ = self.add(None, key);
        }
        let _ = fs::create_dir_all(&self.dir);
        let _ = fs::write(marker, "Подписка из .env перенесена в sources.json\n");
    }

    /// Добавить источник: сначала проверяем ссылку (скачиваем подписку или разбираем ключ),
    /// и только если всё хорошо — сохраняем.
    pub fn add(&self, name: Option<&str>, link: &str) -> Result<SourceInfo, StoreError> {
        let link = link.trim();
        let kind = classify(link)?;
        self.check_can_add(link)?; // быстро отказать ещё до сети
        let fetched = fetch(kind, link)?;
        let _lock = self.write_lock()?;
        self.check_can_add(link)?; // ещё раз: пока качали, то же самое могли добавить в окне или командой
        self.insert(name, link, kind, fetched)
    }

    /// Перенести уже скачанную подписку (например, из старого `.env`) — без обращения к сети.
    pub fn import_cached(
        &self,
        name: Option<&str>,
        link: &str,
        body: &str,
        title: Option<String>,
        usage: Option<Usage>,
    ) -> Result<SourceInfo, StoreError> {
        let link = link.trim();
        let kind = classify(link)?;
        subscription::parse(body)?;
        let _lock = self.write_lock()?;
        self.check_can_add(link)?;
        self.insert(name, link, kind, Fetched { body: body.to_string(), title, usage, key_name: None })
    }

    /// Скачать свежий список серверов источника.
    pub fn refresh(&self, id: &str) -> Result<SourceInfo, StoreError> {
        // Скачиваем без замка: сайт подписки может думать до 20 с, а окно в это время
        // должно спокойно переименовывать и добавлять другие источники.
        let (kind, link) = {
            let file = self.load()?;
            let record = file.sources.iter().find(|r| r.info.id == id).ok_or(StoreError::NotFound)?;
            (record.info.kind, open_secret(record)?)
        };
        let fetched = fetch(kind, &link)?;
        let _lock = self.write_lock()?;
        // Свежая копия файла: пока качали, источник могли переименовать, удалить
        // или добавить другие — старую копию записывать нельзя, изменения бы потерялись.
        let mut file = self.load()?;
        let record = file.sources.iter_mut().find(|r| r.info.id == id).ok_or(StoreError::NotFound)?;
        self.write_cache(id, &fetched.body)?;
        record.info.title = fetched.title.or(record.info.title.take());
        record.info.usage = fetched.usage.or(record.info.usage.take());
        record.info.updated = Some(now());
        let info = record.info.clone();
        self.save(&file)?;
        Ok(info)
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<SourceInfo, StoreError> {
        let _lock = self.write_lock()?;
        let mut file = self.load()?;
        let record = file.sources.iter_mut().find(|r| r.info.id == id).ok_or(StoreError::NotFound)?;
        if let Some(name) = clean_name(Some(name)) {
            record.info.name = name;
        }
        let info = record.info.clone();
        self.save(&file)?;
        Ok(info)
    }

    pub fn remove(&self, id: &str) -> Result<(), StoreError> {
        let _lock = self.write_lock()?;
        let mut file = self.load()?;
        let before = file.sources.len();
        file.sources.retain(|r| r.info.id != id);
        if file.sources.len() == before {
            return Err(StoreError::NotFound);
        }
        self.save(&file)?;
        let _ = fs::remove_file(self.cache_path(id));
        Ok(())
    }

    fn check_can_add(&self, link: &str) -> Result<(), StoreError> {
        let file = self.load()?;
        if file.sources.len() >= MAX_SOURCES {
            return Err(StoreError::TooMany);
        }
        for record in &file.sources {
            if same_link(&open_secret(record)?, link) {
                return Err(StoreError::Duplicate(record.info.name.clone()));
            }
        }
        Ok(())
    }

    fn insert(&self, name: Option<&str>, link: &str, kind: SourceKind, fetched: Fetched) -> Result<SourceInfo, StoreError> {
        let mut file = self.load()?;
        // Название: своё → от сервиса → имя ключа → адрес сайта подписки → «Источник N».
        let host = url::Url::parse(link).ok().filter(|_| kind == SourceKind::Subscription).and_then(|u| u.host_str().map(str::to_string));
        let name = clean_name(name)
            .or_else(|| clean_name(fetched.title.as_deref()))
            .or_else(|| clean_name(fetched.key_name.as_deref()))
            .or_else(|| clean_name(host.as_deref()))
            .unwrap_or_else(|| t!("store.default_name", n = file.sources.len() + 1));
        let info = SourceInfo {
            id: new_id(),
            name,
            kind,
            title: fetched.title,
            usage: fetched.usage,
            updated: Some(now()),
            hint: hint(link),
        };
        self.write_cache(&info.id, &fetched.body)?;
        let secret = STANDARD.encode(protect::protect(link.as_bytes())?);
        file.sources.push(Record { info: info.clone(), secret });
        self.save(&file)?;
        Ok(info)
    }

    /// Замок на изменения: по одному и внутри программы (окно делает несколько дел сразу),
    /// и между программами (окно и командная строка). Чтение замка не требует: файл
    /// подменяется целиком, и читатель видит либо старую, либо новую версию.
    fn write_lock(&self) -> Result<WriteLock, StoreError> {
        static IN_PROCESS: Mutex<()> = Mutex::new(());
        let in_process = IN_PROCESS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join("sources.lock");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut options = fs::OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            // Файл открыт «только для меня»: вторая программа получит отказ и подождёт.
            // Если программа упадёт, Windows сама закроет файл — вечной блокировки не будет.
            #[cfg(windows)]
            std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 0);
            match options.open(&path) {
                Ok(file) => return Ok(WriteLock { _in_process: in_process, _file: file }),
                // 32 — ERROR_SHARING_VIOLATION: файл держит другая программа.
                Err(e) if e.raw_os_error() == Some(32) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) if e.raw_os_error() == Some(32) => return Err(StoreError::Busy),
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn file_path(&self) -> PathBuf {
        self.dir.join("sources.json")
    }

    fn cache_path(&self, id: &str) -> PathBuf {
        self.dir.join("cache").join(format!("{id}.bin"))
    }

    fn load(&self) -> Result<StoreFile, StoreError> {
        match fs::read_to_string(self.file_path()) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| StoreError::Corrupt(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoreFile { version: 1, sources: Vec::new() }),
            Err(e) => Err(e.into()),
        }
    }

    /// Пишем во временный файл и подменяем — при сбое посреди записи старый файл не испортится.
    fn save(&self, file: &StoreFile) -> Result<(), StoreError> {
        fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join("sources.json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(file).expect("JSON всегда сериализуется"))?;
        fs::rename(tmp, self.file_path())?;
        Ok(())
    }

    fn write_cache(&self, id: &str, body: &str) -> Result<(), StoreError> {
        let path = self.cache_path(id);
        fs::create_dir_all(path.parent().expect("у файла кэша есть папка"))?;
        let tmp = path.with_extension("bin.tmp");
        fs::write(&tmp, protect::protect(body.as_bytes())?)?;
        fs::rename(tmp, path)?;
        Ok(())
    }
}

/// Имена-ключи: если два сервера одного источника называются одинаково, у второго будет « (2)».
fn keyed(entries: Vec<Entry>) -> Vec<(String, Entry)> {
    let mut seen = HashMap::<String, usize>::new();
    entries
        .into_iter()
        .map(|entry| {
            let count = seen.entry(entry.label.clone()).or_default();
            *count += 1;
            let key = if *count == 1 { entry.label.clone() } else { format!("{} ({count})", entry.label) };
            (key, entry)
        })
        .collect()
}

fn open_secret(record: &Record) -> Result<String, StoreError> {
    let sealed = STANDARD.decode(&record.secret).map_err(|e| StoreError::Corrupt(e.to_string()))?;
    Ok(String::from_utf8_lossy(&protect::unprotect(&sealed)?).into_owned())
}

/// Та же ли это ссылка. Подпись после `#` — только название («Мой сервер»), сервер от неё
/// не меняется: тот же ключ, скопированный с другим названием, — дубликат.
fn same_link(a: &str, b: &str) -> bool {
    let core = |s: &str| s.trim().split('#').next().unwrap_or_default().to_string();
    core(a) == core(b)
}

fn classify(link: &str) -> Result<SourceKind, StoreError> {
    let start = link.get(..8).unwrap_or(link).to_ascii_lowercase();
    if start.starts_with("https://") {
        Ok(SourceKind::Subscription)
    } else if start.starts_with("vless://") {
        Ok(SourceKind::Key)
    } else if start.starts_with("http://") {
        Err(StoreError::PlainHttp)
    } else {
        Err(StoreError::UnknownLink)
    }
}

fn fetch(kind: SourceKind, link: &str) -> Result<Fetched, StoreError> {
    match kind {
        SourceKind::Subscription => {
            let fetched = subscription::fetch(&Secret::new(link))?;
            subscription::parse(&fetched.body)?;
            Ok(Fetched { body: fetched.body, title: fetched.title, usage: fetched.usage, key_name: None })
        }
        SourceKind::Key => {
            let profile = vless::parse_vless(link)?;
            Ok(Fetched { body: link.to_string(), title: None, usage: None, key_name: Some(profile.name) })
        }
    }
}

/// Узнаваемая, но не секретная часть ссылки.
fn hint(link: &str) -> String {
    let host = url::Url::parse(link).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    if link.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("vless://")) { format!("vless · {host}") } else { host }
}

fn clean_name(name: Option<&str>) -> Option<String> {
    name.map(vless::clean_name).filter(|n| !n.is_empty())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn new_id() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("s{:x}", nanos % 0xffff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "11111111-2222-4333-8444-555555555555";

    fn temp_store(name: &str) -> SourceStore {
        let dir = std::env::temp_dir().join(format!("ninja-sources-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        SourceStore::open(dir)
    }

    fn key(host: &str) -> String {
        format!("vless://{UUID}@{host}:443?security=tls&sni={host}#%D0%A1%D0%B2%D0%BE%D0%B9")
    }

    #[test]
    fn add_list_rename_remove_key() {
        let store = temp_store("crud");
        let info = store.add(None, &key("a.example.com")).unwrap();
        assert_eq!(info.kind, SourceKind::Key);
        assert_eq!(info.name, "Свой");
        assert_eq!(info.hint, "vless · a.example.com");
        assert_eq!(store.list().unwrap(), vec![info.clone()]);
        assert_eq!(store.entries(&info.id).unwrap().len(), 1);

        let renamed = store.rename(&info.id, "  Мой сервер ").unwrap();
        assert_eq!(renamed.name, "Мой сервер");

        assert!(matches!(store.add(Some("ещё раз"), &key("a.example.com")), Err(StoreError::Duplicate(n)) if n == "Мой сервер"));
        // Тот же ключ с другой подписью после # — тоже дубликат.
        let renamed_copy = key("a.example.com").replace("#%D0%A1%D0%B2%D0%BE%D0%B9", "#Другое");
        assert!(matches!(store.add(None, &renamed_copy), Err(StoreError::Duplicate(_))));
        store.add(Some("Второй"), &key("b.example.com")).unwrap();
        assert_eq!(store.list().unwrap().len(), 2);

        store.remove(&info.id).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(matches!(store.entries(&info.id), Err(StoreError::NotFound)));
        let _ = fs::remove_dir_all(&store.dir);
    }

    #[test]
    fn secrets_are_not_stored_in_plain_text() {
        let store = temp_store("secret");
        let info = store.add(Some("Тест"), &key("secret-host.example.com")).unwrap();
        let json = fs::read_to_string(store.file_path()).unwrap();
        let cache = fs::read(store.cache_path(&info.id)).unwrap();
        assert!(!json.contains(UUID));
        assert!(!cache.windows(UUID.len()).any(|w| w == UUID.as_bytes()));
        let _ = fs::remove_dir_all(&store.dir);
    }

    #[test]
    fn import_cached_subscription_without_network() {
        let store = temp_store("import");
        let body = STANDARD.encode(format!("{}\n{}", key("a.example.com"), key("b.example.com")));
        let info = store
            .import_cached(None, "https://sub.example.com/sub/token", &body, Some("Сервис".into()), None)
            .unwrap();
        assert_eq!((info.name.as_str(), info.kind, info.hint.as_str()), ("Сервис", SourceKind::Subscription, "sub.example.com"));
        assert_eq!(store.entries(&info.id).unwrap().len(), 2);
        let _ = fs::remove_dir_all(&store.dir);
    }

    #[test]
    fn servers_have_stable_keys_and_can_be_found() {
        let store = temp_store("keys");
        // Два сервера с одинаковым именем «Свой» и один сломанный.
        let body = format!("{}\n{}\nvless://broken#Сломанный", key("a.example.com"), key("b.example.com"));
        let info = store.import_cached(Some("Сервис"), "https://sub.example.com/x", &body, None, None).unwrap();
        let keys: Vec<String> = store.servers(&info.id).unwrap().into_iter().map(|(k, _)| k).collect();
        let id = &info.id;
        assert_eq!(keys, [format!("{id}/Свой"), format!("{id}/Свой (2)"), format!("{id}/Сломанный")]);

        assert_eq!(store.find(&keys[0]).unwrap().server, "a.example.com");
        assert_eq!(store.find(&keys[1]).unwrap().server, "b.example.com");
        assert!(store.find(&keys[2]).unwrap_err().contains("не подходит"));
        assert!(store.find(&format!("{id}/Нет такого")).unwrap_err().contains("не найден"));
        assert!(store.find("без-косой-черты").is_err());
        let _ = fs::remove_dir_all(&store.dir);
    }

    #[test]
    fn migrates_env_once_from_cached_copy() {
        let store = temp_store("migrate");
        let project = std::env::temp_dir().join(format!("ninja-project-{}", std::process::id()));
        fs::create_dir_all(project.join("runtime")).unwrap();
        fs::write(project.join(".env"), "NINJA_SUBSCRIPTION_URL=https://sub.example.com/sub/t\n").unwrap();
        fs::write(project.join("runtime").join("subscription.txt"), key("a.example.com")).unwrap();

        store.migrate_env(&project);
        let list = store.list().unwrap();
        assert_eq!((list.len(), list[0].hint.as_str()), (1, "sub.example.com"));

        // Удалили источник — второй раз он не возвращается.
        store.remove(&list[0].id).unwrap();
        store.migrate_env(&project);
        assert!(store.list().unwrap().is_empty());
        let _ = fs::remove_dir_all(&store.dir);
        let _ = fs::remove_dir_all(&project);
    }

    #[test]
    fn simultaneous_changes_are_not_lost() {
        let store = std::sync::Arc::new(temp_store("parallel"));
        let first = store.add(Some("Переименуй меня"), &key("base.example.com")).unwrap();
        // 8 потоков одновременно добавляют разные ключи, а ещё один всё время переименовывает
        // и обновляет первый источник. Без замка часть добавленных пропадала бы.
        let mut threads: Vec<_> = (0..8)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || store.add(None, &key(&format!("h{i}.example.com"))).map(|_| ()))
            })
            .collect();
        let (renamer, id) = (store.clone(), first.id.clone());
        threads.push(std::thread::spawn(move || {
            for n in 0..10 {
                renamer.rename(&id, &format!("Имя {n}"))?;
                renamer.refresh(&id)?;
            }
            Ok(())
        }));
        for t in threads {
            t.join().unwrap().unwrap();
        }
        let list = store.list().unwrap();
        assert_eq!(list.len(), 9, "все 8 добавленных и первый источник на месте");
        assert_eq!(list.iter().find(|s| s.id == first.id).unwrap().name, "Имя 9");
        for info in &list {
            assert_eq!(store.servers(&info.id).unwrap().len(), 1, "у каждого источника цел список серверов");
        }
        let _ = fs::remove_dir_all(&store.dir);
    }

    #[test]
    fn rejects_bad_links() {
        let store = temp_store("bad");
        assert!(matches!(store.add(None, "http://sub.example.com/x"), Err(StoreError::PlainHttp)));
        assert!(matches!(store.add(None, "просто текст"), Err(StoreError::UnknownLink)));
        assert!(matches!(store.add(None, "vless://broken"), Err(StoreError::Import(_))));
        assert!(store.list().unwrap().is_empty());
    }
}
