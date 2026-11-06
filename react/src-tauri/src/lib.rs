//! urlnote 桌面版 Rust 壳。
//!
//! 功能全部在 react 前端里(与 Web 版同一份产物,见 ../dist);本壳只负责
//! 窗口与应用生命周期。后续要加原生能力(全局快捷键、本地文件监听等)
//! 在此注册 plugin 与 command。

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("error while running urlnote desktop");
}
