//! native messaging 的入口进程：浏览器按清单里的 `path` 直接拉起它。
//!
//! 浏览器不允许在清单里带参数，所以必须是一个独立 exe；它和界面共用
//! `native.rs` 里的转发逻辑，只是这里不做别的。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use volant_app_core::native;

fn main() {
    std::process::exit(native::run_host());
}
