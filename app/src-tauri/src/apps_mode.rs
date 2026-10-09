//! Режим «Приложения» в окне: список программ, запуск через VPN и сторож.
//!
//! Сторож раз в 1,5 с смотрит, запущены ли программы из списка, мы ли их запустили
//! и куда идут их соединения — через VPN (к нашему прокси) или напрямую. Итог уходит
//! в окно событием `apps-status`, только когда что-то изменилось.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::MutexGuard;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ninja_motor::apps::{self, App, Snapshot, StorePackage, Support};
use ninja_motor::{lnk, t, winsys};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::{AppState, Link, lock};

/// Что режим помнит, пока окно открыто.
#[derive(Default)]
pub struct Apps {
    pub list: Vec<App>,
    /// Кого запустили мы: id программы → номера процессов и порт прокси при запуске.
    launched: HashMap<String, Launched>,
    /// Сторож включён — выбран режим «Приложения».
    guard: bool,
    /// Последний итог сторожа (для окна после перезагрузки).
    last: Vec<AppStatus>,
}

impl Apps {
    pub fn new(list: Vec<App>) -> Self {
        Self { list, ..Self::default() }
    }
}

struct Launched {
    pids: HashSet<u32>,
    port: u16,
}

pub fn apps_lock(state: &AppState) -> MutexGuard<'_, Apps> {
    state.apps.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Программа для окна.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppView {
    id: String,
    name: String,
    icon: Option<String>,
    autostart: bool,
    /// Какой exe сейчас запустится (у Discord он меняется после обновлений).
    exe: Option<String>,
    support: Option<Support>,
    problem: Option<String>,
}

fn app_view(app: &App) -> AppView {
    let found = apps::resolve_app(app);
    AppView {
        id: app.id.clone(),
        name: app.name.clone(),
        icon: app.icon.clone(),
        autostart: app.autostart,
        support: found.as_ref().map(|f| apps::support(&f.exe)),
        problem: found.is_none().then(|| t!("apps.missing_moved")),
        exe: found.map(|f| f.exe.display().to_string()),
    }
}

/// Установленная программа для окна «Добавить».
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramView {
    name: String,
    target: String,
    args: String,
    exe: String,
    icon: Option<String>,
    support: Support,
    /// Приложение из Microsoft Store.
    package: Option<StorePackage>,
}

#[tauri::command]
pub fn list_apps(state: tauri::State<AppState>) -> Vec<AppView> {
    apps_lock(&state).list.iter().map(app_view).collect()
}

