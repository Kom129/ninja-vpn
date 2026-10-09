//! Обёртки над функциями Windows для режима «Приложения»: какие программы запущены,
//! куда они подключаются, значки программ, окно выбора файла, запуск с правами администратора.
//!
//! Всё небезопасное (`unsafe`) собрано здесь, остальной код работает с обычными типами Rust.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

/// Процесс из списка Windows.
#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: u32,
    pub parent: u32,
    /// Имя файла программы, без папки: `Discord.exe`.
    pub name: String,
}

/// Состояние TCP-соединения, которое нам важно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    /// Соединение открыто, данные ходят.
    Established,
    /// Программа только стучится (ещё не ответили).
    Connecting,
    /// Закрывается, слушает порт и т. п. — нам неважно.
    Other,
}

/// TCP-соединение и процесс, которому оно принадлежит.
#[derive(Debug, Clone)]
pub struct TcpConn {
    pub pid: u32,
    pub remote: IpAddr,
    pub remote_port: u16,
    pub state: TcpState,
}

/// Сетевая карта Windows (как в «Сетевых подключениях»).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    /// Имя карты: «DurevVPN», «Беспроводная сеть».
    pub name: String,
    /// Что это за устройство: «sing-tun Tunnel», «MediaTek Wi-Fi 6 …».
    pub description: String,
    /// Тип по классификации Windows (IANA ifType): 6 — Ethernet, 71 — Wi-Fi, 53 — виртуальная…
    pub if_type: u32,
}

