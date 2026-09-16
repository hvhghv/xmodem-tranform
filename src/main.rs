//! XMODEM 串口文件传输工具
//!
//! 功能：
//! - 通过 XMODEM / XMODEM-1K 协议向指定串口发送文件
//! - 内置 Web 界面（HTML 前端），支持串口参数配置与进度显示
//! - 提供简单的串口终端，可收发文本数据
//!
//! 用法：
//! ```text
//! xmodem-tranform [--port 8080] [--host 127.0.0.1] [--no-open]
//! ```

mod protocol;
mod serial;
mod web;
mod xmodem;

use std::net::SocketAddr;

use tracing_subscriber::EnvFilter;

/// 命令行参数
struct Args {
    host: String,
    port: u16,
    open_browser: bool,
}

impl Args {
    fn parse() -> Self {
        let mut host = "127.0.0.1".to_string();
        let mut port = 8080u16;
        let mut open_browser = true;

        let mut iter = std::env::args().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--host" | "-H" => {
                    if let Some(v) = iter.next() {
                        host = v;
                    }
                }
                "--port" | "-p" => {
                    if let Some(v) = iter.next() {
                        if let Ok(p) = v.parse() {
                            port = p;
                        }
                    }
                }
                "--no-open" => open_browser = false,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => {
                    eprintln!("未知参数: {other}");
                    print_help();
                    std::process::exit(1);
                }
            }
        }

        Self {
            host,
            port,
            open_browser,
        }
    }
}

fn print_help() {
    println!(
        "xmodem-tranform - XMODEM 串口文件传输工具\n\n\
         用法: xmodem-tranform [选项]\n\n\
         选项:\n\
           -H, --host <HOST>  监听地址 (默认 127.0.0.1)\n\
           -p, --port <PORT>  监听端口 (默认 8080)\n\
               --no-open      启动后不自动打开浏览器\n\
           -h, --help         显示帮助"
    );
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,serialport=warn")),
        )
        .init();

    let args = Args::parse();
    let addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;

    if args.open_browser {
        let url = format!("http://{addr}");
        tokio::spawn(async move {
            // 稍等片刻确保服务已就绪
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Err(e) = open::that(&url) {
                tracing::warn!("打开浏览器失败: {e}，请手动访问 {url}");
            }
        });
    }

    println!("XMODEM 串口传输工具已启动，请访问 http://{addr}");
    web::serve(addr).await
}
