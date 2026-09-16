//! XMODEM 协议实现（含 XMODEM-1K 与 CRC/Checksum 两种校验方式）
//!
//! 协议要点：
//! - 每个数据包 128 字节（XMODEM-1K 为 1024 字节），不足部分用 0x1A (SUB) 填充
//! - 包结构：`SOH/STX | 包序号 | 255-包序号 | 数据 | 校验`
//! - 接收方先发送 NAK (0x15) 请求 checksum 模式，或发送 'C' (0x43) 请求 CRC 模式
//! - 发送方收到应答后开始传输，每包等待 ACK (0x06)，出错则等待 NAK (0x15) 重传
//! - EOT (0x04) 表示传输结束，接收方回 ACK

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 控制字符定义
pub const SOH: u8 = 0x01; // 128 字节数据包起始
pub const STX: u8 = 0x02; // 1024 字节数据包起始（XMODEM-1K）
pub const EOT: u8 = 0x04; // 传输结束
pub const ACK: u8 = 0x06; // 确认
pub const NAK: u8 = 0x15; // 否认 / 请求重传
pub const CAN: u8 = 0x18; // 取消传输
pub const SUB: u8 = 0x1A; // 填充字节
pub const CRC_CHAR: u8 = b'C'; // 请求 CRC 模式

/// 128 字节包的数据区大小
pub const PAYLOAD_128: usize = 128;
/// 1024 字节包的数据区大小
pub const PAYLOAD_1024: usize = 1024;

/// 单包最大重试次数
pub const MAX_RETRIES: u32 = 10;

/// XMODEM 错误类型
#[derive(Debug, Error)]
pub enum XmodemError {
    #[error("传输被取消")]
    Cancelled,
    #[error("接收方无响应（超时）")]
    Timeout,
    #[error("重试次数超过上限({0})")]
    TooManyRetries(u32),
    #[error("接收方发送了 CAN，传输被中止")]
    RemoteCancel,
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

/// 校验方式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChecksumMode {
    /// 8 位累加和校验
    Checksum,
    /// CRC-16/XMODEM 校验
    Crc,
}

impl ChecksumMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChecksumMode::Checksum => "checksum",
            ChecksumMode::Crc => "crc",
        }
    }
}

/// 传输进度事件，由上层转换成前端消息
#[derive(Debug, Clone)]
pub enum Progress {
    /// 等待接收方发起握手
    WaitingHandshake,
    /// 握手完成，确定校验方式
    HandshakeDone(ChecksumMode),
    /// 已发送的包数量、总包数、已发送字节数、总字节数
    Packet {
        sent: u32,
        total: u32,
        bytes: u64,
        total_bytes: u64,
    },
    /// 某包重传
    Retry { packet: u32, attempt: u32 },
    /// 传输完成
    Finished { packets: u32, bytes: u64 },
}

/// 计算 CRC-16/XMODEM（多项式 0x1021，初值 0x0000，不反转）
pub fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// 计算 8 位累加和校验
pub fn checksum8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b))
}

/// 组装一个 XMODEM 数据包
///
/// `payload` 长度必须等于 `PAYLOAD_128` 或 `PAYLOAD_1024`。
pub fn build_packet(packet_no: u8, payload: &[u8], mode: ChecksumMode) -> Vec<u8> {
    assert!(
        payload.len() == PAYLOAD_128 || payload.len() == PAYLOAD_1024,
        "payload 长度必须为 128 或 1024"
    );
    let header = if payload.len() == PAYLOAD_1024 { STX } else { SOH };
    let mut buf = Vec::with_capacity(payload.len() + 5);
    buf.push(header);
    buf.push(packet_no);
    buf.push(!packet_no);
    buf.extend_from_slice(payload);
    match mode {
        ChecksumMode::Checksum => buf.push(checksum8(payload)),
        ChecksumMode::Crc => {
            let crc = crc16_xmodem(payload);
            buf.push((crc >> 8) as u8);
            buf.push((crc & 0xFF) as u8);
        }
    }
    buf
}

/// 校验一个已解析的数据包（不含帧头与序号）
///
/// 供接收方向以及外部调用者验证数据包完整性。
pub fn verify_packet(payload: &[u8], checksum: &[u8], mode: ChecksumMode) -> bool {
    match mode {
        ChecksumMode::Checksum => {
            checksum.len() == 1 && checksum[0] == checksum8(payload)
        }
        ChecksumMode::Crc => {
            checksum.len() == 2
                && crc16_xmodem(payload)
                    == ((checksum[0] as u16) << 8 | checksum[1] as u16)
        }
    }
}