impl Adapter {
    /// Похожа ли карта на VPN (виртуальный туннель), а не на настоящую сеть: Wi-Fi, кабель,
    /// виртуальные машины. Windows сама помечает такие карты типом; у старых VPN на карте TAP
    /// тип «Ethernet» — их узнаём по названию.
    pub fn looks_like_vpn(&self) -> bool {
        const PPP: u32 = 23; // встроенный VPN Windows (PPTP, L2TP, IKEv2)
        const PROP_VIRTUAL: u32 = 53; // Wintun: sing-box, WireGuard, Durev, Hiddify…
        const TUNNEL: u32 = 131;
        if matches!(self.if_type, PPP | PROP_VIRTUAL | TUNNEL) {
            return true;
        }
        let text = format!("{} {}", self.name, self.description).to_lowercase();
        ["vpn", "tap-windows", "wintun", "wireguard", "tunnel", "tun "].iter().any(|word| text.contains(word))
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE, INVALID_HANDLE_VALUE, NO_ERROR};
    use windows_sys::Win32::Globalization::{CP_ACP, MultiByteToWideChar};
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits,
        GetObjectW, HBITMAP, HDC,
    };
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP_STATE_ESTAB, MIB_TCP_STATE_SYN_SENT, MIB_TCP6TABLE_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
        TCP_TABLE_OWNER_PID_ALL,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, INFINITE, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NODEREFERENCELINKS, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHDefExtractIconW, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO, SW_HIDE};

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Описатель Windows, который закроется сам.
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    pub fn ansi_to_string(bytes: &[u8]) -> String {
        if bytes.is_ascii() {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        unsafe {
            let n = MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), bytes.len() as i32, std::ptr::null_mut(), 0);
            let mut out = vec![0u16; n.max(0) as usize];
            MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), bytes.len() as i32, out.as_mut_ptr(), n);
            String::from_utf16_lossy(&out)
        }
    }

    pub fn processes() -> Vec<Proc> {
        let mut list = Vec::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return list;
            }
            let snap = Handle(snap);
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap.0, &mut entry);
            while ok != 0 {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                list.push(Proc {
                    pid: entry.th32ProcessID,
                    parent: entry.th32ParentProcessID,
                    name: String::from_utf16_lossy(&entry.szExeFile[..len]),
                });
                ok = Process32NextW(snap.0, &mut entry);
            }
        }
        list
    }

    pub fn image_path(pid: u32) -> Option<PathBuf> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let h = Handle(h);
            let mut buf = vec![0u16; 32_768];
            let mut size = buf.len() as u32;
            if QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut size) == 0 {
                return None;
            }
            Some(PathBuf::from(OsString::from_wide(&buf[..size as usize])))
        }
    }

    /// Строка запуска процесса (с параметрами) — по ней видно, запущен ли он через наш прокси.
    pub fn command_line(pid: u32) -> Option<String> {
        #[link(name = "ntdll")]
        unsafe extern "system" {
            fn NtQueryInformationProcess(h: HANDLE, class: u32, info: *mut std::ffi::c_void, len: u32, ret: *mut u32) -> i32;
        }
        /// ProcessCommandLineInformation: есть с Windows 8.1.
        const COMMAND_LINE: u32 = 60;
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let h = Handle(h);
            let mut size = 0u32;
            NtQueryInformationProcess(h.0, COMMAND_LINE, std::ptr::null_mut(), 0, &mut size);
            if size == 0 || size > 1 << 20 {
                return None;
            }
            // Ответ: заголовок UNICODE_STRING (длина в байтах + указатель), а за ним сам текст.
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            if NtQueryInformationProcess(h.0, COMMAND_LINE, buf.as_mut_ptr().cast(), size, &mut size) < 0 {
                return None;
            }
            let bytes = *(buf.as_ptr() as *const u16) as usize;
            let text = *(buf.as_ptr().cast::<u8>().add(8) as *const *const u16);
            if text.is_null() {
                return None;
            }
            Some(String::from_utf16_lossy(std::slice::from_raw_parts(text, bytes / 2)))
        }
    }
    pub fn terminate(pid: u32) -> bool {
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if h.is_null() {
                return false;
            }
            let h = Handle(h);
            TerminateProcess(h.0, 1) != 0
        }
    }

    /// Таблица TCP целиком (для IPv4 или IPv6). Буфер из `u64`, чтобы строки таблицы были выровнены.
    fn tcp_table(family: u16) -> Vec<u64> {
        let mut size = 0u32;
        let mut buf: Vec<u64> = Vec::new();
        for _ in 0..5 {
            let rc = unsafe {
                GetExtendedTcpTable(buf.as_mut_ptr().cast(), &mut size, 0, family as u32, TCP_TABLE_OWNER_PID_ALL, 0)
            };
            match rc {
                NO_ERROR => return buf,
                // Соединений стало больше, пока мы готовили буфер — ещё раз, с запасом.
                ERROR_INSUFFICIENT_BUFFER => buf = vec![0u64; (size as usize + 4096) / 8 + 1],
                _ => break,
            }
            size = (buf.len() * 8) as u32;
        }
        Vec::new()
    }

    fn state(raw: u32) -> TcpState {
        match raw as i32 {
            MIB_TCP_STATE_ESTAB => TcpState::Established,
            MIB_TCP_STATE_SYN_SENT => TcpState::Connecting,
            _ => TcpState::Other,
        }
    }

    pub fn tcp_connections() -> Vec<TcpConn> {
        let mut out = Vec::new();
        let v4 = tcp_table(AF_INET);
        if !v4.is_empty() {
            unsafe {
                let table = &*(v4.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
                let rows = std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
                for r in rows {
                    out.push(TcpConn {
                        pid: r.dwOwningPid,
                        remote: IpAddr::from(r.dwRemoteAddr.to_ne_bytes()),
                        remote_port: u16::from_be(r.dwRemotePort as u16),
                        state: state(r.dwState),
                    });
                }
            }
        }
        let v6 = tcp_table(AF_INET6);
        if !v6.is_empty() {
            unsafe {
                let table = &*(v6.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID);
                let rows = std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
                for r in rows {
                    let remote = std::net::Ipv6Addr::from(r.ucRemoteAddr);
                    // «IPv4 внутри IPv6» (::ffff:1.2.3.4) показываем как обычный IPv4.
                    let remote = remote.to_ipv4_mapped().map_or(IpAddr::V6(remote), IpAddr::V4);
                    out.push(TcpConn {
                        pid: r.dwOwningPid,
                        remote,
                        remote_port: u16::from_be(r.dwRemotePort as u16),
                        state: state(r.dwState),
                    });
                }
            }
        }
        out
    }

    /// Сетевые карты, через которые идёт «весь интернет»: у них есть маршрут 0.0.0.0/0
    /// или пара половинок 0.0.0.0/1 + 128.0.0.0/1 (так делают многие VPN). Только включённые.
    pub fn internet_adapters() -> Vec<Adapter> {
        use windows_sys::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
        use windows_sys::Win32::NetworkManagement::IpHelper::{
            FreeMibTable, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GAA_FLAG_SKIP_UNICAST,
            GetAdaptersAddresses, GetIpForwardTable2, IP_ADAPTER_ADDRESSES_LH, MIB_IPFORWARD_TABLE2,
        };
        use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;

        // 1. Номера карт, у которых есть маршрут «весь интернет».
        let mut indexes: Vec<u32> = Vec::new();
        unsafe {
            let mut table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
            if GetIpForwardTable2(AF_INET, &mut table) == NO_ERROR && !table.is_null() {
                let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
                for r in rows {
                    let first = r.DestinationPrefix.Prefix.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes()[0];
                    let whole = match r.DestinationPrefix.PrefixLength {
                        0 => true,
                        1 => first == 0 || first == 128,
                        _ => false,
                    };
                    if whole && !indexes.contains(&r.InterfaceIndex) {
                        indexes.push(r.InterfaceIndex);
                    }
                }
                FreeMibTable(table.cast());
            }
        }
        if indexes.is_empty() {
            return Vec::new();
        }

        // 2. Имена и типы этих карт. Буфер из `u64` — чтобы записи были выровнены.
        let flags = GAA_FLAG_SKIP_UNICAST | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut buf: Vec<u64> = vec![0; 16 * 1024 / 8];
        let mut ok = false;
        for _ in 0..4 {
            let mut size = (buf.len() * 8) as u32;
            let rc = unsafe {
                GetAdaptersAddresses(AF_INET as u32, flags, std::ptr::null(), buf.as_mut_ptr().cast(), &mut size)
            };
            match rc {
                NO_ERROR => {
                    ok = true;
                    break;
                }
                ERROR_BUFFER_OVERFLOW => buf = vec![0; size as usize / 8 + 512],
                _ => break,
            }
        }
        if !ok {
            return Vec::new();
        }
        let text = |p: *const u16| -> String {
            if p.is_null() {
                return String::new();
            }
            unsafe {
                let len = (0..).take_while(|&i| *p.add(i) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
            }
        };
        let mut out = Vec::new();
        let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !p.is_null() {
            let a = unsafe { &*p };
            let index = unsafe { a.Anonymous1.Anonymous.IfIndex };
            if a.OperStatus == IfOperStatusUp && indexes.contains(&index) {
                out.push(Adapter { name: text(a.FriendlyName), description: text(a.Description), if_type: a.IfType });
            }
            p = a.Next;
        }
        out
    }

    /// IPv4-адреса компьютера в домашней сети — на настоящих картах (Wi-Fi, кабель), не на VPN:
    /// по такому адресу телефон в том же Wi-Fi откроет страницу «отправить ключ». Сначала Wi-Fi.
    pub fn lan_ipv4() -> Vec<std::net::Ipv4Addr> {
        use windows_sys::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
        use windows_sys::Win32::NetworkManagement::IpHelper::{
            GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
        };
        use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
        use windows_sys::Win32::Networking::WinSock::SOCKADDR_IN;
        const ETHERNET: u32 = 6;
        const WIFI: u32 = 71;

        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut buf: Vec<u64> = vec![0; 16 * 1024 / 8];
        let mut ok = false;
        for _ in 0..4 {
            let mut size = (buf.len() * 8) as u32;
            let rc = unsafe { GetAdaptersAddresses(AF_INET as u32, flags, std::ptr::null(), buf.as_mut_ptr().cast(), &mut size) };
            match rc {
                NO_ERROR => {
                    ok = true;
                    break;
                }
                ERROR_BUFFER_OVERFLOW => buf = vec![0; size as usize / 8 + 512],
                _ => break,
            }
        }
        if !ok {
            return Vec::new();
        }
        let mut found: Vec<(u32, std::net::Ipv4Addr)> = Vec::new();
        let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !p.is_null() {
            let a = unsafe { &*p };
            if a.OperStatus == IfOperStatusUp && matches!(a.IfType, ETHERNET | WIFI) {
                let mut u = a.FirstUnicastAddress;
                while !u.is_null() {
                    let addr = unsafe { &*u }.Address;
                    if !addr.lpSockaddr.is_null() && unsafe { (*addr.lpSockaddr).sa_family } == AF_INET {
                        let v4 = unsafe { &*(addr.lpSockaddr as *const SOCKADDR_IN) };
                        let ip = std::net::Ipv4Addr::from(unsafe { v4.sin_addr.S_un.S_addr }.to_ne_bytes());
                        if ip.is_private() {
                            found.push((a.IfType, ip));
                        }
                    }
                    u = unsafe { &*u }.Next;
                }
            }
            p = a.Next;
        }
        found.sort_by_key(|(kind, _)| *kind != WIFI);
        found.into_iter().map(|(_, ip)| ip).collect()
    }

    /// Случайные байты из генератора Windows (для одноразовых секретов).
    pub fn random_bytes<const N: usize>() -> [u8; N] {
        use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
        let mut out = [0u8; N];
        let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), out.as_mut_ptr(), N as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        assert!(status >= 0, "генератор случайных чисел Windows не ответил: {status:#x}");
        out
    }

    /// Пиксели картинки Windows (32 бита на точку, сверху вниз), порядок байтов BGRA.
    unsafe fn bitmap_pixels(dc: HDC, bitmap: HBITMAP, width: i32, height: i32) -> Option<Vec<u8>> {
        let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { std::mem::zeroed() }
        };
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let lines = unsafe { GetDIBits(dc, bitmap, 0, height as u32, pixels.as_mut_ptr().cast(), &mut info, DIB_RGB_COLORS) };
        (lines == height).then_some(pixels)
    }

    /// Значок программы картинкой PNG (`size` × `size`). `index` — номер значка в файле.
    pub fn icon_png(file: &Path, index: i32, size: u32) -> Option<Vec<u8>> {
        unsafe {
            let mut icon: HICON = std::ptr::null_mut();
            let path = wide(file.as_os_str());
            if SHDefExtractIconW(path.as_ptr(), index, 0, &mut icon, std::ptr::null_mut(), size) != 0 || icon.is_null() {
                return None;
            }
            let mut info: ICONINFO = std::mem::zeroed();
            let got = GetIconInfo(icon, &mut info);
            DestroyIcon(icon);
            if got == 0 {
                return None;
            }
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            let result = (|| {
                if info.hbmColor.is_null() {
                    return None; // чёрно-белые значки из прошлого века не показываем
                }
                let mut bm: BITMAP = std::mem::zeroed();
                GetObjectW(info.hbmColor, std::mem::size_of::<BITMAP>() as i32, (&raw mut bm).cast());
                let (w, h) = (bm.bmWidth, bm.bmHeight);
                if w <= 0 || h <= 0 || w > 512 || h > 512 {
                    return None;
                }
                let mut px = bitmap_pixels(dc, info.hbmColor, w, h)?;
                // Старые значки без прозрачности: берём её из маски (белое в маске — прозрачно).
                if px.chunks(4).all(|p| p[3] == 0) {
                    let mask = bitmap_pixels(dc, info.hbmMask, w, h)?;
                    for (p, m) in px.chunks_mut(4).zip(mask.chunks(4)) {
                        p[3] = if m[0] == 0 { 255 } else { 0 };
                    }
                }
                for p in px.chunks_mut(4) {
                    p.swap(0, 2); // BGRA → RGBA
                }
                encode_png(&px, w as u32, h as u32)
            })();
            DeleteDC(dc);
            DeleteObject(info.hbmColor);
            DeleteObject(info.hbmMask);
            result
        }
    }

    /// Папка, куда установлено приложение из Microsoft Store (меняется с каждой версией).
    pub fn package_path(family: &str) -> Option<PathBuf> {
        use windows_sys::Win32::Storage::Packaging::Appx::{GetPackagePathByFullName, GetPackagesByPackageFamily};
        let family = wide(std::ffi::OsStr::new(family));
        unsafe {
            let (mut count, mut len) = (0u32, 0u32);
            GetPackagesByPackageFamily(family.as_ptr(), &mut count, std::ptr::null_mut(), &mut len, std::ptr::null_mut());
            if count == 0 || len == 0 {
                return None;
            }
            let mut names = vec![std::ptr::null_mut::<u16>(); count as usize];
            let mut buf = vec![0u16; len as usize];
            if GetPackagesByPackageFamily(family.as_ptr(), &mut count, names.as_mut_ptr(), &mut len, buf.as_mut_ptr()) != 0 {
                return None;
            }
            // Если версий несколько (идёт обновление), берём последнюю в списке.
            let full = *names.get(count as usize - 1)?;
            let mut path_len = 0u32;
            GetPackagePathByFullName(full, &mut path_len, std::ptr::null_mut());
            let mut path = vec![0u16; path_len as usize];
            if GetPackagePathByFullName(full, &mut path_len, path.as_mut_ptr()) != 0 {
                return None;
            }
            let end = path.iter().position(|&c| c == 0).unwrap_or(path.len());
            Some(PathBuf::from(OsString::from_wide(&path[..end])))
        }
    }

    /// Запустить приложение из Microsoft Store с параметрами (так их можно передать только через
    /// «диспетчер активации» Windows). Возвращает номер процесса.
    pub fn activate_app(aumid: &str, args: &str) -> Result<u32, String> {
        use windows_sys::Win32::System::Com::{CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
        use windows_sys::core::{GUID, HRESULT, PCWSTR};
        const CLSID: GUID = GUID::from_u128(0x45ba127d_10a8_46ea_8ab7_56ea9078943c);
        const IID: GUID = GUID::from_u128(0x2e941141_7f97_4756_ba1d_9decde894a3d);
        /// Таблица методов IApplicationActivationManager (сначала три метода IUnknown).
        #[repr(C)]
        struct Vtbl {
            _query: usize,
            _add_ref: usize,
            release: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
            activate: unsafe extern "system" fn(*mut std::ffi::c_void, PCWSTR, PCWSTR, i32, *mut u32) -> HRESULT,
        }
        let aumid = wide(std::ffi::OsStr::new(aumid));
        let args = wide(std::ffi::OsStr::new(args));
        // Своя нить: у нити окна свои настройки COM, не мешаем им.
        std::thread::spawn(move || unsafe {
            CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
            let mut obj: *mut std::ffi::c_void = std::ptr::null_mut();
            let hr = CoCreateInstance(&CLSID, std::ptr::null_mut(), CLSCTX_LOCAL_SERVER, &IID, &mut obj);
            if hr < 0 || obj.is_null() {
                return Err(crate::t!("winsys.activation_unavailable", code = format!("0x{hr:08x}")));
            }
            let vtbl = &**(obj as *mut *const Vtbl);
            let mut pid = 0u32;
            let hr = (vtbl.activate)(obj, aumid.as_ptr(), args.as_ptr(), 0, &mut pid);
            (vtbl.release)(obj);
            if hr < 0 { Err(crate::t!("winsys.activation_failed", code = format!("0x{hr:08x}"))) } else { Ok(pid) }
        })
        .join()
        .map_err(|_| crate::t!("winsys.launch_crashed"))?
    }
    /// Стандартное окно Windows «Открыть файл»: выбрать программу (.exe) или ярлык (.lnk).
    pub fn pick_program(title: &str) -> Option<PathBuf> {
        let filter: Vec<u16> = format!("{}\0*.exe;*.lnk\0", crate::t!("winsys.filter_programs")).encode_utf16().chain([0]).collect();
        let title = wide(std::ffi::OsStr::new(title));
        let mut file = vec![0u16; 32_768];
        unsafe {
            let mut ofn: OPENFILENAMEW = std::mem::zeroed();
            ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
            ofn.lpstrFilter = filter.as_ptr();
            ofn.lpstrFile = file.as_mut_ptr();
            ofn.nMaxFile = file.len() as u32;
            ofn.lpstrTitle = title.as_ptr();
            // Ярлык возвращаем как ярлык: разберём сами, вместе с параметрами запуска.
            ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NODEREFERENCELINKS;
            if GetOpenFileNameW(&mut ofn) == 0 {
                return None;
            }
        }
        let len = file.iter().position(|&c| c == 0).unwrap_or(file.len());
        Some(PathBuf::from(OsString::from_wide(&file[..len])))
    }

    /// Жив ли процесс.
    pub fn process_alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
        use windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE;
        unsafe {
            let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if h.is_null() {
                return false;
            }
            let h = Handle(h);
            WaitForSingleObject(h.0, 0) == WAIT_TIMEOUT
        }
    }

    /// Процесс, запущенный с правами администратора: держим его описатель.
    pub struct Elevated(Handle);

    // Описатель процесса можно передавать между нитями.
    unsafe impl Send for Elevated {}

    impl Elevated {
        pub fn is_running(&self) -> bool {
            use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
            unsafe { WaitForSingleObject(self.0.0, 0) == WAIT_TIMEOUT }
        }
        /// Остановить силой (если Windows позволит; обычно помощник уходит сам по файлу «стоп»).
        pub fn terminate(&self) {
            unsafe {
                TerminateProcess(self.0.0, 1);
            }
        }
    }

    /// Запустить программу с правами администратора и не ждать её. Windows покажет
    /// окно «Разрешить этому приложению вносить изменения?». `owner` — окно нашей программы:
    /// с ним запрос появляется поверх, а не мигает в панели задач (0 — без окна).
    pub fn spawn_elevated(exe: &Path, params: &str, owner: isize) -> Result<Elevated, String> {
        use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        let verb = wide(std::ffi::OsStr::new("runas"));
        let file = wide(exe.as_os_str());
        let params = wide(std::ffi::OsStr::new(params));
        unsafe {
            let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
            // Оболочке Windows нужен COM на этой нити (на фоновых нитях его может не быть).
            CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
            info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
            info.hwnd = owner as _;
            info.lpVerb = verb.as_ptr();
            info.lpFile = file.as_ptr();
            info.lpParameters = params.as_ptr();
            info.nShow = SW_HIDE;
            if ShellExecuteExW(&mut info) == 0 {
                let err = std::io::Error::last_os_error();
                return Err(if err.raw_os_error() == Some(1223) {
                    crate::t!("winsys.uac_denied")
                } else {
                    err.to_string()
                });
            }
            if info.hProcess.is_null() {
                return Err(crate::t!("winsys.no_process"));
            }
            Ok(Elevated(Handle(info.hProcess)))
        }
    }
    /// Запустить программу с правами администратора (Windows покажет окно «Разрешить изменения?»)
    /// и дождаться её окончания. `Ok(код выхода)`; `Err` — человек отказал или запуск не удался.
    pub fn run_elevated(exe: &Path, params: &str) -> Result<u32, String> {
        let verb = wide(std::ffi::OsStr::new("runas"));
        let file = wide(exe.as_os_str());
        let params = wide(std::ffi::OsStr::new(params));
        unsafe {
            let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
            info.lpVerb = verb.as_ptr();
            info.lpFile = file.as_ptr();
            info.lpParameters = params.as_ptr();
            info.nShow = SW_HIDE;
            if ShellExecuteExW(&mut info) == 0 {
                let err = std::io::Error::last_os_error();
                return Err(if err.raw_os_error() == Some(1223) {
                    crate::t!("winsys.uac_cancelled")
                } else {
                    err.to_string()
                });
            }
            if info.hProcess.is_null() {
                return Err(crate::t!("winsys.no_process"));
            }
            let process = Handle(info.hProcess);
            WaitForSingleObject(process.0, INFINITE);
            let mut code = 0u32;
            GetExitCodeProcess(process.0, &mut code);
            Ok(code)
        }
    }
}

