//! «С телефона»: ключ или подписку можно отправить с телефона, отсканировав QR-код.
//!
//! Ключ обычно приходит на телефон (в мессенджере), а набирать длинный `vless://…` на
//! компьютере неудобно. Пока окно с QR-кодом открыто, программа держит маленькую
//! веб-страницу в домашней сети: `http://<адрес ПК в Wi-Fi>:<порт>/p/<секрет>`. Телефон в том
//! же Wi-Fi открывает её камерой, человек вставляет ключ — он появляется в «Профилях».
//!
//! Защита: страница только на адресе домашней сети (не на VPN и не на всех картах сразу);
//! в адресе — одноразовый случайный секрет (128 бит), без него — «не найдено»; после
//! [`MAX_BAD`] запросов с чужим адресом страница закрывается; живёт не дольше [`LIFETIME`];
//! запрос не больше 64 КБ, медленный клиент обрывается через 5 с. Ключ идёт по домашней сети
//! без шифрования (у домашнего адреса нет сертификата) — об этом честно сказано в окне.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::t;
use crate::winsys;

/// Сколько живёт страница для телефона.
pub const LIFETIME: Duration = Duration::from_secs(10 * 60);
/// Столько запросов с неверным секретом — и страница закрывается (кто-то перебирает адреса).
pub const MAX_BAD: u32 = 30;
const MAX_BODY: usize = 64 * 1024;
const MAX_HEAD: usize = 16 * 1024;

/// Что сделать с присланной ссылкой: добавить источник. `Ok` — его название, `Err` — почему нет.
pub type OnLink = dyn Fn(&str) -> Result<String, String> + Send + Sync;

/// Открытая страница для телефона. Закрывается в [`Pairing::stop`] или при удалении.
pub struct Pairing {
    /// Адрес для телефона (он же в QR-коде).
    pub url: String,
    /// QR-код этого адреса, картинка SVG.
    pub qr_svg: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Pairing {
    /// Открыть страницу в домашней сети. `on_link` вызывается на каждую присланную ссылку.
    pub fn start(on_link: Box<OnLink>) -> Result<Pairing, String> {
        let ip = *winsys::lan_ipv4().first().ok_or_else(|| t!("pair.no_lan"))?;
        Self::start_on(ip, on_link)
    }

    /// То же на заданном адресе (тесты — на 127.0.0.1).
    pub fn start_on(ip: Ipv4Addr, on_link: Box<OnLink>) -> Result<Pairing, String> {
        let listener = TcpListener::bind((ip, 0)).map_err(|e| t!("pair.bind_failed", why = e))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let secret: String = winsys::random_bytes::<16>().iter().map(|b| format!("{b:02x}")).collect();
        let url = format!("http://{ip}:{port}/p/{secret}");
        let qr_svg = qr(&url)?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || serve(listener, &secret, &flag, &*on_link));
        Ok(Pairing { url, qr_svg, stop, thread: Some(thread) })
    }

    /// Страница ещё открыта (не истекло время, не закрыли за перебор)?
    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Pairing {
    fn drop(&mut self) {
        self.stop();
    }
}

fn qr(text: &str) -> Result<String, String> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(code
        .render::<svg::Color>()
        .min_dimensions(232, 232)
        .dark_color(svg::Color("#14283a"))
        .light_color(svg::Color("#ffffff"))
        .quiet_zone(true)
        .build())
}

