//! Режим «Приложения»: выбранные программы ходят в интернет только через VPN.
//!
//! Как это устроено:
//! 1. **Запуск через VPN.** Программу запускаем сами, с настройкой «ходи через прокси
//!    127.0.0.1:порт»: ключом запуска (его понимают программы на Chromium/Electron — Discord,
//!    Slack, VS Code…) и переменными окружения (их понимают многие другие).
//! 2. **Сторож.** Раз в пару секунд смотрим соединения процессов программы: к нашему прокси
//!    (через VPN) или напрямую в интернет. Напрямую — обход, и окно сразу об этом говорит.
//! 3. **Замок** (`firewall.rs`): правило брандмауэра Windows запрещает программе любые
//!    соединения, кроме нашего прокси на этом компьютере. Без VPN программа просто не выходит
//!    в интернет — даже если открыть её с рабочего стола, а не из ninja-vpn.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::lnk;
use crate::winsys::{self, Proc, TcpConn, TcpState};

/// Программа в списке режима «Приложения».
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct App {
    pub id: String,
    pub name: String,
    /// Что запускать: сама программа или «запускалка» (у Discord — `Update.exe`, а какую
    /// программу она запускает, сказано в `args`). Настоящий exe ищем при каждом запуске:
    /// после обновления Discord он лежит в новой папке.
    pub target: PathBuf,
    #[serde(default)]
    pub args: String,
    /// Значок картинкой PNG (data URL), чтобы не доставать его из exe каждый раз.
    #[serde(default)]
    pub icon: Option<String>,
    /// Запускать сразу после подключения.
    #[serde(default = "yes")]
    pub autostart: bool,
    /// Для каких exe включён замок брандмауэра (после обновления программы путь может смениться).
    #[serde(default)]
    pub locked: Vec<PathBuf>,
    /// Приложение из Microsoft Store: его папка меняется с каждой версией, поэтому храним
    /// «адрес» приложения в Windows, а путь к exe находим при каждом запуске.
    #[serde(default)]
    pub package: Option<StorePackage>,
}

/// Приложение из Microsoft Store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StorePackage {
    /// Идентификатор приложения в Windows: `Claude_pzs8sxrjxfjjc!Claude`.
    pub aumid: String,
    /// Семейство пакета (часть до «!»).
    pub family: String,
    /// Путь к exe внутри папки пакета: `app\Claude.exe`.
    pub exe: String,
}

fn yes() -> bool {
    true
}

/// Как программе сказать «ходи через прокси».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Support {
    /// Chromium / Electron / CEF: понимает ключ `--proxy-server` — работает надёжно.
    Chromium,
    /// Другая программа: даём переменные окружения; послушается ли — покажет сторож.
    Other,
}

/// Что на самом деле запускать.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub exe: PathBuf,
    pub args: String,
    /// Приложение из Microsoft Store запускается через Windows по этому идентификатору.
    pub aumid: Option<String>,
}

/// Список программ хранится в `apps.json` рядом с источниками. Секретов в нём нет.
pub struct AppStore {
    file: PathBuf,
}

impl AppStore {
    pub fn open(dir: &Path) -> Self {
        Self { file: dir.join("apps.json") }
    }

    pub fn load(&self) -> Vec<App> {
        std::fs::read(&self.file).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default()
    }

    /// Запись через временный файл: при сбое посреди записи старый список не портится.
    pub fn save(&self, apps: &[App]) -> io::Result<()> {
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.file.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(apps).map_err(io::Error::other)?)?;
        std::fs::rename(&tmp, &self.file)
    }
}

