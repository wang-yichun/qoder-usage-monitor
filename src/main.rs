#![windows_subsystem = "windows"]

mod cli;
mod credits;
mod diagnose;
mod localization;
mod models;
mod native_interop;
mod poller;
mod theme;
mod tray_icon;
mod window;

use std::io::Write;

const HELP: &str = "\
用法: qoder-usage.exe [选项]

选项:
  --demo          使用内置演示数据（无需凭据，便于查看界面）
  --preview       以独立顶层窗口显示小组件，方便截图与调试
  --diagnose      将运行日志写入 %TEMP%\\qoder-usage.log
  --token <token> 显式提供桌面应用 OAuth 令牌（默认自动解密桌面凭据）
  --region <name> global（openapi.qoder.sh）或 cn（openapi.qoder.com.cn），默认自动探测
  --help          显示本帮助

凭据来源优先使用 Qoder 桌面应用的本地令牌（%APPDATA%\\com.qoder.app.stable\\auth.v1.dat），
令牌只在进程内存中使用，不会打印、写入或上传。";

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    if arguments.iter().any(|argument| argument == "--help" || argument == "-h") {
        let _ = std::io::stdout().write_all(HELP.as_bytes());
        return;
    }
    cli::init(&arguments);
    if cli::options().diagnose {
        match diagnose::init() {
            Ok(path) => diagnose::log(format!("diagnose enabled: {}", path.display())),
            Err(error) => {
                let _ = std::io::stderr().write_all(format!("{error}\n").as_bytes());
            }
        }
    }
    window::run();
}
