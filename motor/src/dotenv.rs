//! Чтение файла `.env`: строки `КЛЮЧ=значение`, `#` — комментарий.
//! Там лежит ссылка-подписка; в Git файл не попадает.

use std::collections::HashMap;
use std::path::Path;

/// Пустые значения пропускаем — как будто ключа нет.
pub fn read(path: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else { return HashMap::new() };
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').trim_matches('\'').to_string()))
        .filter(|(_, v)| !v.is_empty())
        .collect()
}