fn serve(listener: TcpListener, secret: &str, stop: &AtomicBool, on_link: &OnLink) {
    let deadline = Instant::now() + LIFETIME;
    let mut bad = 0u32;
    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline && bad < MAX_BAD {
        match listener.accept() {
            Ok((stream, _)) => {
                if !handle(stream, secret, on_link) {
                    bad += 1;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// Ответить на один запрос. `false` — запрос с неверным секретом (считаем перебор).
fn handle(stream: TcpStream, secret: &str, on_link: &OnLink) -> bool {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return true,
    });
    let Some(request) = read_request(&mut reader) else {
        return true;
    };
    let mut stream = stream;
    let expected = format!("/p/{secret}");
    if !same(&request.path, &expected) {
        // Иконку сайта браузер телефона просит сам — это не перебор.
        let honest = request.path == "/favicon.ico";
        respond(&mut stream, 404, "text/plain; charset=utf-8", "not found");
        return honest;
    }
    let page = match request.method.as_str() {
        "GET" => page(None),
        "POST" => {
            let link = form_value(&request.body, "link").unwrap_or_default();
            let link = link.trim();
            if link.is_empty() {
                page(Some(Err(t!("pair.page_empty"))))
            } else {
                page(Some(on_link(link)))
            }
        }
        _ => {
            respond(&mut stream, 405, "text/plain; charset=utf-8", "method not allowed");
            return true;
        }
    };
    respond(&mut stream, 200, "text/html; charset=utf-8", &page);
    true
}

struct Request {
    method: String,
    path: String,
    body: String,
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut head = Vec::new();
    let mut line = String::new();
    let mut content_length = 0usize;
    let mut first: Option<String> = None;
    loop {
        line.clear();
        let n = reader.by_ref().take((MAX_HEAD - head.len().min(MAX_HEAD)) as u64).read_line(&mut line).ok()?;
        if n == 0 {
            return None;
        }
        head.extend_from_slice(line.as_bytes());
        if head.len() >= MAX_HEAD {
            return None;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if first.is_none() {
            first = Some(trimmed.to_string());
        } else if let Some((name, value)) = trimmed.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().ok()?;
        }
    }
    if content_length > MAX_BODY {
        return None;
    }
    let first = first?;
    let mut parts = first.split(' ');
    let method = parts.next()?.to_string();
    let path = parts.next()?.split('?').next()?.to_string();
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).ok()?;
    Some(Request { method, path, body: String::from_utf8_lossy(&body).into_owned() })
}

/// Сравнение секрета за одно и то же время, сколько бы букв ни совпало.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Поле формы `application/x-www-form-urlencoded`: «+» — пробел, остальное — %XX.
fn form_value(body: &str, name: &str) -> Option<String> {
    body.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == name).then(|| percent_encoding::percent_decode_str(&v.replace('+', " ")).decode_utf8_lossy().into_owned())
    })
}