/// 把任意长度的数据切分成 XMODEM 数据包负载（不足补 SUB）
pub fn split_into_payloads(data: &[u8], block_size: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if data.is_empty() {
        return out;
    }
    for chunk in data.chunks(block_size) {
        let mut block = chunk.to_vec();
        block.resize(block_size, SUB);
        out.push(block);
    }
    out
}

/// 串口读写抽象，便于测试时替换为内存实现
pub trait SerialIo {
    /// 读取至多 1 个字节，超时返回 `None`
    fn read_byte(&mut self, timeout: Duration) -> Result<Option<u8>, XmodemError>;
    /// 写入全部字节
    fn write_all(&mut self, data: &[u8]) -> Result<(), XmodemError>;
    /// 丢弃输入缓冲区中的残留数据
    fn flush_input(&mut self) -> Result<(), XmodemError>;
    /// 检查是否被外部请求取消
    fn is_cancelled(&self) -> bool;
}

/// 等待接收方的应答字节。
///
/// 与直接 `read_byte` 的区别：会**跳过噪声字节**，直到读到 ACK/NAK/CAN
/// 或超时。XMODEM 接收方在出错时可能连续发送多个 NAK，若把残留的 NAK
/// 留给下一个包，就会造成永久性的应答错位。
///
/// 返回 `None` 表示超时。
fn wait_response<S: SerialIo>(
    io: &mut S,
    timeout: Duration,
) -> Result<Option<u8>, XmodemError> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        // 单次读取用较短超时，便于及时检查总超时与取消标志
        let slice = remaining.min(Duration::from_millis(200));
        match io.read_byte(slice)? {
            Some(b @ (ACK | NAK | CAN)) => return Ok(Some(b)),
            // 噪声字节（如线路干扰、设备回显），丢弃后继续等待
            Some(_) => {
                if io.is_cancelled() {
                    return Err(XmodemError::Cancelled);
                }
                continue;
            }
            None => {
                if io.is_cancelled() {
                    return Err(XmodemError::Cancelled);
                }
            }
        }
    }
}

/// 丢弃输入缓冲区中**已到达**的残留应答字节。
///
/// 接收方在判定某包校验失败时，可能连续发送多个 NAK。若这些 NAK 被
/// 后续的 `wait_response` 逐个读到，就会对同一个包触发多次重传，
/// 重传次数迅速累积到上限而失败。
///
/// 这里用很短的超时做「非阻塞式排空」：只清掉**立刻可读**的字节，
/// 不会等待、也不会误删尚未到达的合法应答。
fn drain_pending<S: SerialIo>(io: &mut S) {
    // 反复读取，直到连续两次都读不到数据为止
    let mut empty_streak = 0;
    while empty_streak < 2 {
        match io.read_byte(Duration::from_millis(1)) {
            Ok(Some(_)) => empty_streak = 0,
            Ok(None) => empty_streak += 1,
            // 读取出错时放弃排空，交由后续流程处理
            Err(_) => break,
        }
    }
}

/// 解析一个 XMODEM 数据包帧，返回 `(包序号, 负载, 校验字段)`
///
/// `frame` 应包含帧头（SOH/STX）、序号、反码序号、数据与校验字段。
/// 负载长度由帧头决定（SOH=128，STX=1024），校验字段长度由帧总长推断
/// （1 字节 checksum 或 2 字节 CRC），因此 SOH 帧既支持 132 字节也支持 133 字节。
/// 解析失败时返回 `None`。
///
/// 该函数与 [`verify_frame`] 一起构成接收方向的解析能力，
/// 可直接用于校验对端发来的数据包。
#[allow(dead_code)]
pub fn parse_packet(frame: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    if frame.len() < 5 {
        return None;
    }
    let payload_len = match frame[0] {
        SOH => PAYLOAD_128,
        STX => PAYLOAD_1024,
        _ => return None,
    };
    // 剩余长度即为校验字段长度，只接受 1（checksum）或 2（CRC）
    let checksum_len = frame.len().checked_sub(3 + payload_len)?;
    if checksum_len != 1 && checksum_len != 2 {
        return None;
    }
    let packet_no = frame[1];
    // 序号与反码序号必须互补
    if frame[2] != !packet_no {
        return None;
    }
    let payload = &frame[3..3 + payload_len];
    let checksum = &frame[3 + payload_len..];
    Some((packet_no, payload, checksum))
}