/// Найти настоящий exe. Discord, Slack, Teams и другие программы на Squirrel запускаются
/// через `Update.exe --processStart Discord.exe`, а сама программа лежит в `app-1.0.9203\`.
/// Берём самую новую такую папку. `None` — программы на месте нет (удалили).
pub fn resolve(target: &Path, args: &str) -> Option<Launch> {
    let is_updater = target.file_name().is_some_and(|n| n.eq_ignore_ascii_case("update.exe"));
    if is_updater && let Some(name) = arg_value(args, "--processStart") {
        let dir = target.parent()?;
        let newest = std::fs::read_dir(dir)
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let folder = e.file_name().to_string_lossy().into_owned();
                let version = folder.strip_prefix("app-")?.to_string();
                let exe = e.path().join(&name);
                exe.is_file().then(|| (version_key(&version), exe))
            })
            .max_by(|a, b| a.0.cmp(&b.0))?;
        let args = arg_value(args, "--process-start-args").unwrap_or_default();
        return Some(Launch { exe: newest.1, args, aumid: None });
    }
    target.is_file().then(|| Launch { exe: target.to_path_buf(), args: args.to_string(), aumid: None })
}

/// Что запускать для программы из списка (с учётом приложений из Microsoft Store).
pub fn resolve_app(app: &App) -> Option<Launch> {
    match &app.package {
        Some(p) => resolve_package(p, &app.args),
        None => resolve(&app.target, &app.args),
    }
}

fn resolve_package(p: &StorePackage, args: &str) -> Option<Launch> {
    // В манифестах встречается и `app/ChatGPT.exe` — Windows пишет пути процессов через «\».
    let exe = winsys::package_path(&p.family)?.join(p.exe.replace('/', "\\"));
    exe.is_file().then(|| Launch { exe, args: args.to_string(), aumid: Some(p.aumid.clone()) })
}

/// «1.0.9203» → [1, 0, 9203]: чтобы 1.0.10 была новее 1.0.9.
fn version_key(text: &str) -> Vec<u64> {
    text.split(['.', '-']).map(|p| p.parse().unwrap_or(0)).collect()
}

/// Значение параметра запуска: `--processStart Discord.exe` или `--processStart "My App.exe"`.
fn arg_value(args: &str, name: &str) -> Option<String> {
    let lower = args.to_ascii_lowercase();
    let at = lower.find(&name.to_ascii_lowercase())? + name.len();
    let rest = args[at..].trim_start_matches(['=', ' ']);
    let value = match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next()?,
        None => rest.split_whitespace().next()?,
    };
    (!value.is_empty()).then(|| value.to_string())
}

/// Программа на Chromium/Electron/CEF? Узнаём по файлам движка рядом с exe или в соседней
/// подпапке: Chrome и Edge держат их в папке версии (`154.0.8037.93\`), VS Code — в `8a7abeba6e\`.
pub fn support(exe: &Path) -> Support {
    let Some(dir) = exe.parent() else { return Support::Other };
    let marks = ["icudtl.dat", "v8_context_snapshot.bin", "chrome_100_percent.pak", "resources.pak", "libcef.dll"];
    let has_marks = |d: &Path| marks.iter().any(|m| d.join(m).is_file());
    let in_subfolder = || {
        std::fs::read_dir(dir).into_iter().flatten().filter_map(Result::ok).take(64).any(|e| e.path().is_dir() && has_marks(&e.path()))
    };
    if has_marks(dir) || in_subfolder() { Support::Chromium } else { Support::Other }
}

use crate::t;

