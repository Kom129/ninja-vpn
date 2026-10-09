//! Язык сообщений мотора: ошибки и предупреждения, которые видит человек.
//!
//! Фразы — в `motor/locales/<язык>.json`, русский (`ru.json`) — основной: по нему проверка
//! сверяет остальные. В коде: `t!("ключ")` или `t!("ключ", name = значение)` — в тексте
//! фразы это подстановка `{name}`. Язык один на всю программу: окно сообщает его командой
//! `set_language`; командная строка `ninja` остаётся на русском.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Языки по порядку; 0 — русский, на него падаем, если фразы нет.
pub const LANGS: [&str; 7] = ["ru", "en", "es", "pt", "tr", "zh", "fa"];

const SOURCES: [&str; 7] = [
    include_str!("../locales/ru.json"),
    include_str!("../locales/en.json"),
    include_str!("../locales/es.json"),
    include_str!("../locales/pt.json"),
    include_str!("../locales/tr.json"),
    include_str!("../locales/zh.json"),
    include_str!("../locales/fa.json"),
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

fn catalog(index: usize) -> &'static HashMap<String, String> {
    static CACHE: [OnceLock<HashMap<String, String>>; 7] = [const { OnceLock::new() }; 7];
    CACHE[index].get_or_init(|| serde_json::from_str(SOURCES[index]).expect("словарь мотора — правильный JSON"))
}

/// Сменить язык сообщений: `ru`, `en`, `es`, `pt`, `tr`, `zh`, `fa`. Незнакомый — `false`, язык прежний.
pub fn set_lang(code: &str) -> bool {
    match LANGS.iter().position(|l| *l == code) {
        Some(index) => {
            CURRENT.store(index, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

/// Текущий язык сообщений.
pub fn lang() -> &'static str {
    LANGS[CURRENT.load(Ordering::Relaxed)]
}

fn lookup(index: usize, key: &str) -> &'static str {
    catalog(index).get(key).or_else(|| catalog(0).get(key)).map_or("", String::as_str)
}

/// Фраза на текущем языке (без подстановок). Нет такой фразы — сам ключ, чтобы было видно, чего не хватает.
pub fn text(key: &str) -> String {
    let found = lookup(CURRENT.load(Ordering::Relaxed), key);
    if found.is_empty() { key.to_string() } else { found.to_string() }
}

/// Фраза на заданном языке — для проверок, не меняя язык всей программы.
pub fn text_in(lang: &str, key: &str) -> String {
    lookup(LANGS.iter().position(|l| *l == lang).unwrap_or(0), key).to_string()
}

/// Фраза с подстановками: `{name}` заменяется значением. За один проход — значение,
/// в котором случайно есть `{…}`, не подменится повторно.
pub fn fill(key: &str, args: &[(&str, String)]) -> String {
    let template = text(key);
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template.as_str();
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}').map(|close| (&after[..close], close)) {
            Some((name, close)) if args.iter().any(|(n, _)| *n == name) => {
                out.push_str(&args.iter().find(|(n, _)| *n == name).expect("проверили выше").1);
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `t!("ключ")` или `t!("ключ", name = значение, …)` — фраза на текущем языке.
#[macro_export]
macro_rules! t {
    ($key:literal) => {
        $crate::i18n::text($key)
    };
    ($key:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::fill($key, &[$((stringify!($name), ($value).to_string())),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    fn placeholders(text: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else { break };
            found.insert(after[..close].to_string());
            rest = &after[close + 1..];
        }
        found
    }

    /// Во всех языках — те же фразы, что в русском, и с теми же подстановками.
    #[test]
    fn every_language_has_every_phrase() {
        let ru = catalog(0);
        for (index, lang) in LANGS.iter().enumerate().skip(1) {
            let other = catalog(index);
            for (key, text) in ru {
                let translated = other.get(key).unwrap_or_else(|| panic!("{lang}: нет фразы «{key}»"));
                assert!(!translated.trim().is_empty(), "{lang}: пустая фраза «{key}»");
                assert_eq!(placeholders(text), placeholders(translated), "{lang}: подстановки в «{key}»");
            }
            for key in other.keys() {
                assert!(ru.contains_key(key), "{lang}: лишняя фраза «{key}» (нет в ru.json)");
            }
        }
    }

    /// Каждый `t!("ключ")` в коде мотора и окна есть в словаре — опечатка в ключе не пройдёт.
    #[test]
    fn every_key_in_code_exists() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut files = Vec::new();
        for dir in ["motor/src", "motor/src/bin", "app/src-tauri/src"] {
            for entry in std::fs::read_dir(root.join(dir)).unwrap().flatten() {
                if entry.path().extension().is_some_and(|e| e == "rs") {
                    files.push(entry.path());
                }
            }
        }
        let ru = catalog(0);
        let mut used = 0;
        for file in files {
            let code = std::fs::read_to_string(&file).unwrap();
            let marker = "t!(\"";
            for (at, _) in code.match_indices(marker) {
                // «format!(», «print!(» тоже кончаются на «t!(» — перед нашим t! не буква и не цифра.
                if code[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let part = &code[at + marker.len()..];
                let key = &part[..part.find('"').unwrap()];
                if key == "ключ" {
                    continue; // примеры в комментариях
                }
                used += 1;
                assert!(ru.contains_key(key), "{}: нет фразы «{key}» в ru.json", file.display());
            }
        }
        assert!(used > 50, "нашлось подозрительно мало вызовов t!: {used}");
    }

    #[test]
    fn fills_placeholders_once() {
        let text = fill("tun.other_vpn", &[("name", "{name}".to_string())]);
        assert!(text.contains("«{name}»"), "{text}");
        assert!(!text.contains("{name}{name}"));
    }

    #[test]
    fn unknown_language_keeps_current() {
        assert!(!set_lang("xx"));
        assert_eq!(text_in("en", "store.not_found"), "source not found");
    }
}
