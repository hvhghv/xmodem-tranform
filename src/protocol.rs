//! 前后端 WebSocket 消息协议
//!
//! 前端 → 后端：`ClientMessage`
//! 后端 → 前端：`ServerMessage`
//!
//! 终端数据使用 base64 编码，避免二进制/文本混用带来的转义问题。

use serde::{Deserialize, Serialize};

/// 前端发往后端的消息
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// 列出可用串口
    ListPorts,
    /// 打开串口
    Open {
        #[serde(flatten)]
        config: crate::serial::SerialConfig,
    },
    /// 关闭串口
    Close,
    /// 终端输入（base64）
    Input { data: String },
    /// 开始 XMODEM 发送
    XmodemSend {
        /// 文件名（仅用于界面显示）
        #[serde(default)]
        filename: String,
        /// 文件内容（base64）
        data: String,
        /// 是否使用 1024 字节包
        #[serde(default)]
        use_1k: bool,
        /// 握手超时（毫秒）
        #[serde(default = "default_handshake_timeout")]
        handshake_timeout_ms: u64,
        /// 单包应答超时（毫秒）
        #[serde(default = "default_packet_timeout")]
        packet_timeout_ms: u64,
    },
    /// 取消 XMODEM 发送
    XmodemCancel,
    /// 清空终端（前端本地操作，后端仅记录）
    Ping,
}

fn default_handshake_timeout() -> u64 {
    60_000
}
fn default_packet_timeout() -> u64 {
    3_000
}

/// 后端发往前端的消息
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// 串口列表
    Ports {
        ports: Vec<crate::serial::PortInfo>,
    },
    /// 串口状态变化
    Status {
        open: bool,
        port: Option<String>,
        message: String,
    },
    /// 终端输出（base64）
    Output { data: String },
    /// XMODEM 进度
    Progress {
        /// waiting_handshake | handshake_done | packet | retry | finished
        stage: String,
        sent: u32,
        total: u32,
        bytes: u64,
        total_bytes: u64,
        message: String,
    },
    /// XMODEM 结束
    XmodemDone {
        success: bool,
        message: String,
        bytes: u64,
    },
    /// 错误提示
    Error { message: String },
    /// Pong
    Pong,
}

/// 标准 base64 编码
pub fn b64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 标准 base64 解码
pub fn b64_decode(input: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Result<u32, String> {
        match c {
            b'A'..=b'Z' => Ok((c - b'A') as u32),
            b'a'..=b'z' => Ok((c - b'a') as u32 + 26),
            b'0'..=b'9' => Ok((c - b'0') as u32 + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            other => Err(format!("非法 base64 字符: {}", other as char)),
        }
    }
    let cleaned: Vec<u8> = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    if cleaned.len() % 4 == 1 {
        return Err("base64 长度非法".to_string());
    }
    let mut out = Vec::with_capacity(cleaned.len() / 4 * 3);
    for chunk in cleaned.chunks(4) {
        // 每个字符占据 6 位，第 i 个字符放在 (18 - 6*i) 位，
        // 这样拼出的 24 位整数中，高 8 位即第一个字节，无需额外移位。
        let mut n: u32 = 0;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        // 每多一个字符就多还原一个字节
        out.push((n >> 16) as u8);
        if chunk.len() >= 3 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() == 4 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_b64_roundtrip_various_lengths() {
        for len in 0..64usize {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            let encoded = b64_encode(&data);
            let decoded = b64_decode(&encoded).unwrap();
            assert_eq!(decoded, data, "长度 {len} 往返失败");
        }
    }

    #[test]
    fn test_b64_known_values() {
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(b64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn test_b64_decode_ignores_whitespace() {
        assert_eq!(b64_decode("Zm9v\nYmFy").unwrap(), b"foobar");
    }

    #[test]
    fn test_b64_decode_rejects_invalid() {
        assert!(b64_decode("!!!!").is_err());
    }

    #[test]
    fn test_b64_decode_rejects_bad_length() {
        // 长度 % 4 == 1 的 base64 不合法
        assert!(b64_decode("Zm9vY").is_err());
    }

    #[test]
    fn test_client_message_parse_open() {
        let json = r#"{"type":"open","port":"COM3","baud_rate":9600}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Open { config } => {
                assert_eq!(config.port, "COM3");
                assert_eq!(config.baud_rate, 9600);
                assert_eq!(config.data_bits, 8);
            }
            other => panic!("期望 Open，实际 {other:?}"),
        }
    }

    #[test]
    fn test_client_message_parse_xmodem_send_defaults() {
        let json = r#"{"type":"xmodem_send","data":"AAAA"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::XmodemSend {
                use_1k,
                handshake_timeout_ms,
                packet_timeout_ms,
                ..
            } => {
                assert!(!use_1k);
                assert_eq!(handshake_timeout_ms, 60_000);
                assert_eq!(packet_timeout_ms, 3_000);
            }
            other => panic!("期望 XmodemSend，实际 {other:?}"),
        }
    }

    #[test]
    fn test_server_message_serializes_with_type_tag() {
        let msg = ServerMessage::Error {
            message: "boom".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"error""#));
        assert!(json.contains(r#""message":"boom""#));
    }
}