/// Почему эту программу нельзя добавить (или `None`, если можно).
/// Системные программы Windows не трогаем: замок на них сломал бы сам Windows.
pub fn refuse(exe: &Path) -> Option<String> {
    let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    if starts_with_ci(exe, &windir) {
        return Some(t!("apps.windows_part"));
    }
    if let Ok(me) = std::env::current_exe()
        && same_path(&me, exe)
    {
        return Some(t!("apps.is_self"));
    }
    let name = exe.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    if !name.ends_with(".exe") {
        return Some(t!("apps.not_program"));
    }
    if ["sing-box.exe", "xray.exe", "ninja.exe", "ninja-vpn.exe"].contains(&name.as_str()) {
        return Some(t!("apps.part_of_ninja"));
    }
    None
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().to_string_lossy().eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

fn starts_with_ci(path: &Path, dir: &Path) -> bool {
    let (p, d) = (path.to_string_lossy().to_lowercase(), dir.to_string_lossy().to_lowercase());
    p.strip_prefix(d.trim_end_matches('\\')).is_some_and(|rest| rest.starts_with('\\'))
}

/// Запустить программу через прокси `127.0.0.1:port`. Возвращает номер процесса.
pub fn launch(launch: &Launch, port: u16) -> io::Result<u32> {
    let proxy = format!("http://127.0.0.1:{port}");
    if let Some(aumid) = &launch.aumid {
        // Приложению из Microsoft Store переменные окружения не передать — только параметры запуска.
        let mut args = launch.args.trim().to_string();
        if support(&launch.exe) == Support::Chromium {
            args = format!("{args} --proxy-server={proxy} --force-webrtc-ip-handling-policy=disable_non_proxied_udp").trim().to_string();
        }
        return winsys::activate_app(aumid, &args).map_err(io::Error::other);
    }
    let mut cmd = Command::new(&launch.exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if !launch.args.trim().is_empty() {
            cmd.raw_arg(&launch.args);
        }
        if support(&launch.exe) == Support::Chromium {
            cmd.raw_arg(format!("--proxy-server={proxy}"));
            // WebRTC (звонки) иначе может отправить UDP напрямую, мимо прокси.
            cmd.raw_arg("--force-webrtc-ip-handling-policy=disable_non_proxied_udp");
        }
    }
    // Переменные окружения понимают многие программы (в Windows регистр имени не важен).
    cmd.env("HTTP_PROXY", &proxy).env("HTTPS_PROXY", &proxy).env("ALL_PROXY", &proxy);
    cmd.env("NO_PROXY", "localhost,127.0.0.1,::1");
    if let Some(dir) = launch.exe.parent() {
        cmd.current_dir(dir);
    }
    Ok(cmd.spawn()?.id())
}

/// Установленная программа для списка «Добавить» (из меню «Пуск»).
#[derive(Debug, Clone)]
pub struct Installed {
    pub name: String,
    pub target: PathBuf,
    pub args: String,
    pub exe: PathBuf,
    /// Файл значка: exe/ico (тогда `icon_index` — номер значка) или готовая картинка PNG.
    pub icon_file: PathBuf,
    pub icon_index: i32,
    pub package: Option<StorePackage>,
}

/// Все файлы в папке и подпапках (не глубже `depth`).
pub fn walk(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            if depth > 0 {
                out.extend(walk(&path, depth - 1));
            }
        } else {
            out.push(path);
        }
    }
    out
}

/// Программы из меню «Пуск» (своего и общего). Без системных, удалялок и дублей.
pub fn installed() -> Vec<Installed> {
    let mut dirs = Vec::new();
    for (var, rel) in [("APPDATA", r"Microsoft\Windows\Start Menu\Programs"), ("ProgramData", r"Microsoft\Windows\Start Menu\Programs")] {
        if let Some(base) = std::env::var_os(var) {
            dirs.push(PathBuf::from(base).join(rel));
        }
    }
    let mut seen = HashSet::new();
    let mut list = Vec::new();
    for file in dirs.iter().flat_map(|d| walk(d, 4)) {
        if !file.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
            continue;
        }
        let name = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let lower = name.to_lowercase();
        if ["uninstall", "удал", "readme", "help", "справк", "documentation"].iter().any(|w| lower.contains(w)) {
            continue;
        }
        let Some(shortcut) = std::fs::read(&file).ok().and_then(|d| lnk::parse(&d)) else { continue };
        let Some(found) = resolve(&shortcut.target, &shortcut.arguments) else { continue };
        if refuse(&found.exe).is_some() || !seen.insert(found.exe.to_string_lossy().to_lowercase()) {
            continue;
        }
        let (icon_file, icon_index) = shortcut.icon.filter(|(f, _)| f.is_file()).unwrap_or((found.exe.clone(), 0));
        list.push(Installed { name, target: shortcut.target, args: shortcut.arguments, exe: found.exe, icon_file, icon_index, package: None });
    }
    for app in store_apps() {
        if seen.insert(app.exe.to_string_lossy().to_lowercase()) {
            list.push(app);
        }
    }
    list.sort_by_key(|a| a.name.to_lowercase());
    list
}

