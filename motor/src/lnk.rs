//! Ярлыки Windows (`.lnk`): на какую программу указывают.
//!
//! Список «установленных программ» для режима «Приложения» берём из меню «Пуск», а там лежат
//! ярлыки. Формат ярлыка описан Microsoft (MS-SHLLINK); нам нужны только путь к программе,
//! параметры запуска и откуда взять значок.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    pub target: PathBuf,
    /// Параметры запуска как есть, одной строкой (с кавычками внутри).
    pub arguments: String,
    /// Файл со значком и номер значка в нём.
    pub icon: Option<(PathBuf, i32)>,
}

const HEADER_SIZE: usize = 0x4C;
const HAS_ID_LIST: u32 = 0x1;
const HAS_LINK_INFO: u32 = 0x2;
const HAS_NAME: u32 = 0x4;
const HAS_RELATIVE_PATH: u32 = 0x8;
const HAS_WORKING_DIR: u32 = 0x10;
const HAS_ARGUMENTS: u32 = 0x20;
const HAS_ICON_LOCATION: u32 = 0x40;
const IS_UNICODE: u32 = 0x80;
/// В LinkInfo есть локальный путь.
const LOCAL_BASE_PATH: u32 = 0x1;
/// Блок с путём через переменные окружения: `%LOCALAPPDATA%\…`.
const ENVIRONMENT_BLOCK: u32 = 0xA000_0001;
/// То же для значка.
const ICON_ENVIRONMENT_BLOCK: u32 = 0xA000_0007;

/// Чтение чисел без паники: испорченный ярлык — просто `None`.
struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        Some(u16::from_le_bytes(self.0.get(at..at + 2)?.try_into().ok()?))
    }
    fn u32(&self, at: usize) -> Option<u32> {
        Some(u32::from_le_bytes(self.0.get(at..at + 4)?.try_into().ok()?))
    }
    /// Строка UTF-16 до нуля, не дальше `max` символов.
    fn wide_z(&self, at: usize, max: usize) -> Option<String> {
        let units: Vec<u16> = (0..max).map_while(|i| self.u16(at + i * 2).filter(|&u| u != 0)).collect();
        Some(String::from_utf16_lossy(&units))
    }
    /// Строка в кодировке Windows (у нас cp1251) до нуля.
    fn ansi_z(&self, at: usize) -> Option<String> {
        let rest = self.0.get(at..)?;
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        Some(crate::winsys::ansi_to_string(&rest[..end]))
    }
}

/// Разобрать ярлык. `None` — это не ярлык на файл (например, на папку панели управления
/// или «рекламный» ярлык установщика MSI, у которого нет пути).
pub fn parse(data: &[u8]) -> Option<Shortcut> {
    let b = Bytes(data);
    if b.u32(0)? as usize != HEADER_SIZE {
        return None;
    }
    let flags = b.u32(0x14)?;
    let icon_index = b.u32(0x38)? as i32;
    let mut at = HEADER_SIZE;
    if flags & HAS_ID_LIST != 0 {
        at += 2 + b.u16(at)? as usize;
    }

    let mut target = None;
    if flags & HAS_LINK_INFO != 0 {
        let size = b.u32(at)? as usize;
        let header = b.u32(at + 4)? as usize;
        let info_flags = b.u32(at + 8)?;
        if info_flags & LOCAL_BASE_PATH != 0 {
            // Новые ярлыки хранят путь и в Юникоде (если заголовок длиннее 0x24 байт).
            let path = if header >= 0x24 {
                let offset = b.u32(at + 28)? as usize;
                b.wide_z(at + offset, 32_767)?
            } else {
                b.ansi_z(at + b.u32(at + 16)? as usize)?
            };
            let suffix = b.ansi_z(at + b.u32(at + 24)? as usize).unwrap_or_default();
            target = Some(format!("{path}{suffix}"));
        }
        at += size;
    }

    // Строки идут подряд в фиксированном порядке; читаем все, нужны только две.
    let unicode = flags & IS_UNICODE != 0;
    let mut read_string = |present: bool| -> Option<Option<String>> {
        if !present {
            return Some(None);
        }
        let count = b.u16(at)? as usize;
        let text = if unicode {
            let units: Vec<u16> = (0..count).map(|i| b.u16(at + 2 + i * 2)).collect::<Option<_>>()?;
            at += 2 + count * 2;
            String::from_utf16_lossy(&units)
        } else {
            let raw = data.get(at + 2..at + 2 + count)?;
            at += 2 + count;
            crate::winsys::ansi_to_string(raw)
        };
        Some(Some(text))
    };
    read_string(flags & HAS_NAME != 0)?;
    read_string(flags & HAS_RELATIVE_PATH != 0)?;
    read_string(flags & HAS_WORKING_DIR != 0)?;
    let arguments = read_string(flags & HAS_ARGUMENTS != 0)?.unwrap_or_default();
    let mut icon = read_string(flags & HAS_ICON_LOCATION != 0)?;

    // Дополнительные блоки: путь через переменные окружения точнее сохранённого
    // (у программ в профиле пользователя он не зависит от имени пользователя).
    while let Some(size) = b.u32(at).map(|s| s as usize) {
        if size < 8 {
            break;
        }
        match b.u32(at + 4)? {
            ENVIRONMENT_BLOCK => target = b.wide_z(at + 8 + 260, 260).filter(|s| !s.is_empty()).or(target),
            ICON_ENVIRONMENT_BLOCK => icon = b.wide_z(at + 8 + 260, 260).filter(|s| !s.is_empty()).or(icon),
            _ => {}
        }
        at += size;
    }

    let target = PathBuf::from(expand_env(&target?));
    let icon = icon.filter(|s| !s.is_empty()).map(|s| (PathBuf::from(expand_env(&s)), icon_index));
    Some(Shortcut { target, arguments, icon })
}

