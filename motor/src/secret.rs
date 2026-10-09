use std::fmt;

/// Секрет: UUID пользователя, ссылка подписки, пароль.
///
/// При печати показывается как `***`, поэтому случайный `println!("{profile:?}")`
/// или запись в журнал не раскроет ключ. Настоящее значение отдаёт только [`Secret::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Настоящее значение — только туда, где оно действительно нужно (конфиг ядра, запрос подписки).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}
