// 发布版不弹出终端窗口（Windows 下有意义）
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|a| a == "--selftest") {
        skill_board_lib::selftest();
        return;
    }
    skill_board_lib::run()
}
