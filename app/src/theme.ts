// Оформление окна: светлое или тёмное стекло. «Как в Windows» — следим за темой Windows
// (WebView2 сообщает её через prefers-color-scheme). Сами цвета — в styles.css, здесь только
// атрибут data-theme на <html>, по которому включается тёмный набор переменных.

export type ThemeChoice = 'system' | 'light' | 'dark';
export const THEMES: ThemeChoice[] = ['system', 'light', 'dark'];
export const THEME_KEY = 'ninja.theme';

const systemDark = () => window.matchMedia('(prefers-color-scheme: dark)').matches;

/** Выбор из памяти окна — чтобы включить тему до первой отрисовки (иначе тёмное окно мигнёт белым). */
export function storedTheme(): ThemeChoice {
  try {
    const value = JSON.parse(localStorage.getItem(THEME_KEY) ?? 'null');
    return THEMES.includes(value) ? value : 'system';
  } catch {
    return 'system';
  }
}

export function applyTheme(choice: ThemeChoice) {
  document.documentElement.dataset.theme = choice === 'system' ? (systemDark() ? 'dark' : 'light') : choice;
}

/** Применить выбор; для «Как в Windows» — ещё и следить за сменой темы Windows. Возвращает отписку. */
export function followTheme(choice: ThemeChoice): () => void {
  applyTheme(choice);
  if (choice !== 'system') return () => {};
  const query = window.matchMedia('(prefers-color-scheme: dark)');
  const onChange = () => applyTheme('system');
  query.addEventListener('change', onChange);
  return () => query.removeEventListener('change', onChange);
}