#[cfg(windows)]
pub use imp::{
    Elevated, activate_app, ansi_to_string, command_line, icon_png, image_path, internet_adapters, lan_ipv4, package_path,
    pick_program, process_alive, processes, random_bytes, run_elevated, spawn_elevated, tcp_connections, terminate,
};

#[cfg(not(windows))]
mod stub {
    use super::*;
    pub fn ansi_to_string(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }
    pub fn processes() -> Vec<Proc> {
        Vec::new()
    }
    pub fn image_path(_: u32) -> Option<PathBuf> {
        None
    }
    pub fn command_line(_: u32) -> Option<String> {
        None
    }
    pub fn terminate(_: u32) -> bool {
        false
    }
    pub fn tcp_connections() -> Vec<TcpConn> {
        Vec::new()
    }
    pub fn icon_png(_: &Path, _: i32, _: u32) -> Option<Vec<u8>> {
        None
    }
    pub fn package_path(_: &str) -> Option<PathBuf> {
        None
    }
    pub fn activate_app(_: &str, _: &str) -> Result<u32, String> {
        Err("только в Windows".into())
    }
    pub fn pick_program(_: &str) -> Option<PathBuf> {
        None
    }
    pub struct Elevated;
    impl Elevated {
        pub fn is_running(&self) -> bool {
            false
        }
        pub fn terminate(&self) {}
    }
    pub fn spawn_elevated(_: &Path, _: &str, _: isize) -> Result<Elevated, String> {
        Err("только в Windows".into())
    }
    pub fn process_alive(_: u32) -> bool {
        false
    }
    pub fn run_elevated(_: &Path, _: &str) -> Result<u32, String> {
        Err("только в Windows".into())
    }
    pub fn internet_adapters() -> Vec<Adapter> {
        Vec::new()
    }
    pub fn lan_ipv4() -> Vec<std::net::Ipv4Addr> {
        Vec::new()
    }
    pub fn random_bytes<const N: usize>() -> [u8; N] {
        // Не Windows — программа там не работает; для сборки тестов хватит времени.
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        std::array::from_fn(|i| (nanos >> ((i % 16) * 8)) as u8)
    }
}

