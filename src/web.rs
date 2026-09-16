//! Web 服务模块
//!
//! 提供：
//! - `GET /` 返回内嵌的 HTML 前端
//! - `GET /ws` WebSocket 通道，承载终端数据与 XMODEM 控制

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, error, info, warn};

use crate::protocol::{b64_decode, b64_encode, ClientMessage, ServerMessage};
use crate::serial::{self, SerialEvent, SerialSession};
use crate::xmodem::Progress;

/// 内嵌的前端页面
const INDEX_HTML: &str = include_str!("../static/index.html");

/// 应用共享状态
#[derive(Default)]
pub struct AppState {
    /// 当前打开的串口会话（同一时刻仅允许一个）
    session: Mutex<Option<Arc<SerialSession>>>,
}

type SharedState = Arc<AppState>;

/// 启动 Web 服务
pub async fn serve(addr: SocketAddr) -> anyhow::Result<()> {
    let state: SharedState = Arc::new(AppState::default());

    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Web 界面已启动: http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> impl IntoResponse {
    Html(INDEX_HTML)
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// 处理单个 WebSocket 连接
async fn handle_socket(socket: WebSocket, state: SharedState) {
    use futures_util::{SinkExt, StreamExt};

    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ServerMessage>(256);

    // 事件转发任务：把后台产生的 ServerMessage 写入 WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let text = match serde_json::to_string(&msg) {
                Ok(t) => t,
                Err(e) => {
                    error!("序列化消息失败: {e}");
                    continue;
                }
            };
            if sender.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    // 串口事件订阅任务句柄
    let mut serial_task: Option<tokio::task::JoinHandle<()>> = None;

    while let Some(Ok(msg)) = receiver.next().await {
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };

        let client_msg: ClientMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                let _ = tx
                    .send(ServerMessage::Error {
                        message: format!("消息格式错误: {e}"),
                    })
                    .await;
                continue;
            }
        };

        match client_msg {
            ClientMessage::Ping => {
                let _ = tx.send(ServerMessage::Pong).await;
            }

            ClientMessage::ListPorts => {
                let ports = tokio::task::spawn_blocking(serial::list_ports)
                    .await
                    .unwrap_or_default();
                let _ = tx.send(ServerMessage::Ports { ports }).await;
            }

            ClientMessage::Open { config } => {
                // 先关闭已有会话
                if let Some(old) = state.session.lock().await.take() {
                    old.cancel_xmodem();
                }
                if let Some(h) = serial_task.take() {
                    h.abort();
                }

                let cfg = config.clone();
                let opened = tokio::task::spawn_blocking(move || SerialSession::open(cfg)).await;

                match opened {
                    Ok(Ok(session)) => {
                        let session = Arc::new(session);
                        let port_name = session.port_name.clone();

                        // 订阅串口事件并转发到前端
                        let mut events = session.subscribe();
                        let tx_events = tx.clone();
                        serial_task = Some(tokio::spawn(async move {
                            loop {
                                match events.recv().await {
                                    Ok(SerialEvent::Data(data)) => {
                                        if data.is_empty() {
                                            continue;
                                        }
                                        let _ = tx_events
                                            .send(ServerMessage::Output {
                                                data: b64_encode(&data),
                                            })
                                            .await;
                                    }
                                    Ok(SerialEvent::XmodemProgress(p)) => {
                                        let _ = tx_events.send(progress_to_message(p)).await;
                                    }
                                    Ok(SerialEvent::XmodemDone(result)) => {
                                        let (success, message) = match result {
                                            Ok(()) => (true, "传输完成".to_string()),
                                            Err(e) => (false, e),
                                        };
                                        let _ = tx_events
                                            .send(ServerMessage::XmodemDone {
                                                success,
                                                message,
                                                bytes: 0,
                                            })
                                            .await;
                                    }
                                    Ok(SerialEvent::Error(e)) => {
                                        let _ = tx_events
                                            .send(ServerMessage::Error { message: e })
                                            .await;
                                    }
                                    Ok(SerialEvent::Closed) => {
                                        let _ = tx_events
                                            .send(ServerMessage::Status {
                                                open: false,
                                                port: None,
                                                message: "串口已关闭".into(),
                                            })
                                            .await;
                                        break;
                                    }
                                    Err(broadcast::error::RecvError::Lagged(n)) => {
                                        warn!("事件订阅落后 {n} 条");
                                    }
                                    Err(broadcast::error::RecvError::Closed) => break,
                                }
                            }
                        }));

                        *state.session.lock().await = Some(session);
                        let _ = tx
                            .send(ServerMessage::Status {
                                open: true,
                                port: Some(port_name.clone()),
                                message: format!("已打开 {port_name}"),
                            })
                            .await;
                    }
                    Ok(Err(e)) => {
                        let _ = tx
                            .send(ServerMessage::Error {
                                message: e.to_string(),
                            })
                            .await;
                    }
                    Err(e) => {
                        let _ = tx
                            .send(ServerMessage::Error {
                                message: format!("任务调度失败: {e}"),
                            })
                            .await;
                    }
                }
            }

            ClientMessage::Close => {
                let session = state.session.lock().await.take();
                if let Some(s) = session {
                    let _ = tokio::task::spawn_blocking(move || s.close()).await;
                }
                if let Some(h) = serial_task.take() {
                    h.abort();
                }
                let _ = tx
                    .send(ServerMessage::Status {
                        open: false,
                        port: None,
                        message: "串口已关闭".into(),
                    })
                    .await;
            }

            ClientMessage::Input { data } => {
                let bytes = match b64_decode(&data) {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = tx.send(ServerMessage::Error { message: e }).await;
                        continue;
                    }
                };
                let session = state.session.lock().await.clone();
                match session {
                    Some(s) => {
                        if let Err(e) = s.write(bytes) {
                            let _ = tx
                                .send(ServerMessage::Error {
                                    message: e.to_string(),
                                })
                                .await;
                        }
                    }
                    None => {
                        let _ = tx
                            .send(ServerMessage::Error {
                                message: "串口未打开".into(),
                            })
                            .await;
                    }
                }
            }

            ClientMessage::XmodemSend {
                filename,
                data,
                use_1k,
                handshake_timeout_ms,
                packet_timeout_ms,
            } => {
                let bytes = match b64_decode(&data) {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = tx.send(ServerMessage::Error { message: e }).await;
                        continue;
                    }
                };
                let session = state.session.lock().await.clone();
                match session {
                    Some(s) => {
                        info!(
                            "开始 XMODEM 发送: {filename} ({} 字节, 1K={use_1k})",
                            bytes.len()
                        );
                        if let Err(e) = s.xmodem_send(
                            bytes,
                            use_1k,
                            Duration::from_millis(handshake_timeout_ms),
                            Duration::from_millis(packet_timeout_ms),
                        ) {
                            let _ = tx
                                .send(ServerMessage::Error {
                                    message: e.to_string(),
                                })
                                .await;
                        }
                    }
                    None => {
                        let _ = tx
                            .send(ServerMessage::Error {
                                message: "请先打开串口".into(),
                            })
                            .await;
                    }
                }
            }

            ClientMessage::XmodemCancel => {
                let session = state.session.lock().await.clone();
                if let Some(s) = session {
                    s.cancel_xmodem();
                    debug!("已请求取消 XMODEM 传输");
                }
            }
        }
    }

    // 连接断开：清理资源
    if let Some(h) = serial_task.take() {
        h.abort();
    }
    if let Some(s) = state.session.lock().await.take() {
        let _ = tokio::task::spawn_blocking(move || s.close()).await;
    }
    send_task.abort();
    debug!("WebSocket 连接已结束");
}

