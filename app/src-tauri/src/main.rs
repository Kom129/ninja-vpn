// В выпускной сборке не открывать лишнее окно консоли рядом с приложением. НЕ УДАЛЯТЬ.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Помощник TUN (запускается с правами администратора) — без окна, своя работа и выход.
    if let Some(code) = ninja_vpn_lib::helper_mode() {
        std::process::exit(code);
    }
    ninja_vpn_lib::run()
}
