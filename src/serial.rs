//! 串口管理模块
//!
//! 负责：枚举串口、打开/关闭串口、在后台线程中读写数据。
//! 串口读写是阻塞的，因此放在独立线程中运行，通过 channel 与异步运行时通信。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::xmodem::{self, Progress, SerialIo, XmodemError};

/// 串口配置，从前端传入
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerialConfig {
    pub port: String,
    #[serde(default = "default_baud")]
    pub baud_rate: u32,
    #[serde(default = "default_data_bits")]
    pub data_bits: u8,
    /// "none" | "odd" | "even"
    #[serde(default = "default_parity")]
    pub parity: String,
    /// "1" | "2"
    #[serde(default = "default_stop_bits")]
    pub stop_bits: u8,
    /// "none" | "software" | "hardware"
    #[serde(default = "default_flow_control")]
    pub flow_control: String,
}

fn default_baud() -> u32 {
    115200
}
fn default_data_bits() -> u8 {
    8
}
fn default_parity() -> String {
    "none".into()
}
fn default_stop_bits() -> u8 {
    1
}
fn default_flow_control() -> String {
    "none".into()
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            port: String::new(),
            baud_rate: default_baud(),
            data_bits: default_data_bits(),
            parity: default_parity(),
            stop_bits: default_stop_bits(),
            flow_control: default_flow_control(),
        }
    }
}

impl SerialConfig {
    /// 构建 serialport 的串口配置
    fn to_builder(&self) -> anyhow::Result<serialport::SerialPortBuilder> {
        let data_bits = match self.data_bits {
            5 => DataBits::Five,
            6 => DataBits::Six,
            7 => DataBits::Seven,
            8 => DataBits::Eight,
            other => anyhow::bail!("不支持的数据位: {other}"),
        };
        let parity = match self.parity.as_str() {
            "none" => Parity::None,
            "odd" => Parity::Odd,
            "even" => Parity::Even,
            other => anyhow::bail!("不支持的校验位: {other}"),
        };
        let stop_bits = match self.stop_bits {
            1 => StopBits::One,
            2 => StopBits::Two,
            other => anyhow::bail!("不支持的停止位: {other}"),
        };
        let flow_control = match self.flow_control.as_str() {
            "none" => FlowControl::None,
            "software" => FlowControl::Software,
            "hardware" => FlowControl::Hardware,
            other => anyhow::bail!("不支持的流控: {other}"),
        };
        Ok(serialport::new(&self.port, self.baud_rate)
            .data_bits(data_bits)
            .parity(parity)
            .stop_bits(stop_bits)
            .flow_control(flow_control)
            // 读超时取较小值（5ms）：超时只是让串口线程回到循环顶部
            // 重新检查指令队列，不会丢数据；值越小输出延迟越低。
            // XMODEM 传输期间会通过 set_timeout 临时改用更长的包超时。
            .timeout(Duration::from_millis(5)))
    }
}

/// 串口信息（用于前端下拉框）
#[derive(Debug, Clone, Serialize)]
pub struct PortInfo {
    /// 系统串口名，如 "COM3"
    pub name: String,
    /// 串口类型，如 "USB" / "PCI" / "Unknown"
    pub port_type: String,
    /// 面向用户的简短标签，如 "COM3 — USB-SERIAL CH340"
    pub label: String,
    /// 详细信息，用于 tooltip
    pub detail: String,
}

/// 枚举系统可用串口
pub fn list_ports() -> Vec<PortInfo> {
    match serialport::available_ports() {
        Ok(ports) => ports.into_iter().map(describe_port).collect(),
        Err(e) => {
            warn!("枚举串口失败: {e}");
            Vec::new()
        }
    }
}