/// Программы из меню «Пуск» (значки достаём здесь же — это доли секунды на программу).
#[tauri::command]
pub async fn installed_programs() -> Result<Vec<ProgramView>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        apps::installed()
            .into_iter()
            .map(|p| ProgramView {
                icon: apps::icon_data_url(&p.icon_file, p.icon_index).or_else(|| apps::icon_data_url(&p.exe, 0)),
                support: apps::support(&p.exe),
                name: p.name,
                target: p.target.display().to_string(),
                args: p.args,
                exe: p.exe.display().to_string(),
                package: p.package,
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

/// Окно Windows «Открыть файл»: программа (.exe) или ярлык (.lnk). `None` — человек передумал.
#[tauri::command]
pub async fn pick_program() -> Result<Option<ProgramView>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Some(file) = winsys::pick_program(&t!("apps.pick_title")) else { return Ok(None) };
        let is_lnk = file.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
        let (target, args, icon) = if is_lnk {
            let data = std::fs::read(&file).map_err(|e| e.to_string())?;
            let s = lnk::parse(&data).ok_or_else(|| t!("apps.shortcut_not_program"))?;
            (s.target, s.arguments, s.icon)
        } else {
            (file.clone(), String::new(), None)
        };
        let found = apps::resolve(&target, &args).ok_or_else(|| t!("apps.shortcut_target_missing"))?;
        if let Some(why) = apps::refuse(&found.exe) {
            return Err(t!("apps.cannot_add", why = why));
        }
        let name = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let icon = icon
            .filter(|(f, _)| f.is_file())
            .and_then(|(f, i)| apps::icon_data_url(&f, i))
            .or_else(|| apps::icon_data_url(&found.exe, 0));
        Ok(Some(ProgramView {
            name,
            target: target.display().to_string(),
            args,
            support: apps::support(&found.exe),
            exe: found.exe.display().to_string(),
            icon,
            package: None,
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn add_app(
    state: tauri::State<AppState>,
    name: String,
    target: String,
    args: String,
    icon: Option<String>,
    package: Option<StorePackage>,
) -> Result<AppView, String> {
    let target = PathBuf::from(target);
    let found = match &package {
        Some(p) => apps::resolve_app(&App { package: Some(p.clone()), ..App::default() }),
        None => apps::resolve(&target, &args),
    }
    .ok_or_else(|| t!("apps.not_found"))?;
    if let Some(why) = apps::refuse(&found.exe) {
        return Err(t!("apps.cannot_add", why = why));
    }
    let mut guard = apps_lock(&state);
    let same = guard
        .list
        .iter()
        .find(|a| apps::resolve_app(a).is_some_and(|f| apps::same_path(&f.exe, &found.exe)));
    if let Some(existing) = same {
        return Err(t!("apps.already_listed", name = existing.name));
    }
    let name = name.trim();
    let app = App {
        id: format!("app{:x}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()),
        name: if name.is_empty() { t!("apps.default_name") } else { name.to_string() },
        // Значок от окна — тот, что человек видел в списке; иначе достаём из программы.
        icon: icon.filter(|i| i.starts_with("data:image/png;base64,") && i.len() < 200_000).or_else(|| apps::icon_data_url(&found.exe, 0)),
        target,
        args,
        autostart: true,
        locked: Vec::new(),
        package,
    };
    guard.list.push(app.clone());
    state.app_store.save(&guard.list).map_err(|e| t!("apps.save_failed", why = e))?;
    Ok(app_view(&app))
}

#[tauri::command]
pub fn remove_app(state: tauri::State<AppState>, id: String) -> Result<(), String> {
    let mut guard = apps_lock(&state);
    guard.list.retain(|a| a.id != id);
    guard.launched.remove(&id);
    state.app_store.save(&guard.list).map_err(|e| t!("apps.save_failed", why = e))
}

#[tauri::command]
pub fn set_app_autostart(state: tauri::State<AppState>, id: String, on: bool) -> Result<(), String> {
    let mut guard = apps_lock(&state);
    let app = guard.list.iter_mut().find(|a| a.id == id).ok_or_else(|| t!("apps.not_in_list"))?;
    app.autostart = on;
    state.app_store.save(&guard.list).map_err(|e| t!("apps.save_failed", why = e))
}

/// Окно выбрало режим «Приложения» (или ушло из него).
#[tauri::command]
pub fn set_apps_guard(state: tauri::State<AppState>, on: bool) {
    let mut guard = apps_lock(&state);
    guard.guard = on;
    if !on {
        guard.last.clear();
    }
}

#[tauri::command]
pub fn apps_status(state: tauri::State<AppState>) -> Vec<AppStatus> {
    apps_lock(&state).last.clone()
}

/// Порт работающего VPN.
fn vpn_port(state: &AppState) -> Option<u16> {
    match &lock(state).link {
        Link::Up { session, .. } => Some(session.listen_port),
        _ => None,
    }
}

/// Итог запуска одной программы.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchView {
    id: String,
    /// Уже была запущена не через VPN: чтобы пустить её через VPN, нужно перезапустить.
    running_direct: bool,
    error: Option<String>,
}

/// Запустить программы через VPN. Уже работающие через VPN не трогаем.
#[tauri::command]
pub fn launch_apps(state: tauri::State<AppState>, ids: Vec<String>) -> Result<Vec<LaunchView>, String> {
    let port = vpn_port(&state).ok_or_else(|| t!("app.connect_first"))?;
    let mut guard = apps_lock(&state);
    let chosen: Vec<App> = guard.list.iter().filter(|a| ids.contains(&a.id)).cloned().collect();
    let exes: Vec<PathBuf> = chosen.iter().filter_map(apps::resolve_app).map(|f| f.exe).collect();
    let snap = Snapshot::take(&exes);
    let mut results = Vec::new();
    for app in chosen {
        let mut result = LaunchView { id: app.id.clone(), running_direct: false, error: None };
        let Some(found) = apps::resolve_app(&app) else {
            result.error = Some(t!("apps.not_found_short"));
            results.push(result);
            continue;
        };
        let ours = guard.launched.get(&app.id).filter(|l| l.port == port).map(|l| l.pids.clone()).unwrap_or_default();
        let running = snap.pids_of(&found.exe);
        let foreign = running.iter().filter(|p| !snap.is_ours(**p, &ours)).count();
        if foreign > 0 {
            // Программа уже открыта сама по себе: новый запуск просто покажет её окно,
            // а ходить она продолжит напрямую. Честно говорим и предлагаем перезапуск.
            result.running_direct = true;
        } else if running.is_empty() {
            match apps::launch(&found, port) {
                Ok(pid) => {
                    guard.launched.insert(app.id.clone(), Launched { pids: HashSet::from([pid]), port });
                }
                Err(e) => result.error = Some(t!("apps.start_failed_short", why = e)),
            }
        }
        results.push(result);
    }
    Ok(results)
}

/// Закрыть все процессы программы и дождаться, пока они исчезнут (до 6 с).
fn close_and_wait(exe: &std::path::Path) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let pids = Snapshot::take(std::slice::from_ref(&exe.to_path_buf())).pids_of(exe);
        if pids.is_empty() {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(t!("apps.did_not_close"));
        }
        apps::close(&pids);
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Перезапустить программу через VPN: закрыть (все её окна) и открыть заново.
#[tauri::command]
pub async fn restart_app(app: AppHandle, id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let port = vpn_port(&state).ok_or_else(|| t!("app.connect_first"))?;
        let entry = apps_lock(&state).list.iter().find(|a| a.id == id).cloned().ok_or_else(|| t!("apps.not_in_list"))?;
        let found = apps::resolve_app(&entry).ok_or_else(|| t!("apps.not_found"))?;
        close_and_wait(&found.exe)?;
        // Сразу после закрытия некоторые программы (VS Code) ещё секунду держат «я уже запущена»:
        // новый запуск передаёт управление уходящей копии и тут же завершается. Поэтому пауза,
        // а если программа так и не появилась — ещё одна попытка.
        for attempt in 0..2 {
            std::thread::sleep(Duration::from_millis(if attempt == 0 { 800 } else { 1500 }));
            let pid = apps::launch(&found, port).map_err(|e| t!("apps.start_failed", why = e))?;
            apps_lock(&state).launched.insert(id.clone(), Launched { pids: HashSet::from([pid]), port });
            let launched = HashSet::from([pid]);
            for _ in 0..16 {
                std::thread::sleep(Duration::from_millis(250));
                let snap = Snapshot::take(std::slice::from_ref(&found.exe));
                if snap.pids_of(&found.exe).iter().any(|p| snap.is_ours(*p, &launched)) {
                    return Ok(());
                }
            }
        }
        Err(t!("apps.did_not_start"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn close_app(app: AppHandle, id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let entry = apps_lock(&state).list.iter().find(|a| a.id == id).cloned().ok_or_else(|| t!("apps.not_in_list"))?;
        let found = apps::resolve_app(&entry).ok_or_else(|| t!("apps.not_found"))?;
        close_and_wait(&found.exe)?;
        apps_lock(&state).launched.remove(&id);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Что сейчас с программой — для окна.
#[derive(Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    id: String,
    /// Сколько процессов программы запущено (0 — не запущена).
    processes: usize,
    /// Сколько из них запустили мы через VPN.
    ours: usize,
    /// Наши процессы настроены на текущий порт VPN (после отключения и нового подключения
    /// на другом порту их нужно перезапустить).
    port_ok: bool,
    via_vpn: usize,
    /// Соединения напрямую в интернет (до пяти адресов) и сколько их всего.
    direct: Vec<String>,
    direct_total: usize,
}

/// Сторож: раз в 1,5 с — итог по всем программам списка, в окно — только изменения.
pub fn spawn_guard(app: AppHandle) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(1500));
            let state = app.state::<AppState>();
            let port = vpn_port(&state);
            let (list, launched) = {
                let guard = apps_lock(&state);
                if !guard.guard || guard.list.is_empty() {
                    continue;
                }
                let launched: HashMap<String, (HashSet<u32>, u16)> =
                    guard.launched.iter().map(|(id, l)| (id.clone(), (l.pids.clone(), l.port))).collect();
                (guard.list.clone(), launched)
            };
            let resolved: Vec<(String, Option<PathBuf>)> =
                list.iter().map(|a| (a.id.clone(), apps::resolve_app(a).map(|f| f.exe))).collect();
            let exes: Vec<PathBuf> = resolved.iter().filter_map(|(_, e)| e.clone()).collect();
            let snap = Snapshot::take(&exes);
            let empty = (HashSet::new(), 0);
            let statuses: Vec<AppStatus> = resolved
                .iter()
                .map(|(id, exe)| {
                    let (pids, launched_port) = launched.get(id).unwrap_or(&empty);
                    let seen = exe.as_ref().map(|e| snap.observe(e, pids, port)).unwrap_or_default();
                    AppStatus {
                        id: id.clone(),
                        processes: seen.processes,
                        ours: seen.ours,
                        port_ok: seen.ours > 0 && port.is_some_and(|p| *launched_port == p || seen.ports.contains(&p)),
                        via_vpn: seen.via_vpn,
                        direct: seen.direct,
                        direct_total: seen.direct_total,
                    }
                })
                .collect();
            let mut guard = apps_lock(&state);
            // Программа закрылась — забываем её процессы (номера потом достанутся другим).
            // Только если за время проверки её не запустили заново.
            for s in statuses.iter().filter(|s| s.processes == 0) {
                let unchanged = guard.launched.get(&s.id).map(|l| &l.pids) == launched.get(&s.id).map(|(p, _)| p);
                if unchanged {
                    guard.launched.remove(&s.id);
                }
            }
            if guard.guard && guard.last != statuses {
                guard.last = statuses.clone();
                let _ = app.emit("apps-status", statuses);
            }
        }
    });
}