/// Приложения из Microsoft Store, у которых есть обычный exe (Claude, ChatGPT, Teams…).
/// Список берём у Windows одной командой PowerShell: `Get-StartApps` + описание пакета.
pub fn store_apps() -> Vec<Installed> {
    const SCRIPT: &str = r#"
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$ErrorActionPreference = 'SilentlyContinue'
$packages = @{}
Get-AppxPackage | ForEach-Object { $packages[$_.PackageFamilyName] = $_ }
@(Get-StartApps | Where-Object { $_.AppID -match '!' } | ForEach-Object {
  $family, $id = $_.AppID -split '!', 2
  $p = $packages[$family]
  if (-not $p) { return }
  [xml]$m = Get-Content -LiteralPath (Join-Path $p.InstallLocation 'AppxManifest.xml') -Raw -Encoding UTF8
  $app = @($m.Package.Applications.Application | Where-Object { $_.Id -eq $id })[0]
  if ($app.Executable -and $app.EntryPoint -eq 'Windows.FullTrustApplication') {
    [pscustomobject]@{ name = $_.Name; aumid = $_.AppID; family = $family; exe = $app.Executable; logo = $app.VisualElements.Square44x44Logo }
  }
}) | ConvertTo-Json -Compress
"#;
    #[derive(Deserialize)]
    struct Row {
        name: String,
        aumid: String,
        family: String,
        exe: String,
        logo: Option<String>,
    }
    let Some(out) = powershell(SCRIPT) else { return Vec::new() };
    let rows: Vec<Row> = serde_json::from_str(out.trim()).unwrap_or_default();
    rows.into_iter()
        .filter_map(|r| {
            let package = StorePackage { aumid: r.aumid, family: r.family, exe: r.exe };
            let found = resolve_package(&package, "")?;
            if refuse(&found.exe).is_some() {
                return None;
            }
            let dir = winsys::package_path(&package.family)?;
            let logo = r.logo.and_then(|l| store_logo(&dir, &l));
            Some(Installed {
                name: r.name,
                target: found.exe.clone(),
                args: String::new(),
                icon_file: logo.unwrap_or_else(|| found.exe.clone()),
                icon_index: 0,
                exe: found.exe,
                package: Some(package),
            })
        })
        .collect()
}

/// Картинка значка приложения из Microsoft Store. В манифесте написано `Assets\Square44x44Logo.png`,
/// а на диске лежат варианты: `Square44x44Logo.targetsize-48.png`, `….scale-200.png`.
fn store_logo(dir: &Path, logo: &str) -> Option<PathBuf> {
    let wanted = dir.join(logo);
    let folder = wanted.parent()?;
    let stem = wanted.file_stem()?.to_string_lossy().to_lowercase();
    let mut variants: Vec<PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            name.starts_with(&stem) && name.ends_with(".png") && !name.contains("contrast")
        })
        .collect();
    let score = |p: &PathBuf| {
        let name = p.to_string_lossy().to_lowercase();
        if name.contains("targetsize-48") && !name.contains("altform") { 0 }
        else if name.contains("targetsize-48") { 1 }
        else if name.contains("scale-200") { 2 }
        else { 3 }
    };
    variants.sort_by_key(score);
    variants.into_iter().next().or_else(|| wanted.is_file().then_some(wanted))
}

/// Выполнить скрипт PowerShell без окна и вернуть то, что он напечатал.
fn powershell(script: &str) -> Option<String> {
    use base64::Engine;
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let mut cmd = Command::new(windir.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &encoded]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // без окна консоли
    }
    let out = cmd.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Значок для окна: PNG в виде data URL. Файл значка — exe/ico или готовая картинка PNG.
pub fn icon_data_url(file: &Path, index: i32) -> Option<String> {
    use base64::Engine;
    let is_png = file.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
    let png = if is_png { std::fs::read(file).ok().filter(|d| d.len() < 150_000)? } else { winsys::icon_png(file, index, 48)? };
    Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png)))
}

