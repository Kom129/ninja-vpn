// Разбор названий серверов из подписки в понятный вид.
// «Germany 92 🇩🇪 → [📃 Белые списки] CDN резерв» →
//   страна DE, заголовок «Германия 92», группа «Белые списки», значок 📃, примечание «CDN резерв».
// Windows не рисует эмодзи-флаги (показывает буквы), поэтому флаги рисуем картинками.

/** Группа серверов без своей группы в подписке — «Страны» (подпись берётся из словаря). */
export const COUNTRIES = '__countries__';

export interface ServerName {
  country: string | null;
  title: string;
  group: string | null;
  groupIcon: string | null;
  note: string | null;
}

const REGIONAL = /[\u{1F1E6}-\u{1F1FF}]{2}/u;
const PICTOGRAPHS = /[\p{Extended_Pictographic}\u{1F1E6}-\u{1F1FF}\u{FE0F}\u{200D}]/gu;

// Названия стран — из справочника, встроенного в WebView2, на языке окна: «Германия»,
// «Germany», «Alemania», «德国»… Язык меняет окно (setNamesLocale), переводить вручную не нужно.
let regionNames = new Intl.DisplayNames(['ru'], { type: 'region' });
// Для опознания кода в начале имени («DE-1») — всегда английский справочник, не зависящий от языка.
const regionCheck = new Intl.DisplayNames(['en'], { type: 'region' });

export function setNamesLocale(locale: string) {
  regionNames = new Intl.DisplayNames([locale, 'en'], { type: 'region' });
}

/** Код страны → «Германия» на языке окна. Для неизвестного кода — сам код. */
export function countryName(code: string): string {
  try {
    return regionNames.of(code.toUpperCase()) ?? code;
  } catch {
    return code;
  }
}

function knownRegion(code: string): boolean {
  try {
    return regionCheck.of(code) !== code;
  } catch {
    return false;
  }
}

function flagToCode(text: string): string | null {
  const match = text.match(REGIONAL);
  if (!match) return null;
  return Array.from(match[0])
    .map((c) => String.fromCharCode(c.codePointAt(0)! - 0x1f1e6 + 65))
    .join('');
}

const clean = (s: string) => s.replace(PICTOGRAPHS, '').replace(/\s+/g, ' ').trim();

/** Названия стран и крупные города → код. Нужны, когда в имени сервера нет флага:
 *  «Germany-1», «Frankfurt 2», «Нидерланды». Ключи — в нижнем регистре. */
const NAMES: Record<string, string> = Object.fromEntries(
  Object.entries({
    NL: 'netherlands holland amsterdam нидерланды голландия амстердам',
    DE: 'germany deutschland frankfurt berlin munich germany германия франкфурт берлин',
    US: 'usa united states america new york los angeles chicago dallas miami seattle сша америка',
    FR: 'france paris франция париж',
    FI: 'finland helsinki финляндия хельсинки',
    PL: 'poland warsaw krakow польша варшава',
    TR: 'turkey türkiye turkiye istanbul турция стамбул',
    AE: 'uae emirates dubai оаэ эмираты дубай',
    JP: 'japan tokyo osaka япония токио',
    KZ: 'kazakhstan almaty astana казахстан алматы астана',
    RU: 'russia moscow россия москва',
    AL: 'albania tirana албания',
    UA: 'ukraine kyiv kiev украина киев',
    AU: 'australia sydney melbourne австралия сидней',
    HK: 'hong kong hongkong гонконг',
    IN: 'india mumbai delhi индия',
    AR: 'argentina buenos aires аргентина',
    BR: 'brazil sao paulo são paulo бразилия',
    CA: 'canada toronto montreal vancouver канада',
    SG: 'singapore сингапур',
    MD: 'moldova chisinau молдова',
    NG: 'nigeria lagos нигерия',
    GB: 'united kingdom uk britain england london великобритания англия лондон',
    SE: 'sweden stockholm швеция стокгольм',
    CH: 'switzerland zurich швейцария цюрих',
    AT: 'austria vienna австрия вена',
    LV: 'latvia riga латвия рига',
    LT: 'lithuania vilnius литва вильнюс',
    EE: 'estonia tallinn эстония таллин',
    ES: 'spain madrid barcelona испания мадрид',
    IT: 'italy milan rome италия милан рим',
    CZ: 'czechia czech republic prague чехия прага',
  }).flatMap(([code, words]) => {
    // Названия из двух слов («new york», «hong kong») — целиком, остальные — по одному слову.
    const phrases = ['new york', 'los angeles', 'united states', 'hong kong', 'buenos aires', 'sao paulo', 'são paulo', 'united kingdom', 'czech republic'];
    const found = phrases.filter((p) => words.includes(p));
    const rest = found.reduce((w, p) => w.replace(p, ' '), words).split(/\s+/).filter(Boolean);
    return [...found, ...rest].map((name) => [name, code]);
  }),
);
const NAME_LIST = Object.keys(NAMES).sort((a, b) => b.length - a.length);

/** Страна по словам в имени сервера («Germany-1», «DE Frankfurt»). `null` — не узнали. */
function nameToCode(text: string): string | null {
  const lower = ` ${text.toLowerCase().replace(/[^\p{L}\p{N}]+/gu, ' ')} `;
  const word = NAME_LIST.find((name) => lower.includes(` ${name} `));
  if (word) return NAMES[word];
  // Код страны в начале: «DE-1», «NL Amsterdam», «US2». Только заглавными и известный Windows.
  const iso = text.trim().match(/^([A-Z]{2})(?=[\s\-_|.:\d]|$)/)?.[1];
  if (iso && knownRegion(iso)) return iso;
  return null;
}

export function parseServerName(raw: string): ServerName {
  const flag = flagToCode(raw);
  const country = flag ?? nameToCode(raw.split('→')[0]);
  const [head, tail = ''] = raw.split('→').map((s) => s.trim());
  const bracket = tail.match(/\[([^\]]*)\]\s*(.*)$/);
  const groupRaw = bracket ? bracket[1] : tail || null;
  const groupIcon = groupRaw ? (groupRaw.match(/\p{Extended_Pictographic}/u)?.[0] ?? null) : null;
  const group = groupRaw ? clean(groupRaw) || null : null;
  const note = bracket && bracket[2] ? clean(bracket[2]) : null;
  const headClean = clean(head);
  const number = headClean.match(/\s(\d+)$/)?.[1];

  let title: string;
  // По флагу — «Германия 92»; узнали по словам («Frankfurt 2») — имя оставляем как есть.
  if (flag) title = countryName(flag) + (number ? ` ${number}` : '');
  else if (/^auto\b/i.test(headClean) && group) title = group + (number ? ` ${number}` : '');
  else title = headClean || raw;
  return { country, title, group, groupIcon, note };
}

/** Короткое имя протокола для подписи: «VLESS · XHTTP · REALITY» → «XHTTP». */
export function transportOf(summary: string): string {
  return summary.split(' · ')[1] ?? summary;
}