/// 把协议层的进度事件转换为前端消息
fn progress_to_message(p: Progress) -> ServerMessage {
    match p {
        Progress::WaitingHandshake => ServerMessage::Progress {
            stage: "waiting_handshake".into(),
            sent: 0,
            total: 0,
            bytes: 0,
            total_bytes: 0,
            message: "等待接收方发起握手...".into(),
        },
        Progress::HandshakeDone(mode) => ServerMessage::Progress {
            stage: "handshake_done".into(),
            sent: 0,
            total: 0,
            bytes: 0,
            total_bytes: 0,
            message: format!("握手完成，校验方式: {}", mode.as_str()),
        },
        Progress::Packet {
            sent,
            total,
            bytes,
            total_bytes,
        } => ServerMessage::Progress {
            stage: "packet".into(),
            sent,
            total,
            bytes,
            total_bytes,
            message: format!("已发送 {sent}/{total} 包"),
        },
        Progress::Retry { packet, attempt } => ServerMessage::Progress {
            stage: "retry".into(),
            sent: packet,
            total: 0,
            bytes: 0,
            total_bytes: 0,
            message: format!("第 {packet} 包重传（第 {attempt} 次）"),
        },
        Progress::Finished { packets, bytes } => ServerMessage::Progress {
            stage: "finished".into(),
            sent: packets,
            total: packets,
            bytes,
            total_bytes: bytes,
            message: format!("传输完成，共 {packets} 包 / {bytes} 字节"),
        },
    }
}