/// 把 serialport 的串口信息转换为前端友好的结构
fn describe_port(p: serialport::SerialPortInfo) -> PortInfo {
    let port_type = match &p.port_type {
        serialport::SerialPortType::UsbPort(_) => "USB".to_string(),
        serialport::SerialPortType::PciPort => "PCI".to_string(),
        serialport::SerialPortType::BluetoothPort => "Bluetooth".to_string(),
        serialport::SerialPortType::Unknown => "Unknown".to_string(),
    };

    let detail = match &p.port_type {
        serialport::SerialPortType::UsbPort(info) => {
            let mut parts = Vec::new();
            if let Some(m) = &info.manufacturer {
                parts.push(m.clone());
            }
            if let Some(prod) = &info.product {
                parts.push(prod.clone());
            }
            parts.push(format!("VID:{:04X} PID:{:04X}", info.vid, info.pid));
            if let Some(sn) = &info.serial_number {
                parts.push(format!("SN:{sn}"));
            }
            format!("{} [{}]", p.port_name, parts.join(", "))
        }
        _ => format!("{} [{}]", p.port_name, port_type),
    };

    // 标签优先使用产品名，便于用户识别设备
    let label = match &p.port_type {
        serialport::SerialPortType::UsbPort(info) => match &info.product {
            Some(prod) => format!("{} — {}", p.port_name, prod),
            None => format!("{} — USB 串口", p.port_name),
        },
        _ => format!("{} ({})", p.port_name, port_type),
    };

    PortInfo {
        name: p.port_name,
        port_type,
        label,
        detail,
    }
}

/// 从串口线程发往异步运行时的事件
#[derive(Debug, Clone)]
pub enum SerialEvent {
    /// 收到终端数据
    Data(Vec<u8>),
    /// XMODEM 传输进度
    XmodemProgress(Progress),
    /// XMODEM 传输结束（Ok 表示成功）
    XmodemDone(Result<(), String>),
    /// XMODEM 接收完成，携带收到的数据
    XmodemReceived(Vec<u8>),
    /// 串口已关闭
    Closed,
    /// 串口错误
    Error(String),
}

/// 发送给串口线程的指令
enum SerialCommand {
    /// 写入原始数据
    Write(Vec<u8>),
    /// 执行 XMODEM 发送
    Xmodem {
        data: Vec<u8>,
        use_1k: bool,
        handshake_timeout: Duration,
        packet_timeout: Duration,
    },
    /// 执行 XMODEM 接收
    XmodemRecv {
        mode: xmodem::ChecksumMode,
        handshake_timeout: Duration,
        packet_timeout: Duration,
        max_packets: u32,
    },
    /// 取消当前 XMODEM 传输
    CancelXmodem,
    /// 关闭串口
    Shutdown,
}

