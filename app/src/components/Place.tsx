// «Окно в страну»: вместо флага в правом кружке — красивое место страны выхода.
// До подключения окно запотевшее, пока подключаемся — оттаивает, на «Подключено» становится
// чистым. Флаг остаётся маленьким значком в углу. Нет картинки для страны — показываем флаг, как раньше.
//
// Картинки: src/assets/places/<код страны>.webp (nl.webp, de.webp…) и world.webp — для
// «Оптимальной локации» и стран без своей картинки. Готовит их scripts/build-places.mjs.

import { Flag } from './Flag';

const FILES = import.meta.glob('../assets/places/*.webp', { eager: true, import: 'default' }) as Record<string, string>;
const BY_CODE: Record<string, string> = Object.fromEntries(
  Object.entries(FILES).map(([file, url]) => [file.match(/([\w-]+)\.webp$/)![1].toUpperCase(), url]),
);

/** Картинка места для страны; `null` — картинки нет. Без страны — «весь мир». */
export function placeImage(code: string | null): string | null {
  if (!code) return BY_CODE.WORLD ?? null;
  return BY_CODE[code.toUpperCase()] ?? null;
}

interface Props {
  /** Код страны (NL, DE…) или `null`, если страна неизвестна (балансировщик до подключения). */
  code: string | null;
  /** Значок группы, если нет ни страны, ни картинки «весь мир» (🚀 для «Оптимальной»). */
  fallbackIcon: string;
  /** Название страны для подсказки. */
  label: string;
}

export function Place({ code, fallbackIcon, label }: Props) {
  const image = placeImage(code);
  if (!image) {
    return code ? <Flag code={code} className="orb-flag" /> : <span className="orb-emoji">{fallbackIcon}</span>;
  }
  return (
    <>
      <span className="orb-place" role="img" aria-label={label}>
        <img src={image} alt="" draggable={false} />
      </span>
      {code && (
        <span className="orb-badge">
          <Flag code={code} className="orb-badge-flag" />
        </span>
      )}
    </>
  );
}
