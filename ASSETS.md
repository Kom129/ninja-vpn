# Откуда взяты картинки и ресурсы

| Файл | Что это | Источник | Права |
|---|---|---|---|
| `docs/design/selected-glass.png` | Референс окна (в открытую копию не входит — неизвестно происхождение) | Из архива автора | — |
| `docs/design/ninja-concepts/styles-abc.png` | Три стиля ниндзя (выбран A) | Генерация OpenAI через Codex CLI (gpt-6-astra), 2026-10-03 | У пользователя по условиям OpenAI |
| `docs/design/ninja-concepts/concept-a.png` | Вырезка стиля A — образец для следующих генераций | Из `styles-abc.png` | То же |
| `docs/design/ninja-concepts/run-cycle.png` | 6 кадров бега, прозрачный фон | Генерация OpenAI через Codex CLI, 2026-10-03 | То же |
| `docs/design/ninja-concepts/poses.png` | Позы idle, jump, slide, deliver, sad | Генерация OpenAI через Codex CLI, 2026-10-03 | То же |
| `docs/design/ninja-concepts/dodges.png` | Увороты ниндзя: сальто, уклон, рывок (+ стоящая для масштаба) | Генерация OpenAI через Codex CLI (gpt-6-astra), 2026-10-05 | У пользователя по условиям OpenAI |
| `docs/design/ninja-concepts/guards.png` | Страж в маске: стоит, бросается, спотыкается, упал | То же | То же |
| `docs/design/ninja-concepts/caught.png` | Страж поймал ниндзя: держит за шарф, тащит под мышкой | То же | То же |
| `app/src/assets/ninja/*.webp`, `ninja.json` | Готовые кадры для приложения | Нарезаны `app/scripts/build-sprites.mjs` из листов выше | То же |
| `docs/design/places/styles-nl.png` | «Окно в страну»: Нидерланды в трёх стилях (выбран A — мягкое 3D) | Генерация OpenAI через Codex CLI (gpt-6-astra), 2026-10-06 | У пользователя по условиям OpenAI |
| `docs/design/places/ref-style-a.png` | Вырезка стиля A — образец для генерации стран | Из `styles-nl.png` | То же |
| `docs/design/places/archive/*.webp` | Места стран (768 px) и «весь мир»; исходные PNG лежат только на диске | Генерация OpenAI через Codex CLI (gpt-6-astra), `app/scripts/gen-places.mjs`, 2026-10-06 | То же |
| `app/src/assets/places/*.webp` | Картинки для кружка страны (256 px) | `app/scripts/build-places.mjs` из картинок выше | То же |
| `docs/design/icon/icon-a.png` | Иконка приложения (выбрана): лицо ниндзя на стеклянной плитке | Генерация OpenAI через Codex CLI (gpt-6-astra), 2026-10-04, образец — `concept-a.png` | У пользователя по условиям OpenAI |
| `docs/design/icon/icon-b.png` | Запасной вариант иконки: ниндзя в прыжке с пакетом | То же | То же |
| `app/src-tauri/icons/*` | Все размеры иконки (.ico, PNG) | `app/scripts/build-icon.mjs` чистит `icon-a.png` → `npx tauri icon` | То же |
| Флаги стран | SVG-флаги | npm `country-flag-icons` | MIT |
| Иконки | Иконки интерфейса | npm `lucide-react` | ISC |
| sing-box 1.14.2 | Сетевое ядро (скачивается, в Git не хранится) | github.com/SagerNet/sing-box | GPL-3.0-or-later |
| Xray 26.3.27 | Сетевое ядро для XHTTP (скачивается, в Git не хранится) | github.com/XTLS/Xray-core | MPL-2.0 |

Пересобрать кадры: `cd app && node scripts/build-sprites.mjs` (контрольный лист — `docs/design/ninja-concepts/sprites-preview.png`).