/// 串口会话句柄
pub struct SerialSession {
    cmd_tx: Sender<SerialCommand>,
    /// 取消标志，供 XMODEM 传输轮询
    cancel_flag: Arc<AtomicBool>,
    /// 事件广播（终端数据、错误等）
    event_tx: broadcast::Sender<SerialEvent>,
    /// 串口名称
    pub port_name: String,
    /// 串口线程句柄
    handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl SerialSession {
    /// 打开串口并启动后台读写线程
    pub fn open(config: SerialConfig) -> anyhow::Result<Self> {
        let builder = config.to_builder()?;
        let port = builder
            .open()
            .map_err(|e| anyhow::anyhow!("打开串口 {} 失败: {e}", config.port))?;

        let (cmd_tx, cmd_rx) = mpsc::channel::<SerialCommand>();
        let (event_tx, _) = broadcast::channel::<SerialEvent>(1024);
        let cancel_flag = Arc::new(AtomicBool::new(false));

        let port_name = config.port.clone();
        let worker = SerialWorker {
            port,
            cmd_rx,
            event_tx: event_tx.clone(),
            cancel_flag: cancel_flag.clone(),
            port_name: port_name.clone(),
        };

        let handle = thread::Builder::new()
            .name(format!("serial-{port_name}"))
            .spawn(move || worker.run())
            .map_err(|e| anyhow::anyhow!("启动串口线程失败: {e}"))?;

        info!("串口 {port_name} 已打开");
        Ok(Self {
            cmd_tx,
            cancel_flag,
            event_tx,
            port_name,
            handle: Mutex::new(Some(handle)),
        })
    }

    /// 订阅串口事件
    pub fn subscribe(&self) -> broadcast::Receiver<SerialEvent> {
        self.event_tx.subscribe()
    }

    /// 向串口写入数据（终端模式下由用户输入）
    pub fn write(&self, data: Vec<u8>) -> anyhow::Result<()> {
        self.cmd_tx
            .send(SerialCommand::Write(data))
            .map_err(|_| anyhow::anyhow!("串口已关闭"))
    }

    /// 请求取消当前 XMODEM 传输
    pub fn cancel_xmodem(&self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
        let _ = self.cmd_tx.send(SerialCommand::CancelXmodem);
    }

    /// 执行 XMODEM 发送（异步等待完成）
    ///
    /// 进度与结果通过 [`SerialEvent`] 广播上报。
    pub fn xmodem_send(
        &self,
        data: Vec<u8>,
        use_1k: bool,
        handshake_timeout: Duration,
        packet_timeout: Duration,
    ) -> anyhow::Result<()> {
        self.cancel_flag.store(false, Ordering::SeqCst);
        self.cmd_tx
            .send(SerialCommand::Xmodem {
                data,
                use_1k,
                handshake_timeout,
                packet_timeout,
            })
            .map_err(|_| anyhow::anyhow!("串口已关闭"))
    }

    /// 执行 XMODEM 接收（下载）
    ///
    /// 进度与结果通过 [`SerialEvent`] 广播上报；收到的数据通过
    /// [`SerialEvent::XmodemReceived`] 下发。
    pub fn xmodem_receive(
        &self,
        mode: xmodem::ChecksumMode,
        handshake_timeout: Duration,
        packet_timeout: Duration,
        max_packets: u32,
    ) -> anyhow::Result<()> {
        self.cancel_flag.store(false, Ordering::SeqCst);
        self.cmd_tx
            .send(SerialCommand::XmodemRecv {
                mode,
                handshake_timeout,
                packet_timeout,
                max_packets,
            })
            .map_err(|_| anyhow::anyhow!("串口已关闭"))
    }

    /// 关闭串口（可重复调用，幂等）
    pub fn close(&self) {
        let _ = self.cmd_tx.send(SerialCommand::Shutdown);
        let handle = self
            .handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(h) = handle {
            let _ = h.join();
            info!("串口 {} 已关闭", self.port_name);
        }
    }
}

impl Drop for SerialSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// 串口后台工作线程
struct SerialWorker {
    port: Box<dyn SerialPort>,
    cmd_rx: Receiver<SerialCommand>,
    event_tx: broadcast::Sender<SerialEvent>,
    cancel_flag: Arc<AtomicBool>,
    port_name: String,
}

impl SerialWorker {
    fn run(mut self) {
        let mut buf = [0u8; 8192];
        debug!("串口 {} 工作线程已启动", self.port_name);
        loop {
            // 1) 处理**所有**已排队的指令。
            //    原来每次循环只取一条，多条指令会串行拖慢响应。
            let mut shutdown = false;
            loop {
                match self.cmd_rx.try_recv() {
                    Ok(SerialCommand::Write(data)) => {
                        if let Err(e) = self.port.write_all(&data) {
                            let _ = self
                                .event_tx
                                .send(SerialEvent::Error(format!("写入失败: {e}")));
                        }
                        // 不调用 flush()：Windows 上是阻塞的 FlushFileBuffers，
                        // 会等到硬件缓冲排空，显著增加按键回显延迟。
                        // 串口驱动本身有发送缓冲，无需显式刷新。
                    }
                    Ok(SerialCommand::Xmodem {
                        data,
                        use_1k,
                        handshake_timeout,
                        packet_timeout,
                    }) => {
                        self.run_xmodem(data, use_1k, handshake_timeout, packet_timeout);
                    }
                    Ok(SerialCommand::XmodemRecv {
                        mode,
                        handshake_timeout,
                        packet_timeout,
                        max_packets,
                    }) => {
                        self.run_xmodem_receive(
                            mode,
                            handshake_timeout,
                            packet_timeout,
                            max_packets,
                        );
                    }
                    Ok(SerialCommand::CancelXmodem) => {
                        // 取消标志已由调用方设置，这里补发 CAN 通知接收方
                        let _ = self
                            .port
                            .write_all(&[xmodem::CAN, xmodem::CAN, xmodem::CAN]);
                    }
                    Ok(SerialCommand::Shutdown) => {
                        debug!("串口 {} 收到关闭指令", self.port_name);
                        shutdown = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        shutdown = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            if shutdown {
                break;
            }

            // 2) 读取串口数据。
            //    用较短的读超时（5ms）降低输出延迟；读超时只是回到循环顶部
            //    重新检查指令队列，不会丢数据。
            match self.port.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => {
                    let _ = self.event_tx.send(SerialEvent::Data(buf[..n].to_vec()));
                    // 一次读满缓冲区说明还有数据，立刻再读一轮，
                    // 避免每轮都等一次超时
                    if n == buf.len() {
                        continue;
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => {
                    let _ = self
                        .event_tx
                        .send(SerialEvent::Error(format!("读取失败: {e}")));
                    break;
                }
            }
        }
        let _ = self.event_tx.send(SerialEvent::Closed);
    }

    /// 在串口线程内执行 XMODEM 发送，进度通过广播事件上报
    fn run_xmodem(
        &mut self,
        data: Vec<u8>,
        use_1k: bool,
        handshake_timeout: Duration,
        packet_timeout: Duration,
    ) {
        let event_tx = self.event_tx.clone();
        let result = {
            let mut io = PortIo {
                port: &mut self.port,
                cancel: &self.cancel_flag,
            };
            xmodem::send(
                &mut io,
                &data,
                handshake_timeout,
                packet_timeout,
                use_1k,
                |p| {
                    let _ = event_tx.send(SerialEvent::XmodemProgress(p));
                },
            )
        };
        let done = match result {
            Ok(()) => {
                info!("XMODEM 传输完成: {} 字节", data.len());
                Ok(())
            }
            Err(XmodemError::Cancelled) => {
                warn!("XMODEM 传输被取消");
                Err("传输被取消".to_string())
            }
            Err(e) => {
                error!("XMODEM 传输失败: {e}");
                Err(e.to_string())
            }
        };
        let _ = self.event_tx.send(SerialEvent::XmodemDone(done));
    }

    /// 在串口线程内执行 XMODEM 接收，进度与数据通过广播事件上报
    fn run_xmodem_receive(
        &mut self,
        mode: xmodem::ChecksumMode,
        handshake_timeout: Duration,
        packet_timeout: Duration,
        max_packets: u32,
    ) {
        let event_tx = self.event_tx.clone();
        let result = {
            let mut io = PortIo {
                port: &mut self.port,
                cancel: &self.cancel_flag,
            };
            xmodem::receive(
                &mut io,
                mode,
                handshake_timeout,
                packet_timeout,
                max_packets,
                |p| {
                    let _ = event_tx.send(SerialEvent::XmodemProgress(p));
                },
            )
        };
        match result {
            Ok(data) => {
                info!("XMODEM 接收完成: {} 字节", data.len());
                let _ = self.event_tx.send(SerialEvent::XmodemReceived(data));
                let _ = self.event_tx.send(SerialEvent::XmodemDone(Ok(())));
            }
            Err(XmodemError::Cancelled) => {
                warn!("XMODEM 接收被取消");
                let _ = self
                    .event_tx
                    .send(SerialEvent::XmodemDone(Err("接收被取消".to_string())));
            }
            Err(e) => {
                error!("XMODEM 接收失败: {e}");
                let _ = self.event_tx.send(SerialEvent::XmodemDone(Err(e.to_string())));
            }
        }
    }
}

/// 把 `serialport::SerialPort` 适配为 XMODEM 的 `SerialIo`
struct PortIo<'a> {
    port: &'a mut Box<dyn SerialPort>,
    cancel: &'a AtomicBool,
}

impl<'a> SerialIo for PortIo<'a> {
    fn read_byte(&mut self, timeout: Duration) -> Result<Option<u8>, XmodemError> {
        self.port
            .set_timeout(timeout)
            .map_err(|e| XmodemError::Io(std::io::Error::other(e.to_string())))?;
        let mut b = [0u8; 1];
        match self.port.read(&mut b) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(b[0])),
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(None),
            Err(e) => Err(XmodemError::Io(e)),
        }
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), XmodemError> {
        // 只写数据，不调用 flush()。
        //
        // Windows 上 flush() 等价于 FlushFileBuffers，会**阻塞直到硬件发送
        // 缓冲区完全排空**（1024 字节 @115200 约 89ms）。这会拖慢整体速度，
        // 更重要的是会让「写」与「读应答」之间的时序变得不可预测，
        // 造成接收方应答与发送方读取错位。
        //
        // XMODEM 是停等协议，接收方的 ACK/NAK 会自然串行化，无需显式 flush。
        self.port.write_all(data).map_err(XmodemError::Io)
    }

    fn flush_input(&mut self) -> Result<(), XmodemError> {
        self.port
            .clear(serialport::ClearBuffer::Input)
            .map_err(|e| XmodemError::Io(std::io::Error::other(e.to_string())))
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_values() {
        let cfg = SerialConfig::default();
        assert_eq!(cfg.baud_rate, 115200);
        assert_eq!(cfg.data_bits, 8);
        assert_eq!(cfg.parity, "none");
        assert_eq!(cfg.stop_bits, 1);
        assert_eq!(cfg.flow_control, "none");
    }

    #[test]
    fn test_config_deserialize_with_defaults() {
        let cfg: SerialConfig = serde_json::from_str(r#"{"port":"COM1"}"#).unwrap();
        assert_eq!(cfg.port, "COM1");
        assert_eq!(cfg.baud_rate, 115200);
    }

    #[test]
    fn test_config_to_settings_rejects_bad_values() {
        let mut cfg = SerialConfig::default();
        cfg.parity = "invalid".into();
        assert!(cfg.to_builder().is_err());

        let mut cfg = SerialConfig::default();
        cfg.data_bits = 9;
        assert!(cfg.to_builder().is_err());

        let mut cfg = SerialConfig::default();
        cfg.stop_bits = 3;
        assert!(cfg.to_builder().is_err());
    }

    #[test]
    fn test_config_to_settings_ok() {
        let cfg = SerialConfig {
            port: "COM3".into(),
            baud_rate: 9600,
            data_bits: 7,
            parity: "even".into(),
            stop_bits: 2,
            flow_control: "hardware".into(),
        };
        assert!(cfg.to_builder().is_ok());
    }

    #[test]
    fn test_describe_port_usb() {
        let info = serialport::SerialPortInfo {
            port_name: "COM7".into(),
            port_type: serialport::SerialPortType::UsbPort(serialport::UsbPortInfo {
                vid: 0x1A86,
                pid: 0x7523,
                serial_number: Some("5".into()),
                manufacturer: Some("wch.cn".into()),
                product: Some("USB-SERIAL CH340".into()),
            }),
        };
        let p = describe_port(info);
        assert_eq!(p.name, "COM7");
        assert_eq!(p.port_type, "USB");
        assert_eq!(p.label, "COM7 — USB-SERIAL CH340");
        assert!(p.detail.contains("wch.cn"));
        assert!(p.detail.contains("VID:1A86"));
    }

    #[test]
    fn test_describe_port_unknown() {
        let info = serialport::SerialPortInfo {
            port_name: "COM1".into(),
            port_type: serialport::SerialPortType::Unknown,
        };
        let p = describe_port(info);
        assert_eq!(p.port_type, "Unknown");
        assert_eq!(p.label, "COM1 (Unknown)");
    }
}
