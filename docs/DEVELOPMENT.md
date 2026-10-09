# Разработка ninja-vpn

Как собрать, запустить и проверить программу из исходников. Что умеет программа — в [README](../README.md).

## Что нужно

- Windows 10/11, 64 бита
- [Rust](https://rustup.rs) (stable) и Visual Studio Build Tools (C++)
- [Node.js](https://nodejs.org) 22+
- WebView2 (в Windows 11 уже есть)

Все команды — из папки проекта.

## Как запустить

1. Скачать ядра sing-box и Xray (один раз, с проверкой SHA-256):
   `powershell -ExecutionPolicy Bypass -File scripts\fetch-engines.ps1`
2. Добавить подписку или ключ: `cargo run -q --bin ninja -- add` → вставить ссылку → Enter.
   Или в окне: «+ Добавить подписку или ключ».
3. Серверы: `cargo run -q --bin ninja -- list`, скорость всех: `… -- probe`.
4. Подключиться по номеру или части имени: `… -- connect 19`, `… -- connect нидерл`.
   `--browser` откроет обычный браузер через VPN. Все команды: `… -- help`.
5. Тесты: `cargo test`, типы окна: `cd app && npx tsc --noEmit`.
6. **Окно:** `cd app`, один раз `npm install`, потом `npm run tauri dev`.
   Только вёрстка в обычном браузере (без мотора, с плашкой «Демо»): `npm run dev` → http://127.0.0.1:1420
   Если пишет «beforeDevCommand terminated» — порт 1420 занят старым Vite: `Get-NetTCPConnection -LocalPort 1420`.
7. **Установщик:** `cd app && npm run tauri build` → `target\release\bundle\nsis\ninja-vpn_<версия>_x64-setup.exe`.
   Ставится для текущего пользователя (без прав администратора), ядра — внутри, рабочие файлы — в `%LOCALAPPDATA%\com.ninjavpn.app\runtime`.
   Сменил версию ядра в `engines/engines.json` — поменяй и пути в `bundle.resources` в `app/src-tauri/tauri.conf.json`.

Подписки и ключи хранятся зашифрованными (DPAPI) в `%APPDATA%\com.ninjavpn.app` — общее хранилище окна и командной строки.

## Живые проверки настоящего окна

Сценарии в `app/scripts/*.mjs` управляют окном через отладочный порт WebView2. Запуск окна с портом:

```powershell
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS='--remote-debugging-port=9333'; npm run tauri dev
```

Отладочный порт включай только для проверки.

| Сценарий | Что проверяет |
|---|---|
| `live-reserve-test.mjs` | резерв, падение и зависание ядра, отключение; `--soak 30` — 30 минут подключения |
| `leak-test.mjs [сервер]` | утечки режима «Браузер»: адрес у сайтов, IPv6, WebRTC, DNS, прямые соединения, обрыв ядра |
| `apps-live-test.mjs` | «Приложения → только эти»: программа через VPN и «нарушитель» |
| `tun-live-test.mjs` | «все, кроме этих» — **только** с `NINJA_TUN_TEST_ROUTES=1.1.1.1/32,1.0.0.1/32` (TUN забирает лишь два адреса) |
| `device-live-test.mjs` | «Устройство»: `SERVER=Netherlands HOLD=90 node scripts/device-live-test.mjs` (другие VPN выключить, «Да» в окне Windows) |
| `device-switch-test.mjs` | смена страны в «Устройстве» на лету: TUN не пропадает, нового «Да» нет, адрес выхода меняется |
| `switch-live-test.mjs` | смена сервера во время подключения в режиме «Браузер» |
| `other-vpn-check.mjs`, `other-vpn-later-check.mjs` | другой VPN на весь компьютер: предупреждение до подключения и уступка, если его включили после |
| `browser-restart-watch.mjs` | наблюдение за обычным браузером: через VPN ли его соединения после перезапуска |

## Из чего состоит

| Папка / файл | Для чего |
|---|---|
| `motor/` | «Мотор» на Rust: профили, разбор ключей и подписок, конфиги и запуск sing-box / Xray, проверка передачи |
| `motor/src/bin/ninja.rs` | Командная строка мотора |
| `motor/src/tun.rs` | TUN для «Устройства» и «все, кроме этих»; помощник с правами администратора; «включён ли другой VPN» |
| `motor/src/apps.rs`, `lnk.rs`, `winsys.rs` | Программы: меню «Пуск», Microsoft Store, запуск с ключом прокси, процессы и соединения |
| `motor/src/browser.rs` | Обычный браузер через VPN: как запущен, перезапуск с восстановлением вкладок |
| `motor/src/i18n.rs`, `motor/locales/` | Сообщения мотора на 7 языках |
| `app/` | Окно: Tauri 2 + React/TypeScript. `src/App.tsx` — экран, `src/ninja/` — сцена с ниндзя, `src-tauri/src/lib.rs` — мост к мотору |
| `app/src/i18n/` | Языки окна: `ru.ts` — основной словарь, остальные TypeScript сверяет с ним |
| `app/src/styles.css`, `app/src/theme.ts` | Стекло, светлая и тёмная тема (цвета — только переменными) |
| `engines/engines.json` | Закреплённые версии sing-box и Xray и их SHA-256 |
| `scripts/fetch-engines.ps1` | Скачивает ядра и сверяет контрольные суммы |
| `docs/` | Исходный промт, исследование, спецификация, дизайн-бриф, план проверок |
| `ASSETS.md` | Откуда картинки, иконки и ядра, их лицензии |

## Технологии и почему

- **Tauri 2 + React/TypeScript** — окно: лёгкое, на веб-технологиях, удобно для стеклянного дизайна и анимации на Canvas.
- **Rust** — мотор: надёжный, и Tauri написан на нём же.
- **sing-box** — готовое сетевое ядро (VLESS и TUN). Свой протокол не изобретаем.
- **Xray** — второе ядро, только для XHTTP, которого sing-box не умеет. Мотор сам выбирает ядро для каждого сервера.