/// Куда идёт соединение программы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// К нашему прокси — через VPN.
    Vpn,
    /// К другой программе на этом же компьютере (не в интернет) — неважно.
    Local,
    /// В домашнюю сеть (роутер, принтер) — не в интернет, настоящий адрес наружу не уходит.
    Lan,
    /// В интернет напрямую — мимо VPN.
    Direct,
}

pub fn route(remote: IpAddr, remote_port: u16, proxy_port: Option<u16>) -> Route {
    if remote.is_loopback() {
        return if Some(remote_port) == proxy_port { Route::Vpn } else { Route::Local };
    }
    if remote.is_unspecified() {
        return Route::Local;
    }
    let lan = match remote {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80,
    };
    if lan { Route::Lan } else { Route::Direct }
}

/// Что сейчас с программой.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// Сколько её процессов запущено.
    pub processes: usize,
    /// Сколько из них запустили мы (через VPN) — сами или их дочерние.
    pub ours: usize,
    /// Соединения через VPN.
    pub via_vpn: usize,
    /// Соединения напрямую в интернет: адреса (не больше пяти) и всего.
    pub direct: Vec<String>,
    pub direct_total: usize,
    /// Номера процессов программы — чтобы закрыть её.
    pub pids: Vec<u32>,
    /// На какие порты нашего прокси настроены её процессы (из строки запуска).
    pub ports: Vec<u16>,
}