#[cfg(not(windows))]
pub use stub::*;

/// RGBA → PNG.
fn encode_png(rgba: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().ok()?.write_image_data(rgba).ok()?;
    Some(out)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn sees_itself_in_process_list() {
        let me = std::process::id();
        let list = processes();
        let own = list.iter().find(|p| p.pid == me).expect("свой процесс в списке");
        let path = image_path(me).unwrap();
        assert!(command_line(me).is_some_and(|c| !c.is_empty()));
        assert!(path.file_name().unwrap().to_string_lossy().eq_ignore_ascii_case(&own.name));
    }

    #[test]
    fn sees_own_tcp_connection() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let _client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let me = std::process::id();
        let found = tcp_connections()
            .into_iter()
            .any(|c| c.pid == me && c.remote_port == port && c.state == TcpState::Established && c.remote.is_loopback());
        assert!(found, "своё соединение с 127.0.0.1:{port} не найдено");
    }

    #[test]
    fn extracts_icon_of_notepad() {
        let windir = std::env::var("WINDIR").unwrap();
        let png = icon_png(&Path::new(&windir).join(r"System32\notepad.exe"), 0, 48).expect("значок блокнота");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn finds_store_app_folder() {
        // Блокнот из Microsoft Store есть почти на любой Windows 11; если нет — проверять нечего.
        if let Some(dir) = package_path("Microsoft.WindowsNotepad_8wekyb3d8bbwe") {
            assert!(dir.join("AppxManifest.xml").is_file(), "{}", dir.display());
        }
        assert_eq!(package_path("Ninja.NoSuchPackage_0000000000000"), None);
    }

    /// Открывает «Блокнот» на пару секунд — поэтому запускается только вручную:
    /// `cargo test -p ninja-motor activates_store_app -- --ignored`.
    #[test]
    #[ignore]
    fn activates_store_app() {
        // Параметр передаётся программе: по нему и находим её процесс (номер, который вернула
        // Windows, у «Блокнота» живёт недолго — он перезапускает сам себя).
        let mark = format!("ninja-test-{}.txt", std::process::id());
        let pid = activate_app("Microsoft.WindowsNotepad_8wekyb3d8bbwe!App", &mark).expect("запуск через Windows");
        assert!(pid > 0);
        std::thread::sleep(std::time::Duration::from_millis(2500));
        let found: Vec<u32> = processes()
            .into_iter()
            .filter(|p| p.name.eq_ignore_ascii_case("notepad.exe"))
            .filter(|p| command_line(p.pid).is_some_and(|c| c.contains(&mark)))
            .map(|p| p.pid)
            .collect();
        assert!(!found.is_empty(), "процесс с параметром не найден");
        for pid in found {
            terminate(pid);
        }
    }

    #[test]
    fn tells_vpn_from_real_network() {
        let card = |name: &str, description: &str, if_type: u32| Adapter { name: name.into(), description: description.into(), if_type };
        assert!(card("DurevVPN", "sing-tun Tunnel", 53).looks_like_vpn());
        assert!(card("Ethernet 3", "TAP-Windows Adapter V9", 6).looks_like_vpn());
        assert!(card("Работа", "WAN Miniport (IKEv2)", 23).looks_like_vpn());
        assert!(!card("Беспроводная сеть", "MediaTek Wi-Fi 6 MT7921 Wireless LAN Card", 71).looks_like_vpn());
        assert!(!card("Ethernet", "Realtek PCIe GbE Family Controller", 6).looks_like_vpn());
        assert!(!card("vEthernet (Default Switch)", "Hyper-V Virtual Ethernet Adapter", 6).looks_like_vpn());
    }

    /// Хотя бы одна карта с выходом в интернет есть у любого компьютера, где идёт проверка.
    #[test]
    fn finds_internet_adapter() {
        let list = internet_adapters();
        assert!(!list.is_empty());
        assert!(list.iter().all(|a| !a.name.is_empty()), "{list:?}");
    }

    #[test]
    fn ansi_ascii_passthrough() {
        assert_eq!(ansi_to_string(b"C:\\Apps\\a.exe"), "C:\\Apps\\a.exe");
    }
}
