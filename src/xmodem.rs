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
    /// 接收方向：已发出握手请求，等待发送方响应
    WaitingSender,
    /// 接收方向：已收到的包数量、已接收字节数
    Received { packets: u32, bytes: u64 },
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

/// 从流中读取一个完整的数据包帧（按已知的校验方式确定帧长）。
///
/// XMODEM 是字节流协议，发送方可能在任何位置开始发送，因此这里会
/// **跳过所有非 SOH/STX 的字节**（设备提示符、回显、线路噪声），
/// 直到遇到帧头再按帧长读取剩余部分。
///
/// 返回 `Ok(Some(frame))` 表示读到一个完整帧；`Ok(None)` 表示超时。
fn read_frame<S: SerialIo>(
    io: &mut S,
    mode: ChecksumMode,
    wait_timeout: Duration,
) -> Result<Option<Vec<u8>>, XmodemError> {
    // 第一阶段：等待帧头。发送方可能正忙于处理上一包的应答，
    // 帧间间隔可能明显大于单字节间隔，因此这里用调用方给的等待超时。
    let deadline = std::time::Instant::now() + wait_timeout;
    let header = loop {
        if io.is_cancelled() {
            return Err(XmodemError::Cancelled);
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let slice = remaining.min(Duration::from_millis(200));
        match io.read_byte(slice)? {
            Some(b @ (SOH | STX)) => break b,
            // 非帧头字节（噪声、回显）直接丢弃
            Some(_) => continue,
            None => continue,
        }
    };

    let payload_len = if header == STX { PAYLOAD_1024 } else { PAYLOAD_128 };
    let checksum_len = match mode {
        ChecksumMode::Crc => 2,
        ChecksumMode::Checksum => 1,
    };
    let total_len = 3 + payload_len + checksum_len;

    // 第二阶段：读取帧剩余部分。帧内字节必须连续到达，
    // 因此用较短的超时快速判定丢包。
    let mut frame = Vec::with_capacity(total_len);
    frame.push(header);
    let inner_timeout = Duration::from_millis(500);
    while frame.len() < total_len {
        if io.is_cancelled() {
            return Err(XmodemError::Cancelled);
        }
        match io.read_byte(inner_timeout)? {
            Some(b) => frame.push(b),
            None => return Ok(None), // 帧不完整，按丢包处理
        }
    }
    Ok(Some(frame))
}

/// 使用 XMODEM 协议接收数据
///
/// - `mode`：本端请求的校验方式（`Crc` 发送 'C'，`Checksum` 发送 NAK）
/// - `handshake_timeout`：等待发送方首个数据包的最长时间
/// - `packet_timeout`：单包等待超时
/// - `max_packets`：安全上限，防止无限接收耗尽内存（0 表示不限制）
///
/// 返回接收到的原始数据（已去除末尾的 SUB 填充）。
pub fn receive<S: SerialIo>(
    io: &mut S,
    mode: ChecksumMode,
    handshake_timeout: Duration,
    packet_timeout: Duration,
    max_packets: u32,
    mut on_progress: impl FnMut(Progress),
) -> Result<Vec<u8>, XmodemError> {
    io.flush_input()?;

    let mut out: Vec<u8> = Vec::new();
    let mut expected_no: u8 = 1;
    let mut packets: u32 = 0;

    // 握手：CRC 模式发 'C'，checksum 模式发 NAK。
    // 发送方可能尚未就绪，因此周期性重发，直到收到第一个数据包。
    let handshake_deadline = std::time::Instant::now() + handshake_timeout;
    let mut next_handshake = std::time::Instant::now();
    on_progress(Progress::WaitingSender);

    let mut got_first = false;
    while !got_first {
        if io.is_cancelled() {
            let _ = io.write_all(&[CAN, CAN, CAN]);
            return Err(XmodemError::Cancelled);
        }
        if std::time::Instant::now() >= handshake_deadline {
            return Err(XmodemError::Timeout);
        }

        if std::time::Instant::now() >= next_handshake {
            let request = match mode {
                ChecksumMode::Crc => CRC_CHAR,
                ChecksumMode::Checksum => NAK,
            };
            io.write_all(&[request])?;
            next_handshake = std::time::Instant::now() + Duration::from_secs(3);
        }

        match read_frame(io, mode, Duration::from_millis(300))? {
            Some(frame) => match parse_packet(&frame) {
                Some((no, payload, checksum))
                    if verify_packet(payload, checksum, mode) && no == expected_no =>
                {
                    out.extend_from_slice(payload);
                    io.write_all(&[ACK])?;
                    packets += 1;
                    expected_no = expected_no.wrapping_add(1);
                    on_progress(Progress::HandshakeDone(mode));
                    on_progress(Progress::Received {
                        packets,
                        bytes: out.len() as u64,
                    });
                    got_first = true;
                }
                // 校验失败或序号不符，请求重传
                _ => {
                    io.write_all(&[NAK])?;
                }
            },
            None => continue, // 超时，重发握手字符
        }
    }

    // 主循环：逐包接收，直到 EOT
    let mut retries = 0u32;
    loop {
        if io.is_cancelled() {
            let _ = io.write_all(&[CAN, CAN, CAN]);
            return Err(XmodemError::Cancelled);
        }

        // read_frame 只认 SOH/STX 作为帧头，会把 EOT 当作噪声丢弃，
        // 因此这里先单独探测一个字节：EOT 结束传输，CAN 判定取消，
        // SOH/STX 则说明数据包已经开始，转交 read_frame 补全整帧。
        match io.read_byte(packet_timeout)? {
            Some(EOT) => {
                io.write_all(&[ACK])?;
                // 部分发送方会重发 EOT，补读一次做容错（读不到也无妨）
                let _ = io.read_byte(Duration::from_millis(200));
                break;
            }
            Some(CAN) => {
                // 连续三个 CAN 才算真正取消
                let mut cans = 1;
                while cans < 3 {
                    match io.read_byte(Duration::from_millis(200))? {
                        Some(CAN) => cans += 1,
                        Some(_) => continue,
                        None => break,
                    }
                }
                if cans >= 3 {
                    return Err(XmodemError::RemoteCancel);
                }
                continue;
            }
            // 帧头已读到，补全整帧后按正常流程校验
            Some(header @ (SOH | STX)) => {
                match read_frame_after_header(io, mode, header) {
                    Some(frame) => {
                        if handle_frame(
                            io,
                            &frame,
                            mode,
                            &mut out,
                            &mut expected_no,
                            &mut packets,
                            &mut retries,
                            max_packets,
                            &mut on_progress,
                        )? {
                            break;
                        }
                    }
                    None => {
                        // 帧不完整，请求重传
                        io.write_all(&[NAK])?;
                        retries += 1;
                    }
                }
            }
            // 噪声字节，忽略
            Some(_) => continue,
            None => {
                // 超时：请求重传当前期望的包
                io.write_all(&[NAK])?;
                retries += 1;
            }
        }

        if retries > MAX_RETRIES {
            let _ = io.write_all(&[CAN, CAN, CAN]);
            return Err(XmodemError::TooManyRetries(MAX_RETRIES));
        }
    }

    // 去掉末尾的 SUB 填充字节（XMODEM 用 0x1A 补齐最后一个块）
    while out.last() == Some(&SUB) {
        out.pop();
    }

    on_progress(Progress::Finished {
        packets,
        bytes: out.len() as u64,
    });
    Ok(out)
}

/// 已读到帧头后，补全一个完整的数据包帧
fn read_frame_after_header<S: SerialIo>(
    io: &mut S,
    mode: ChecksumMode,
    header: u8,
) -> Option<Vec<u8>> {
    let payload_len = if header == STX { PAYLOAD_1024 } else { PAYLOAD_128 };
    let checksum_len = match mode {
        ChecksumMode::Crc => 2,
        ChecksumMode::Checksum => 1,
    };
    let total_len = 3 + payload_len + checksum_len;

    let mut frame = Vec::with_capacity(total_len);
    frame.push(header);
    let inner_timeout = Duration::from_millis(500);
    while frame.len() < total_len {
        match io.read_byte(inner_timeout) {
            Ok(Some(b)) => frame.push(b),
            // 读超时或出错都按帧不完整处理
            _ => return None,
        }
    }
    Some(frame)
}

/// 校验并处理一个已读全的数据包帧。
///
/// 返回 `Ok(true)` 表示收到 EOT 语义的终止（本函数不产生该结果，
/// 保留返回值以便调用方统一处理循环退出）。
#[allow(clippy::too_many_arguments)]
fn handle_frame<S: SerialIo>(
    io: &mut S,
    frame: &[u8],
    mode: ChecksumMode,
    out: &mut Vec<u8>,
    expected_no: &mut u8,
    packets: &mut u32,
    retries: &mut u32,
    max_packets: u32,
    on_progress: &mut impl FnMut(Progress),
) -> Result<bool, XmodemError> {
    match parse_packet(frame) {
        Some((no, payload, checksum)) if verify_packet(payload, checksum, mode) => {
            if no == *expected_no {
                out.extend_from_slice(payload);
                *packets += 1;
                *expected_no = expected_no.wrapping_add(1);
                *retries = 0;
                io.write_all(&[ACK])?;
                on_progress(Progress::Received {
                    packets: *packets,
                    bytes: out.len() as u64,
                });
                if max_packets > 0 && *packets >= max_packets {
                    let _ = io.write_all(&[CAN, CAN, CAN]);
                    return Err(XmodemError::TooManyRetries(max_packets));
                }
            } else if no == expected_no.wrapping_sub(1) {
                // 重复包（上一个 ACK 丢失导致发送方重传）：再 ACK 一次
                io.write_all(&[ACK])?;
            } else {
                // 序号跳跃，请求重传
                io.write_all(&[NAK])?;
                *retries += 1;
            }
        }
        _ => {
            // 校验失败或帧非法
            io.write_all(&[NAK])?;
            *retries += 1;
        }
    }
    Ok(false)
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
            let is_data_frame = data.len() > 1 && (data[0] == SOH || data[0] == STX);
            if is_data_frame || data == [EOT] {
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

    // ---------------------------------------------------------------------
    // 接收方向测试
    // ---------------------------------------------------------------------

    /// 模拟发送方：等待本端握手字符后，依次发送给定的数据包，最后发 EOT。
    ///
    /// 每个包发出后等待 ACK；收到 NAK 则重发当前包。
    struct FakeSender {
        /// 待发送的包负载（已按块大小切分）
        payloads: Vec<Vec<u8>>,
        /// 已发送到的包索引
        idx: usize,
        /// 当前包是否已发出、正在等待应答
        awaiting_ack: bool,
        /// 已收到的握手字符（'C' 或 NAK）
        handshake_seen: bool,
        /// 是否已发送 EOT
        eot_sent: bool,
        /// 本端写入的字节（握手字符与 ACK/NAK）
        received: Vec<u8>,
        mode: ChecksumMode,
        /// 强制对第 N 个包（0-based）发一次损坏数据，用于测试重传
        corrupt_once: Option<usize>,
        corrupt_done: bool,
        /// 记录每个包实际发送次数
        send_counts: Vec<u32>,
    }

    impl FakeSender {
        fn new(data: &[u8], mode: ChecksumMode, block: usize) -> Self {
            Self {
                payloads: split_into_payloads(data, block),
                idx: 0,
                awaiting_ack: false,
                handshake_seen: false,
                eot_sent: false,
                received: Vec::new(),
                mode,
                corrupt_once: None,
                corrupt_done: false,
                send_counts: Vec::new(),
            }
        }

        /// 构造下一个待发送的字节序列
        fn next_bytes(&mut self) -> Option<Vec<u8>> {
            if !self.handshake_seen {
                return None;
            }
            if self.idx >= self.payloads.len() {
                return if self.eot_sent {
                    None
                } else {
                    self.eot_sent = true;
                    Some(vec![EOT])
                };
            }
            let no = ((self.idx + 1) & 0xFF) as u8;
            let payload = &self.payloads[self.idx];
            let mut frame = build_packet(no, payload, self.mode);
            // 按需破坏一次校验，触发接收方 NAK 重传
            if self.corrupt_once == Some(self.idx) && !self.corrupt_done {
                let last = frame.len() - 1;
                frame[last] ^= 0xFF;
                self.corrupt_done = true;
            }
            if self.send_counts.len() == self.idx {
                self.send_counts.push(1);
            } else {
                self.send_counts[self.idx] += 1;
            }
            self.awaiting_ack = true;
            Some(frame)
        }
    }

    impl SerialIo for FakeSender {
        fn read_byte(&mut self, _timeout: Duration) -> Result<Option<u8>, XmodemError> {
            // 本端（接收方）写入的字节在这里被“发送方”读到
            if self.received.is_empty() {
                return Ok(None);
            }
            let b = self.received.remove(0);
            Ok(Some(b))
        }
        fn write_all(&mut self, data: &[u8]) -> Result<(), XmodemError> {
            // 接收方写入：握手字符或 ACK/NAK
            for &b in data {
                match b {
                    CRC_CHAR | NAK if !self.handshake_seen => {
                        self.handshake_seen = true;
                    }
                    ACK => {
                        if self.awaiting_ack {
                            self.awaiting_ack = false;
                            self.idx += 1;
                        }
                    }
                    NAK => {
                        // 重传当前包：不推进索引
                        self.awaiting_ack = false;
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        fn flush_input(&mut self) -> Result<(), XmodemError> {
            self.received.clear();
            Ok(())
        }
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    /// 把 FakeSender 的待发数据喂给接收方。
    ///
    /// 由于 `receive` 会主动从 io 读字节，而 FakeSender 需要先看到握手字符
    /// 才产生数据，这里用一个包装层把两者串起来：读操作先尝试从发送方取
    /// 待发字节，取不到时返回 None（模拟超时）。
    struct LoopbackIo {
        sender: FakeSender,
        /// 发送方待发的字节队列
        pending: VecDeque<u8>,
    }

    impl LoopbackIo {
        fn new(sender: FakeSender) -> Self {
            Self {
                sender,
                pending: VecDeque::new(),
            }
        }

        /// 若待发队列为空，向发送方索取下一批数据
        fn refill(&mut self) {
            if !self.pending.is_empty() {
                return;
            }
            if let Some(bytes) = self.sender.next_bytes() {
                self.pending.extend(bytes);
            }
        }
    }

    impl SerialIo for LoopbackIo {
        fn read_byte(&mut self, _timeout: Duration) -> Result<Option<u8>, XmodemError> {
            self.refill();
            Ok(self.pending.pop_front())
        }
        fn write_all(&mut self, data: &[u8]) -> Result<(), XmodemError> {
            // 接收方写入的 ACK/NAK/握手字符交给发送方处理
            self.sender.write_all(data)
        }
        fn flush_input(&mut self) -> Result<(), XmodemError> {
            self.pending.clear();
            Ok(())
        }
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    #[test]
    fn test_receive_success_crc_128() {
        let data = vec![0x42u8; 300]; // 3 个 128 字节包
        let sender = FakeSender::new(&data, ChecksumMode::Crc, PAYLOAD_128);
        let mut io = LoopbackIo::new(sender);

        let mut events = Vec::new();
        let got = receive(
            &mut io,
            ChecksumMode::Crc,
            Duration::from_millis(200),
            Duration::from_millis(100),
            0,
            |p| events.push(p),
        )
        .unwrap();

        assert_eq!(got, data, "接收内容应与发送内容一致");
        assert!(matches!(events[0], Progress::WaitingSender));
        assert!(events
            .iter()
            .any(|e| matches!(e, Progress::HandshakeDone(ChecksumMode::Crc))));
        assert!(events
            .iter()
            .any(|e| matches!(e, Progress::Finished { packets: 3, bytes: 300 })));
    }

    #[test]
    fn test_receive_strips_sub_padding() {
        // 数据长度不是块大小的整数倍，末尾会被 SUB 填充
        let data = vec![0x7Fu8; 130];
        let sender = FakeSender::new(&data, ChecksumMode::Crc, PAYLOAD_128);
        let mut io = LoopbackIo::new(sender);

        let got = receive(
            &mut io,
            ChecksumMode::Crc,
            Duration::from_millis(200),
            Duration::from_millis(100),
            0,
            |_| {},
        )
        .unwrap();

        assert_eq!(got.len(), 130, "末尾 SUB 填充应被去除");
        assert_eq!(got, data);
    }

    #[test]
    fn test_receive_checksum_mode() {
        let data = vec![0x11u8; 100];
        let sender = FakeSender::new(&data, ChecksumMode::Checksum, PAYLOAD_128);
        let mut io = LoopbackIo::new(sender);

        let got = receive(
            &mut io,
            ChecksumMode::Checksum,
            Duration::from_millis(200),
            Duration::from_millis(100),
            0,
            |_| {},
        )
        .unwrap();
        assert_eq!(got, data);
    }

    #[test]
    fn test_receive_retransmits_on_corruption() {
        let data = vec![0x33u8; 200]; // 2 个包
        let mut sender = FakeSender::new(&data, ChecksumMode::Crc, PAYLOAD_128);
        sender.corrupt_once = Some(0); // 第一个包首次发送时损坏
        let mut io = LoopbackIo::new(sender);

        let got = receive(
            &mut io,
            ChecksumMode::Crc,
            Duration::from_millis(200),
            Duration::from_millis(100),
            0,
            |_| {},
        )
        .unwrap();

        assert_eq!(got, data, "损坏包重传后应完整接收");
        // 第一个包应被发送两次（首次损坏 + 重传）
        assert_eq!(io.sender.send_counts[0], 2, "损坏包应触发一次重传");
    }

    #[test]
    fn test_receive_1k_packets() {
        let data = vec![0x99u8; 2500]; // 3 个 1024 字节包
        let sender = FakeSender::new(&data, ChecksumMode::Crc, PAYLOAD_1024);
        let mut io = LoopbackIo::new(sender);

        let got = receive(
            &mut io,
            ChecksumMode::Crc,
            Duration::from_millis(200),
            Duration::from_millis(100),
            0,
            |_| {},
        )
        .unwrap();
        assert_eq!(got, data);
    }

    #[test]
    fn test_receive_skips_leading_noise() {
        // 发送方数据前混入设备提示符等噪声字节
        let data = vec![0x24u8; 50];
        let sender = FakeSender::new(&data, ChecksumMode::Crc, PAYLOAD_128);
        let mut io = LoopbackIo::new(sender);
        // 手工塞入噪声（模拟发送前设备回显）
        io.pending.extend(b"~ # \r\n".iter().copied());

        let got = receive(
            &mut io,
            ChecksumMode::Crc,
            Duration::from_millis(200),
            Duration::from_millis(100),
            0,
            |_| {},
        )
        .unwrap();
        assert_eq!(got, data, "前导噪声应被跳过");
    }

    #[test]
    fn test_receive_timeout_when_silent() {
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
        let err = receive(
            &mut Silent,
            ChecksumMode::Crc,
            Duration::from_millis(20),
            Duration::from_millis(10),
            0,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, XmodemError::Timeout));
    }

    #[test]
    fn test_receive_cancelled() {
        struct Cancelled;
        impl SerialIo for Cancelled {
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
                true
            }
        }
        let err = receive(
            &mut Cancelled,
            ChecksumMode::Crc,
            Duration::from_millis(200),
            Duration::from_millis(100),
            0,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, XmodemError::Cancelled));
    }

    #[test]
    fn test_receive_respects_max_packets() {
        let data = vec![0x01u8; 1000]; // 8 个包
        let sender = FakeSender::new(&data, ChecksumMode::Crc, PAYLOAD_128);
        let mut io = LoopbackIo::new(sender);

        let err = receive(
            &mut io,
            ChecksumMode::Crc,
            Duration::from_millis(200),
            Duration::from_millis(100),
            3, // 只允许 3 个包
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, XmodemError::TooManyRetries(3)));
    }

    #[test]
    fn test_send_receive_roundtrip() {
        // 端到端：用 send 生成的帧序列，验证 receive 能完整解析
        let data: Vec<u8> = (0..=255u8).cycle().take(1500).collect();
        let sender = FakeSender::new(&data, ChecksumMode::Crc, PAYLOAD_128);
        let mut io = LoopbackIo::new(sender);

        let got = receive(
            &mut io,
            ChecksumMode::Crc,
            Duration::from_millis(200),
            Duration::from_millis(100),
            0,
            |_| {},
        )
        .unwrap();
        assert_eq!(got, data, "往返数据应完全一致");
    }

    /// 真正的双端对接测试：`send` 与 `receive` 在两个线程里直接对话。
    ///
    /// 与 `LoopbackIo` 不同，这里两个方向都跑真实的协议实现，
    /// 能验证握手协商、序号递增、EOT 收尾等完整交互。
    #[test]
    fn test_send_and_receive_interoperate() {
        use std::sync::{Arc, Condvar, Mutex as StdMutex};

        /// 线程安全的双向字节管道
        #[derive(Default)]
        struct Pipe {
            to_sender: VecDeque<u8>,
            to_receiver: VecDeque<u8>,
        }

        struct Shared {
            pipe: StdMutex<Pipe>,
            cv: Condvar,
        }

        /// 接收方视角的 io
        struct PipeReceiver(Arc<Shared>);
        /// 发送方视角的 io
        struct PipeSender(Arc<Shared>);

        impl PipeReceiver {
            #[allow(dead_code)]
            fn read(&self) -> Option<u8> {
                let mut p = self.0.pipe.lock().unwrap();
                p.to_receiver.pop_front()
            }
        }

        impl SerialIo for PipeReceiver {
            fn read_byte(&mut self, timeout: Duration) -> Result<Option<u8>, XmodemError> {
                let mut p = self.0.pipe.lock().unwrap();
                let deadline = std::time::Instant::now() + timeout;
                loop {
                    if let Some(b) = p.to_receiver.pop_front() {
                        return Ok(Some(b));
                    }
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        return Ok(None);
                    }
                    let (guard, _) = self.0.cv.wait_timeout(p, deadline - now).unwrap();
                    p = guard;
                }
            }
            fn write_all(&mut self, data: &[u8]) -> Result<(), XmodemError> {
                let mut p = self.0.pipe.lock().unwrap();
                p.to_sender.extend(data.iter().copied());
                self.0.cv.notify_all();
                Ok(())
            }
            fn flush_input(&mut self) -> Result<(), XmodemError> {
                self.0.pipe.lock().unwrap().to_receiver.clear();
                Ok(())
            }
            fn is_cancelled(&self) -> bool {
                false
            }
        }

        impl SerialIo for PipeSender {
            fn read_byte(&mut self, timeout: Duration) -> Result<Option<u8>, XmodemError> {
                let mut p = self.0.pipe.lock().unwrap();
                // 在 timeout 内等待接收方写入应答
                let deadline = std::time::Instant::now() + timeout;
                loop {
                    if let Some(b) = p.to_sender.pop_front() {
                        return Ok(Some(b));
                    }
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        return Ok(None);
                    }
                    let (guard, _) = self
                        .0
                        .cv
                        .wait_timeout(p, deadline - now)
                        .unwrap();
                    p = guard;
                }
            }
            fn write_all(&mut self, data: &[u8]) -> Result<(), XmodemError> {
                let mut p = self.0.pipe.lock().unwrap();
                p.to_receiver.extend(data.iter().copied());
                self.0.cv.notify_all();
                Ok(())
            }
            fn flush_input(&mut self) -> Result<(), XmodemError> {
                Ok(())
            }
            fn is_cancelled(&self) -> bool {
                false
            }
        }

        let data: Vec<u8> = (0..=255u8).cycle().take(700).collect();
        let shared = Arc::new(Shared {
            pipe: StdMutex::new(Pipe {
                to_sender: VecDeque::new(),
                to_receiver: VecDeque::new(),
            }),
            cv: Condvar::new(),
        });

        // 接收方线程
        let rx_shared = shared.clone();
        let rx_handle = std::thread::spawn(move || {
            let mut io = PipeReceiver(rx_shared);
            receive(
                &mut io,
                ChecksumMode::Crc,
                Duration::from_millis(2000),
                Duration::from_millis(500),
                0,
                |_| {},
            )
        });

        // 发送方线程
        let tx_shared = shared.clone();
        let tx_data = data.clone();
        let tx_handle = std::thread::spawn(move || {
            let mut io = PipeSender(tx_shared);
            send(
                &mut io,
                &tx_data,
                Duration::from_millis(2000),
                Duration::from_millis(500),
                false,
                |_| {},
            )
        });

        let tx_result = tx_handle.join().expect("发送线程 panic");
        let rx_result = rx_handle.join().expect("接收线程 panic");

        if let Err(e) = &tx_result {
            panic!("发送失败: {e}");
        }
        let got = rx_result.expect("接收应成功");
        assert_eq!(got, data, "双端对接应完整还原数据");
    }
}