/// `%LOCALAPPDATA%\Discord` → `C:\Users\…\AppData\Local\Discord`. Неизвестные переменные оставляем как есть.
pub fn expand_env(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(value) if !name.is_empty() => out.push_str(&value),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Собрать маленький ярлык: заголовок + LinkInfo с путём + строки.
    fn sample(path: &str, args: Option<&str>, icon: Option<&str>) -> Vec<u8> {
        let mut flags = HAS_LINK_INFO | IS_UNICODE;
        if args.is_some() {
            flags |= HAS_ARGUMENTS;
        }
        if icon.is_some() {
            flags |= HAS_ICON_LOCATION;
        }
        let mut data = vec![0u8; HEADER_SIZE];
        data[0..4].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
        data[0x14..0x18].copy_from_slice(&flags.to_le_bytes());
        data[0x38..0x3C].copy_from_slice(&2u32.to_le_bytes());

        // LinkInfo со старым (ANSI) заголовком 0x1C байт: путь сразу за заголовком, суффикс — пустая строка.
        let mut info = vec![0u8; 0x1C];
        info.extend_from_slice(path.as_bytes());
        info.push(0);
        let suffix_at = info.len();
        info.push(0);
        let size = info.len() as u32;
        info[0..4].copy_from_slice(&size.to_le_bytes());
        info[4..8].copy_from_slice(&0x1Cu32.to_le_bytes());
        info[8..12].copy_from_slice(&LOCAL_BASE_PATH.to_le_bytes());
        info[16..20].copy_from_slice(&0x1Cu32.to_le_bytes());
        info[24..28].copy_from_slice(&(suffix_at as u32).to_le_bytes());
        data.extend(info);

        for text in [args, icon].into_iter().flatten() {
            let units: Vec<u16> = text.encode_utf16().collect();
            data.extend((units.len() as u16).to_le_bytes());
            units.iter().for_each(|u| data.extend(u.to_le_bytes()));
        }
        data.extend(0u32.to_le_bytes()); // конец дополнительных блоков
        data
    }

    #[test]
    fn reads_target_arguments_and_icon() {
        let lnk = sample(r"C:\Apps\Discord\Update.exe", Some("--processStart Discord.exe"), Some(r"C:\Apps\Discord\app.ico"));
        let s = parse(&lnk).unwrap();
        assert_eq!(s.target, PathBuf::from(r"C:\Apps\Discord\Update.exe"));
        assert_eq!(s.arguments, "--processStart Discord.exe");
        assert_eq!(s.icon, Some((PathBuf::from(r"C:\Apps\Discord\app.ico"), 2)));
    }

    #[test]
    fn plain_shortcut_without_extras() {
        let s = parse(&sample(r"C:\Program Files\App\app.exe", None, None)).unwrap();
        assert_eq!(s.target, PathBuf::from(r"C:\Program Files\App\app.exe"));
        assert_eq!(s.arguments, "");
        assert_eq!(s.icon, None);
    }

    #[test]
    fn garbage_is_not_a_shortcut() {
        assert_eq!(parse(b"not a shortcut"), None);
        assert_eq!(parse(&[]), None);
        let mut cut = sample(r"C:\a.exe", Some("x"), None);
        cut.truncate(HEADER_SIZE + 10);
        assert_eq!(parse(&cut), None);
    }

    #[test]
    fn expands_known_variables_only() {
        let windir = std::env::var("WINDIR").unwrap_or_default();
        assert_eq!(expand_env(r"%WINDIR%\a"), format!(r"{windir}\a"));
        assert_eq!(expand_env("%NINJA_NO_SUCH_VAR%"), "%NINJA_NO_SUCH_VAR%");
        assert_eq!(expand_env("100%"), "100%");
    }

    /// Настоящие ярлыки меню «Пуск» этого компьютера: разбираются без ошибок,
    /// у большинства есть существующий путь.
    #[test]
    fn real_start_menu_shortcuts() {
        let Some(appdata) = std::env::var_os("APPDATA") else { return };
        let dir = PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs");
        let mut total = 0;
        let mut resolved = 0;
        for file in crate::apps::walk(&dir, 4).into_iter().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk"))) {
            total += 1;
            if parse(&std::fs::read(&file).unwrap()).is_some_and(|s| s.target.exists()) {
                resolved += 1;
            }
        }
        if total > 0 {
            assert!(resolved * 2 >= total, "разобрано {resolved} из {total}");
        }
    }
}