/// 按帧长与校验字段长度推断校验方式，校验整个数据包帧
///
/// 与 [`parse_packet`] 配套使用，用于接收方向校验对端数据包。
#[allow(dead_code)]
pub fn verify_frame(frame: &[u8]) -> bool {
    match parse_packet(frame) {
        Some((_, payload, checksum)) => {
            let mode = if checksum.len() == 2 {
                ChecksumMode::Crc
            } else {
                ChecksumMode::Checksum
            };
            verify_packet(payload, checksum, mode)
        }
        None => false,
    }
}

/// 等待接收方握手，返回协商出的校验方式
fn wait_handshake<S: SerialIo>(
    io: &mut S,
    timeout: Duration,
    allow_1k: bool,
    mut on_progress: impl FnMut(Progress),
) -> Result<ChecksumMode, XmodemError> {
    on_progress(Progress::WaitingHandshake);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if io.is_cancelled() {
            return Err(XmodemError::Cancelled);
        }
        if std::time::Instant::now() >= deadline {
            return Err(XmodemError::Timeout);
        }
        match io.read_byte(Duration::from_millis(200))? {
            Some(CRC_CHAR) => {
                let mode = ChecksumMode::Crc;
                on_progress(Progress::HandshakeDone(mode));
                let _ = allow_1k;
                return Ok(mode);
            }
            Some(NAK) => {
                let mode = ChecksumMode::Checksum;
                on_progress(Progress::HandshakeDone(mode));
                return Ok(mode);
            }
            // 忽略噪声数据（设备提示符、回显等）
            Some(_) => continue,
            None => continue,
        }
    }
}