fn respond(stream: &mut TcpStream, code: u16, kind: &str, body: &str) {
    let status = match code {
        200 => "OK",
        404 => "Not Found",
        _ => "Method Not Allowed",
    };
    let head = format!(
        "HTTP/1.1 {code} {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\
         Referrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\nX-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; form-action 'self'\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// Страница для телефона: поле для ключа и итог прошлой отправки.
fn page(result: Option<Result<String, String>>) -> String {
    let lang = crate::i18n::lang();
    let dir = if lang == "fa" { "rtl" } else { "ltr" };
    let note = match result {
        None => String::new(),
        Some(Ok(name)) => format!("<p class=\"ok\">{}</p>", escape(&t!("pair.page_done", name = name))),
        Some(Err(why)) => format!("<p class=\"bad\">{}</p>", escape(&t!("pair.page_error", why = why))),
    };
    format!(
        r#"<!doctype html>
<html lang="{lang}" dir="{dir}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<title>{title}</title>
<style>
:root {{ --ink: #1d3448; --muted: #5f7a8c; --bg: #e4f2f9; --card: rgba(255,255,255,.72); --line: rgba(120,160,180,.45); --accent: #1c9fae; }}
@media (prefers-color-scheme: dark) {{ :root {{ --ink: #e4eef4; --muted: #8ba3b4; --bg: #0c1822; --card: rgba(255,255,255,.06); --line: rgba(160,200,220,.25); --accent: #5fd0db; }} }}
* {{ box-sizing: border-box; }}
body {{ margin: 0; min-height: 100vh; padding: 24px 16px; font: 16px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif; color: var(--ink); background: var(--bg); }}
main {{ max-width: 520px; margin: 0 auto; padding: 22px 18px; border-radius: 22px; background: var(--card); border: 1px solid var(--line); }}
h1 {{ margin: 0 0 8px; font-size: 22px; font-weight: 600; }}
p {{ margin: 0 0 14px; color: var(--muted); }}
textarea {{ width: 100%; min-height: 140px; padding: 12px; border-radius: 14px; border: 1px solid var(--line); background: transparent; color: var(--ink); font: 14px/1.4 ui-monospace, Consolas, monospace; word-break: break-all; direction: ltr; text-align: left; }}
button {{ width: 100%; margin-top: 12px; padding: 14px; border: 0; border-radius: 999px; background: var(--accent); color: #fff; font-size: 17px; font-weight: 600; }}
.ok, .bad {{ padding: 10px 12px; border-radius: 12px; color: var(--ink); }}
.ok {{ background: rgba(61,191,125,.18); }}
.bad {{ background: rgba(196,111,123,.2); }}
</style>
</head>
<body>
<main>
<h1>{title}</h1>
<p>{lead}</p>
{note}
<form method="post">
<textarea name="link" placeholder="vless://…  https://…" autocapitalize="off" autocomplete="off" autocorrect="off" spellcheck="false" required></textarea>
<button type="submit">{send}</button>
</form>
</main>
</body>
</html>"#,
        title = escape(&t!("pair.page_title")),
        lead = escape(&t!("pair.page_lead")),
        send = escape(&t!("pair.page_send")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::Mutex;

    fn request(addr: SocketAddr, raw: &str) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(raw.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn phone_sends_a_key_only_with_the_secret() {
        let got = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = got.clone();
        let pairing = Pairing::start_on(
            Ipv4Addr::LOCALHOST,
            Box::new(move |link| {
                sink.lock().unwrap().push(link.to_string());
                if link.starts_with("vless://") { Ok("Свой".into()) } else { Err("не ключ".into()) }
            }),
        )
        .unwrap();
        assert!(pairing.qr_svg.starts_with("<?xml") || pairing.qr_svg.contains("<svg"));
        let url = url::Url::parse(&pairing.url).unwrap();
        let addr: SocketAddr = format!("127.0.0.1:{}", url.port().unwrap()).parse().unwrap();
        let path = url.path().to_string();
        assert_eq!(path.len(), "/p/".len() + 32, "секрет — 16 байт");

        // Без секрета — «не найдено», ключ не принят.
        let wrong = request(addr, "GET /p/00000000000000000000000000000000 HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(wrong.starts_with("HTTP/1.1 404"), "{wrong}");
        // Со секретом — страница с формой.
        let form = request(addr, &format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n"));
        assert!(form.starts_with("HTTP/1.1 200") && form.contains("<form method=\"post\">"), "{form}");
        assert!(form.contains("Referrer-Policy: no-referrer"));
        // Отправка: «+» и %XX раскодируются.
        let body = "link=vless%3A%2F%2Fa%40b.example.com%3A443%3Fsecurity%3Dtls%23%D0%9C%D0%BE%D0%B9+%D1%81%D0%B5%D1%80%D0%B2%D0%B5%D1%80";
        let sent = request(addr, &format!("POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
        assert!(sent.contains("class=\"ok\""), "{sent}");
        let bad = request(addr, &format!("POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: 9\r\n\r\nlink=abcd"));
        assert!(bad.contains("class=\"bad\"") && bad.contains("не ключ"), "{bad}");
        assert_eq!(*got.lock().unwrap(), vec!["vless://a@b.example.com:443?security=tls#Мой сервер".to_string(), "abcd".to_string()]);
        drop(pairing);
    }

    #[test]
    fn closes_after_too_many_wrong_secrets() {
        let pairing = Pairing::start_on(Ipv4Addr::LOCALHOST, Box::new(|_| Ok(String::new()))).unwrap();
        let port = url::Url::parse(&pairing.url).unwrap().port().unwrap();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        for i in 0..MAX_BAD {
            request(addr, &format!("GET /p/{i:032} HTTP/1.1\r\n\r\n"));
        }
        std::thread::sleep(Duration::from_millis(400));
        assert!(!pairing.is_running(), "после перебора страница закрыта");
    }

    #[test]
    fn page_escapes_what_it_shows() {
        let html = page(Some(Err("<script>x</script>".into())));
        assert!(!html.contains("<script>x") && html.contains("&lt;script&gt;"));
        assert_eq!(form_value("a=1&link=x+y%2B", "link").as_deref(), Some("x y+"));
        assert!(same("abc", "abc") && !same("abc", "abd") && !same("abc", "ab"));
    }
}

#[cfg(all(test, windows))]
mod live {
    /// Только смотрит: `cargo test -p ninja-motor lan_now -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn lan_now() {
        println!("адреса в домашней сети: {:?}", crate::winsys::lan_ipv4());
    }
}