/// Порт нашего прокси из строки запуска: `--proxy-server=http://127.0.0.1:2080`.
pub fn proxy_port(command_line: &str) -> Option<u16> {
    let rest = &command_line[command_line.find("--proxy-server=http://127.0.0.1:")? + 32..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Снимок системы: процессы и соединения. Берётся один раз на круг сторожа.
pub struct Snapshot {
    procs: Vec<Proc>,
    conns: Vec<TcpConn>,
    /// Пути exe — только для процессов с подходящими именами (узнавать путь у всех — долго).
    paths: HashMap<u32, PathBuf>,
    /// Порт нашего прокси из строки запуска — у тех из них, кого запустили через VPN.
    flagged: HashMap<u32, u16>,
}

impl Snapshot {
    /// `exes` — какие программы нас интересуют.
    pub fn take(exes: &[PathBuf]) -> Self {
        let names: HashSet<String> =
            exes.iter().filter_map(|e| e.file_name()).map(|n| n.to_string_lossy().to_lowercase()).collect();
        let procs = winsys::processes();
        let paths: HashMap<u32, PathBuf> = procs
            .iter()
            .filter(|p| names.contains(&p.name.to_lowercase()))
            .filter_map(|p| winsys::image_path(p.pid).map(|path| (p.pid, path)))
            .collect();
        let flagged = paths
            .keys()
            .filter_map(|pid| winsys::command_line(*pid).as_deref().and_then(proxy_port).map(|port| (*pid, port)))
            .collect();
        Self { procs, conns: winsys::tcp_connections(), paths, flagged }
    }

    /// Процессы этой программы.
    pub fn pids_of(&self, exe: &Path) -> Vec<u32> {
        self.paths.iter().filter(|(_, p)| same_path(p, exe)).map(|(pid, _)| *pid).collect()
    }

    /// Запущен ли процесс через VPN: сам он или кто-то из его «родителей» есть в `launched`
    /// или запущен с нашим прокси в строке запуска. Второй признак нужен приложениям из
    /// Microsoft Store (их запускает Windows, а не мы) и после перезапуска окна ninja-vpn.
    pub fn is_ours(&self, pid: u32, launched: &HashSet<u32>) -> bool {
        self.our_port(pid, launched).is_some()
    }

    /// Как `is_ours`, но с портом прокси (0 — порт неизвестен: запустили мы, строку не прочли).
    fn our_port(&self, pid: u32, launched: &HashSet<u32>) -> Option<u16> {
        let parent: HashMap<u32, u32> = self.procs.iter().map(|p| (p.pid, p.parent)).collect();
        let mut current = pid;
        for _ in 0..16 {
            if let Some(port) = self.flagged.get(&current) {
                return Some(*port);
            }
            if launched.contains(&current) {
                return Some(0);
            }
            match parent.get(&current) {
                Some(&p) if p != 0 && p != current => current = p,
                _ => return None,
            }
        }
        None
    }

    /// Что делает программа: сколько процессов, чьи они, куда подключаются.
    pub fn observe(&self, exe: &Path, launched: &HashSet<u32>, proxy_port: Option<u16>) -> Observed {
        let pids = self.pids_of(exe);
        let set: HashSet<u32> = pids.iter().copied().collect();
        let ports: Vec<u16> = pids.iter().filter_map(|p| self.our_port(*p, launched)).collect();
        let mut seen = Observed { processes: pids.len(), ours: ports.len(), ..Default::default() };
        seen.ports = ports.into_iter().filter(|p| *p != 0).collect::<HashSet<_>>().into_iter().collect();
        for c in self.conns.iter().filter(|c| set.contains(&c.pid) && c.state != TcpState::Other) {
            match route(c.remote, c.remote_port, proxy_port) {
                Route::Vpn => seen.via_vpn += 1,
                Route::Direct => {
                    seen.direct_total += 1;
                    let addr = format!("{}:{}", c.remote, c.remote_port);
                    if seen.direct.len() < 5 && !seen.direct.contains(&addr) {
                        seen.direct.push(addr);
                    }
                }
                Route::Local | Route::Lan => {}
            }
        }
        seen.pids = pids;
        seen
    }
}

/// Закрыть программу (все её процессы). Возвращает, сколько удалось.
pub fn close(pids: &[u32]) -> usize {
    pids.iter().filter(|p| winsys::terminate(**p)).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ninja-apps-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn squirrel_launcher_resolves_to_newest_app_folder() {
        let dir = tmp("squirrel");
        for v in ["app-1.0.9", "app-1.0.10", "app-0.9.99"] {
            std::fs::create_dir_all(dir.join(v)).unwrap();
            std::fs::write(dir.join(v).join("Discord.exe"), b"").unwrap();
        }
        std::fs::write(dir.join("Update.exe"), b"").unwrap();
        let found = resolve(&dir.join("Update.exe"), "--processStart Discord.exe").unwrap();
        assert_eq!(found.exe, dir.join("app-1.0.10").join("Discord.exe"));
        assert_eq!(found.args, "");
        let found = resolve(&dir.join("Update.exe"), r#"--processStart "Discord.exe" --process-start-args "--start-minimized""#).unwrap();
        assert_eq!(found.args, "--start-minimized");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plain_exe_and_missing_program() {
        let dir = tmp("plain");
        std::fs::write(dir.join("app.exe"), b"").unwrap();
        assert_eq!(resolve(&dir.join("app.exe"), "-x").unwrap(), Launch { exe: dir.join("app.exe"), args: "-x".into(), aumid: None });
        assert_eq!(resolve(&dir.join("gone.exe"), ""), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn detects_chromium_by_engine_files() {
        let dir = tmp("chromium");
        std::fs::write(dir.join("app.exe"), b"").unwrap();
        assert_eq!(support(&dir.join("app.exe")), Support::Other);
        std::fs::create_dir_all(dir.join("154.0.1.2")).unwrap();
        std::fs::write(dir.join("154.0.1.2").join("resources.pak"), b"").unwrap();
        assert_eq!(support(&dir.join("app.exe")), Support::Chromium, "движок в папке версии");
        std::fs::remove_dir_all(dir.join("154.0.1.2")).unwrap();
        std::fs::write(dir.join("icudtl.dat"), b"").unwrap();
        assert_eq!(support(&dir.join("app.exe")), Support::Chromium);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_windows_and_own_parts() {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        assert!(refuse(&Path::new(&windir).join(r"System32\cmd.exe")).is_some());
        assert!(refuse(Path::new(r"C:\Apps\engines\xray.exe")).is_some());
        assert!(refuse(Path::new(r"C:\Apps\readme.txt")).is_some());
        assert!(refuse(Path::new(r"C:\Program Files\Discord\Discord.exe")).is_none());
        // «C:\WindowsApps» — не папка Windows, хоть и начинается так же.
        assert!(refuse(Path::new(&format!("{windir}Apps\\x.exe"))).is_none());
    }

    #[test]
    fn routes() {
        let p = Some(2080);
        assert_eq!(route(IpAddr::V4(Ipv4Addr::LOCALHOST), 2080, p), Route::Vpn);
        assert_eq!(route(IpAddr::V6(Ipv6Addr::LOCALHOST), 2080, p), Route::Vpn);
        assert_eq!(route(IpAddr::V4(Ipv4Addr::LOCALHOST), 9222, p), Route::Local);
        assert_eq!(route(IpAddr::V4(Ipv4Addr::LOCALHOST), 2080, None), Route::Local);
        assert_eq!(route("192.168.31.1".parse().unwrap(), 80, p), Route::Lan);
        assert_eq!(route("fe80::1".parse().unwrap(), 80, p), Route::Lan);
        assert_eq!(route("162.159.135.232".parse().unwrap(), 443, p), Route::Direct);
        assert_eq!(route("2606:4700::1".parse().unwrap(), 443, p), Route::Direct);
        // Адрес провайдерской сети (CGNAT) — это уже интернет, не домашняя сеть.
        assert_eq!(route("100.64.1.1".parse().unwrap(), 443, p), Route::Direct);
    }

    #[test]
    fn proxy_port_from_command_line() {
        assert_eq!(proxy_port(r#""C:\a\Code.exe" --proxy-server=http://127.0.0.1:2080 --x"#), Some(2080));
        assert_eq!(proxy_port("app.exe --proxy-server=http://127.0.0.1:51234"), Some(51234));
        assert_eq!(proxy_port("app.exe --proxy-server=http://10.0.0.1:2080"), None);
        assert_eq!(proxy_port("app.exe"), None);
    }

    #[test]
    fn arg_values() {
        assert_eq!(arg_value("--processStart Discord.exe", "--processStart").as_deref(), Some("Discord.exe"));
        assert_eq!(arg_value(r#"--processstart "Slack App.exe" -x"#, "--processStart").as_deref(), Some("Slack App.exe"));
        assert_eq!(arg_value("--other", "--processStart"), None);
    }

    #[test]
    fn store_round_trip() {
        let dir = tmp("store");
        let store = AppStore::open(&dir);
        assert!(store.load().is_empty());
        let app = App {
            id: "a1".into(),
            name: "Discord".into(),
            target: PathBuf::from(r"C:\x\Update.exe"),
            args: "--processStart Discord.exe".into(),
            icon: None,
            autostart: true,
            locked: vec![],
            package: None,
        };
        store.save(std::slice::from_ref(&app)).unwrap();
        assert_eq!(store.load(), vec![app]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Настоящий запуск: программа получает адрес прокси, сторож видит её процесс как «наш»
    /// и её соединение с прокси как «через VPN».
    #[cfg(windows)]
    #[test]
    fn launched_process_is_ours_and_uses_proxy() {
        // Наш «прокси» — просто слушающий порт.
        let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = proxy.local_addr().unwrap().port();
        // Программа: PowerShell подключается к адресу из HTTPS_PROXY и ждёт.
        let windir = std::env::var("WINDIR").unwrap();
        let ps = PathBuf::from(windir).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let script = "$u=[uri]$env:HTTPS_PROXY; $c=New-Object Net.Sockets.TcpClient($u.Host,$u.Port); Start-Sleep 20";
        let pid = launch(&Launch { exe: ps.clone(), args: format!("-NoProfile -Command \"{script}\""), aumid: None }, port).unwrap();
        let (_conn, _) = proxy.accept().unwrap();

        let launched = HashSet::from([pid]);
        let snap = Snapshot::take(std::slice::from_ref(&ps));
        assert!(snap.pids_of(&ps).contains(&pid));
        assert!(snap.is_ours(pid, &launched));
        let seen = snap.observe(&ps, &launched, Some(port));
        assert!(seen.via_vpn >= 1, "соединение через прокси не замечено: {seen:?}");
        assert_eq!(close(&[pid]), 1);
    }
}
