// Языки окна. Словари — в соседних файлах: ru.ts — основной (по нему сверяются остальные),
// en, es, pt, tr, zh, fa — переводы. TypeScript не соберёт программу, если в каком-то
// словаре не хватает фразы или есть лишняя.
//
// Как пользоваться в компоненте:  const { t } = useI18n();  t('status.connected')
// Подстановки: t('cap.connecting', { engine: 'Xray' }) — в тексте «Запускаю ядро {engine}».
// Числа подставляются в записи языка (у персидского — свои цифры).
// Множественное число: в словаре объект { one, few, many, other }, а в вызове — { n: 5 }.

import { createContext, useContext } from 'react';
import { ru } from './ru';
import { en } from './en';
import { es } from './es';
import { pt } from './pt';
import { tr } from './tr';
import { zh } from './zh';
import { fa } from './fa';

export type Lang = 'ru' | 'en' | 'es' | 'pt' | 'tr' | 'zh' | 'fa';
export type Key = keyof typeof ru;

/** Формы множественного числа (как их называет Unicode: one — «1 сервер», few — «3 сервера»…). */
export type Plural = { other: string } & Partial<Record<'zero' | 'one' | 'two' | 'few' | 'many', string>>;
/** Словарь любого языка: те же ключи, что в русском; где в русском формы числа — там тоже. */
export type Dict = { [K in Key]: (typeof ru)[K] extends string ? string : Plural };
export type Params = Record<string, string | number>;

export interface LangInfo {
  id: Lang;
  /** Название на самом языке — так его узнают в списке. */
  name: string;
  /** Полное обозначение для Intl (даты, числа, названия стран). */
  locale: string;
  dir: 'ltr' | 'rtl';
}

export const LANGS: LangInfo[] = [
  { id: 'ru', name: 'Русский', locale: 'ru-RU', dir: 'ltr' },
  { id: 'en', name: 'English', locale: 'en-US', dir: 'ltr' },
  { id: 'es', name: 'Español', locale: 'es-ES', dir: 'ltr' },
  { id: 'pt', name: 'Português', locale: 'pt-BR', dir: 'ltr' },
  { id: 'tr', name: 'Türkçe', locale: 'tr-TR', dir: 'ltr' },
  { id: 'zh', name: '中文（简体）', locale: 'zh-CN', dir: 'ltr' },
  { id: 'fa', name: 'فارسی', locale: 'fa-IR', dir: 'rtl' },
];

const DICTS: Record<Lang, Dict> = { ru: ru as unknown as Dict, en, es, pt, tr, zh, fa };

/** Язык по настройкам Windows (WebView2 сообщает их через navigator.languages). */
export function detectLang(): Lang {
  const prefs = typeof navigator === 'undefined' ? [] : navigator.languages ?? [navigator.language];
  for (const pref of prefs) {
    const base = pref.toLowerCase().split('-')[0];
    if (LANGS.some((l) => l.id === base)) return base as Lang;
    // Соседние языки, на которых чаще понимают один из наших.
    if (['be', 'kk', 'ky', 'tg', 'uz', 'hy'].includes(base)) return 'ru';
    if (base === 'az') return 'tr';
    if (base === 'gl' || base === 'ca') return 'es';
  }
  return 'en';
}

export interface I18n {
  lang: Lang;
  info: LangInfo;
  t: (key: Key, params?: Params) => string;
  /** Число в записи языка: 1 234,5 · 1,234.5 · ۱٬۲۳۴٫۵ */
  num: (n: number, digits?: number) => string;
}

function fill(text: string, params: Params | undefined, numbers: Intl.NumberFormat): string {
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (whole, name: string) => {
    const value = params[name];
    if (value === undefined) return whole;
    return typeof value === 'number' ? numbers.format(value) : value;
  });
}

export function makeI18n(lang: Lang): I18n {
  const info = LANGS.find((l) => l.id === lang) ?? LANGS[0];
  const dict = DICTS[info.id];
  const numbers = new Intl.NumberFormat(info.locale, { maximumFractionDigits: 1 });
  const plurals = new Intl.PluralRules(info.locale);
  const t = (key: Key, params?: Params): string => {
    const entry = dict[key] ?? DICTS.ru[key];
    if (typeof entry === 'string') return fill(entry, params, numbers);
    const n = Number(params?.n ?? 0);
    const form = plurals.select(n) as keyof Plural;
    return fill(entry[form] ?? entry.other, params, numbers);
  };
  const num = (n: number, digits = 1) => new Intl.NumberFormat(info.locale, { maximumFractionDigits: digits }).format(n);
  return { lang: info.id, info, t, num };
}

const I18nContext = createContext<I18n>(makeI18n('ru'));
export const I18nProvider = I18nContext.Provider;
export const useI18n = () => useContext(I18nContext);