/// 使用 XMODEM 协议发送数据
///
/// - `data`：待发送的原始数据
/// - `handshake_timeout`：等待接收方握手的最长时间
/// - `packet_timeout`：单包等待应答的超时时间
/// - `use_1k`：是否使用 1024 字节包（XMODEM-1K）
pub fn send<S: SerialIo>(
    io: &mut S,
    data: &[u8],
    handshake_timeout: Duration,
    packet_timeout: Duration,
    use_1k: bool,
    mut on_progress: impl FnMut(Progress),
) -> Result<(), XmodemError> {
    let block_size = if use_1k { PAYLOAD_1024 } else { PAYLOAD_128 };

    // 清理串口残留数据，避免旧数据干扰握手
    io.flush_input()?;

    let mode = wait_handshake(io, handshake_timeout, use_1k, &mut on_progress)?;

    let payloads = split_into_payloads(data, block_size);
    let total = payloads.len() as u32;
    let mut sent_bytes: u64 = 0;

    for (idx, payload) in payloads.iter().enumerate() {
        if io.is_cancelled() {
            // 主动取消时通知接收方
            let _ = io.write_all(&[CAN, CAN, CAN]);
            return Err(XmodemError::Cancelled);
        }

        // 包序号从 1 开始，按 8 位回绕
        let packet_no = ((idx + 1) & 0xFF) as u8;
        let packet = build_packet(packet_no, payload, mode);

        let mut attempt = 0u32;
        loop {
            if io.is_cancelled() {
                let _ = io.write_all(&[CAN, CAN, CAN]);
                return Err(XmodemError::Cancelled);
            }
            if attempt > 0 {
                on_progress(Progress::Retry {
                    packet: packet_no as u32,
                    attempt,
                });
            }
            io.write_all(&packet)?;

            match wait_response(io, packet_timeout)? {
                Some(ACK) => break,
                Some(NAK) => {
                    attempt += 1;
                    if attempt > MAX_RETRIES {
                        let _ = io.write_all(&[CAN, CAN, CAN]);
                        return Err(XmodemError::TooManyRetries(MAX_RETRIES));
                    }
                    // 排空接收方连发的残留 NAK，避免对同一包重复计数重传
                    drain_pending(io);
                    continue;
                }
                Some(CAN) => {
                    return Err(XmodemError::RemoteCancel);
                }
                Some(_) | None => {
                    // 超时或其它字节，重传
                    attempt += 1;
                    if attempt > MAX_RETRIES {
                        let _ = io.write_all(&[CAN, CAN, CAN]);
                        return Err(XmodemError::TooManyRetries(MAX_RETRIES));
                    }
                    // 超时后同样排空，避免把迟到字节算到下一次
                    drain_pending(io);
                    continue;
                }
            }
        }

        sent_bytes += payload.len() as u64;
        on_progress(Progress::Packet {
            sent: idx as u32 + 1,
            total,
            bytes: sent_bytes,
            total_bytes: data.len() as u64,
        });
    }

    // 发送 EOT 并等待 ACK
    let mut attempt = 0u32;
    loop {
        io.write_all(&[EOT])?;
        match wait_response(io, packet_timeout)? {
            Some(ACK) => break,
            Some(CAN) => return Err(XmodemError::RemoteCancel),
            Some(_) | None => {
                attempt += 1;
                if attempt > MAX_RETRIES {
                    return Err(XmodemError::TooManyRetries(MAX_RETRIES));
                }
            }
        }
    }

    on_progress(Progress::Finished {
        packets: total,
        bytes: data.len() as u64,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn test_crc16_xmodem_known_vector() {
        // "123456789" 的 CRC-16/XMODEM 标准测试值为 0x31C3
        assert_eq!(crc16_xmodem(b"123456789"), 0x31C3);
    }

    #[test]
    fn test_checksum8() {
        assert_eq!(checksum8(&[0x01, 0x02, 0x03]), 0x06);
        // 溢出回绕
        assert_eq!(checksum8(&[0xFF, 0x02]), 0x01);
    }

    #[test]
    fn test_build_packet_128_crc() {
        let payload = vec![0xAAu8; PAYLOAD_128];
        let pkt = build_packet(1, &payload, ChecksumMode::Crc);
        assert_eq!(pkt.len(), 128 + 5);
        assert_eq!(pkt[0], SOH);
        assert_eq!(pkt[1], 1);
        assert_eq!(pkt[2], 254);
        assert!(verify_packet(&pkt[3..131], &pkt[131..133], ChecksumMode::Crc));
    }

    #[test]
    fn test_build_packet_1024_checksum() {
        let payload = vec![0x55u8; PAYLOAD_1024];
        let pkt = build_packet(7, &payload, ChecksumMode::Checksum);
        assert_eq!(pkt.len(), 1024 + 4);
        assert_eq!(pkt[0], STX);
        assert_eq!(pkt[1], 7);
        assert_eq!(pkt[2], 248);
        assert!(verify_packet(&pkt[3..1027], &pkt[1027..1028], ChecksumMode::Checksum));
    }

    #[test]
    fn test_verify_packet_rejects_corruption() {
        let payload = vec![0x00u8; PAYLOAD_128];
        let mut pkt = build_packet(1, &payload, ChecksumMode::Crc);
        let last = pkt.len() - 1;
        pkt[last] ^= 0xFF; // 破坏校验字段
        assert!(!verify_packet(&pkt[3..131], &pkt[131..133], ChecksumMode::Crc));
    }

    #[test]
    fn test_split_into_payloads_pads_with_sub() {
        let data = vec![1u8, 2, 3];
        let blocks = split_into_payloads(&data, PAYLOAD_128);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].len(), PAYLOAD_128);
        assert_eq!(&blocks[0][..3], &[1, 2, 3]);
        assert!(blocks[0][3..].iter().all(|&b| b == SUB));
    }

    #[test]
    fn test_split_into_payloads_empty() {
        assert!(split_into_payloads(&[], PAYLOAD_128).is_empty());
    }

    #[test]
    fn test_parse_packet_roundtrip() {
        // SOH + CRC 模式：校验字段为 2 字节，帧长 133
        let payload = vec![0x11u8; PAYLOAD_128];
        let frame = build_packet(3, &payload, ChecksumMode::Crc);
        let (no, parsed_payload, checksum) = parse_packet(&frame).unwrap();
        assert_eq!(no, 3);
        assert_eq!(parsed_payload, &payload[..]);
        assert_eq!(checksum.len(), 2);
        assert!(verify_frame(&frame));
    }

    #[test]
    fn test_parse_packet_128_checksum_mode() {
        // SOH + checksum 模式：校验字段为 1 字节，帧长 132
        let payload = vec![0x22u8; PAYLOAD_128];
        let frame = build_packet(5, &payload, ChecksumMode::Checksum);
        let (no, parsed_payload, checksum) = parse_packet(&frame).unwrap();
        assert_eq!(no, 5);
        assert_eq!(parsed_payload.len(), PAYLOAD_128);
        assert_eq!(checksum.len(), 1);
        assert!(verify_frame(&frame));
    }

    #[test]
    fn test_parse_packet_rejects_bad_sequence() {
        let payload = vec![0u8; PAYLOAD_128];
        let mut frame = build_packet(3, &payload, ChecksumMode::Crc);
        frame[2] = 0x00; // 破坏反码序号
        assert!(parse_packet(&frame).is_none());
    }

    #[test]
    fn test_parse_packet_rejects_bad_header() {
        let payload = vec![0u8; PAYLOAD_128];
        let mut frame = build_packet(3, &payload, ChecksumMode::Crc);
        frame[0] = 0xFF;
        assert!(parse_packet(&frame).is_none());
    }

    #[test]
    fn test_verify_frame_detects_corruption() {
        let payload = vec![0x77u8; PAYLOAD_128];
        let mut frame = build_packet(1, &payload, ChecksumMode::Checksum);
        let last = frame.len() - 1;
        frame[last] = frame[last].wrapping_add(1);
        assert!(!verify_frame(&frame));
    }

    /// 模拟接收方：先发 'C' 握手，之后对每个包回 ACK
    struct FakeReceiver {
        input: VecDeque<u8>,
        output: Vec<u8>,
        cancel: bool,
    }

    impl FakeReceiver {
        fn new() -> Self {
            Self {
                input: VecDeque::from(vec![CRC_CHAR]),
                output: Vec::new(),
                cancel: false,
            }
        }
    }

    impl SerialIo for FakeReceiver {
        fn read_byte(&mut self, _timeout: Duration) -> Result<Option<u8>, XmodemError> {
            Ok(self.input.pop_front())
        }
        fn write_all(&mut self, data: &[u8]) -> Result<(), XmodemError> {
            self.output.extend_from_slice(data);
            // 收到完整包后回 ACK；收到 EOT 也回 ACK
            if data.len() > 1 && (data[0] == SOH || data[0] == STX) {
                self.input.push_back(ACK);
            } else if data == [EOT] {
                self.input.push_back(ACK);
            }
            Ok(())
        }
        fn flush_input(&mut self) -> Result<(), XmodemError> {
            Ok(())
        }
        fn is_cancelled(&self) -> bool {
            self.cancel
        }
    }

    #[test]
    fn test_send_success_path() {
        let mut rx = FakeReceiver::new();
        let data = vec![0x42u8; 300]; // 需要 3 个 128 字节包
        let mut events = Vec::new();
        send(
            &mut rx,
            &data,
            Duration::from_millis(100),
            Duration::from_millis(50),
            false,
            |p| events.push(p),
        )
        .unwrap();

        // 3 个数据包 + 1 个 EOT
        assert!(matches!(events[0], Progress::WaitingHandshake));
        assert!(matches!(events[1], Progress::HandshakeDone(ChecksumMode::Crc)));
        let finished = events.iter().any(|e| matches!(e, Progress::Finished { packets: 3, .. }));
        assert!(finished, "应产生 Finished 事件且包数为 3");
        assert_eq!(*rx.output.last().unwrap(), EOT);
    }

    #[test]
    fn test_send_cancelled() {
        let mut rx = FakeReceiver::new();
        rx.cancel = true;
        let err = send(
            &mut rx,
            &[0u8; 10],
            Duration::from_millis(100),
            Duration::from_millis(50),
            false,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, XmodemError::Cancelled));
    }

    #[test]
    fn test_send_handshake_timeout() {
        struct Silent;
        impl SerialIo for Silent {
            fn read_byte(&mut self, _t: Duration) -> Result<Option<u8>, XmodemError> {
                Ok(None)
            }
            fn write_all(&mut self, _d: &[u8]) -> Result<(), XmodemError> {
                Ok(())
            }
            fn flush_input(&mut self) -> Result<(), XmodemError> {
                Ok(())
            }
            fn is_cancelled(&self) -> bool {
                false
            }
        }
        let err = send(
            &mut Silent,
            &[0u8; 10],
            Duration::from_millis(10),
            Duration::from_millis(10),
            false,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, XmodemError::Timeout));
    }

    /// 模拟接收方：第一包连发多个 NAK，之后正常 ACK。
    /// 用于验证连发的残留 NAK 不会导致应答错位。
    struct NakBurstReceiver {
        input: VecDeque<u8>,
        packets_seen: u32,
        flushes: u32,
    }

    impl NakBurstReceiver {
        fn new() -> Self {
            Self {
                input: VecDeque::new(),
                packets_seen: 0,
                flushes: 0,
            }
        }
    }

    impl SerialIo for NakBurstReceiver {
        fn read_byte(&mut self, _timeout: Duration) -> Result<Option<u8>, XmodemError> {
            Ok(self.input.pop_front())
        }
        fn write_all(&mut self, data: &[u8]) -> Result<(), XmodemError> {
            if data.len() > 1 && (data[0] == SOH || data[0] == STX) {
                self.packets_seen += 1;
                if self.packets_seen == 1 {
                    // 第一包：连发 3 个 NAK（接收方连续否认）
                    self.input.push_back(NAK);
                    self.input.push_back(NAK);
                    self.input.push_back(NAK);
                } else {
                    self.input.push_back(ACK);
                }
            } else if data == [EOT] {
                self.input.push_back(ACK);
            }
            Ok(())
        }
        fn flush_input(&mut self) -> Result<(), XmodemError> {
            self.flushes += 1;
            self.input.clear();
            // 真实接收方在 flush 后仍会继续发送握手字符，
            // 这里模拟该行为：清理后补上 CRC 握手字符
            self.input.push_back(CRC_CHAR);
            Ok(())
        }
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    #[test]
    fn test_nak_burst_does_not_desync() {
        let mut rx = NakBurstReceiver::new();
        let data = vec![0x5Au8; 200]; // 2 个 128 字节包
        let mut retries = 0;
        send(
            &mut rx,
            &data,
            Duration::from_millis(100),
            Duration::from_millis(50),
            false,
            |p| {
                if matches!(p, Progress::Retry { .. }) {
                    retries += 1;
                }
            },
        )
        .unwrap();

        // 接收方连发 3 个 NAK，但只应对同一包重传一次
        // （其余 NAK 被 drain_pending 排空，不重复计数）
        assert_eq!(retries, 1, "连发 NAK 应只触发一次重传，实际 {retries}");
        // 只应在开始时清理一次输入缓冲（握手前），重传时不应清空
        // —— 否则会丢掉可能已到达的合法 ACK
        assert_eq!(rx.flushes, 1, "flush_input 只应在握手前调用一次");
    }

    #[test]
    fn test_wait_response_skips_noise() {
        struct Noisy {
            input: VecDeque<u8>,
        }
        impl SerialIo for Noisy {
            fn read_byte(&mut self, _t: Duration) -> Result<Option<u8>, XmodemError> {
                Ok(self.input.pop_front())
            }
            fn write_all(&mut self, _d: &[u8]) -> Result<(), XmodemError> {
                Ok(())
            }
            fn flush_input(&mut self) -> Result<(), XmodemError> {
                Ok(())
            }
            fn is_cancelled(&self) -> bool {
                false
            }
        }
        // 噪声（设备提示符等）之后才是真正的 ACK
        let mut io = Noisy {
            input: VecDeque::from(vec![b'~', b' ', b'#', b'\r', ACK]),
        };
        assert_eq!(
            wait_response(&mut io, Duration::from_millis(100)).unwrap(),
            Some(ACK)
        );
    }

    #[test]
    fn test_wait_response_timeout_returns_none() {
        struct Silent;
        impl SerialIo for Silent {
            fn read_byte(&mut self, _t: Duration) -> Result<Option<u8>, XmodemError> {
                Ok(None)
            }
            fn write_all(&mut self, _d: &[u8]) -> Result<(), XmodemError> {
                Ok(())
            }
            fn flush_input(&mut self) -> Result<(), XmodemError> {
                Ok(())
            }
            fn is_cancelled(&self) -> bool {
                false
            }
        }
        let mut io = Silent;
        assert_eq!(
            wait_response(&mut io, Duration::from_millis(20)).unwrap(),
            None
        );
    }
}
