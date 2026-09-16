# xmodem-tranform

[![CI](https://github.com/hvhghv/xmodem-tranform/actions/workflows/ci.yml/badge.svg)](https://github.com/hvhghv/xmodem-tranform/actions/workflows/ci.yml)
[![Release](https://github.com/hvhghv/xmodem-tranform/actions/workflows/release.yml/badge.svg)](https://github.com/hvhghv/xmodem-tranform/actions/workflows/release.yml)

用 Rust 实现的 XMODEM 串口文件传输工具，内置 HTML 前端界面与串口终端。

## 下载

从 [Releases](https://github.com/hvhghv/xmodem-tranform/releases) 页面下载对应平台的压缩包。

| 平台 | 架构 | 链接方式 | 包名后缀 | 说明 |
| --- | --- | --- | --- | --- |
| Windows | x64 | 动态 | `windows-x64` | — |
| Linux | x64 | 动态 | `linux-x64-gnu` | 需 glibc 2.17+ |
| Linux | arm64 | 动态 | `linux-arm64-gnu` | — |
| Linux | armhf | 动态 | `linux-armhf-gnu` | armv7 硬浮点 ABI |
| Linux | armel | 动态 | `linux-armel-gnu-nolibudev` | armv7 软浮点 ABI，**无 libudev** |
| Linux | riscv64 | 动态 | `linux-riscv64-gnu` | — |
| Linux | x64 | 动态 / 静态 | `linux-x64-musl-{dynamic,static}` | — |
| Linux | arm64 | 动态 / 静态 | `linux-arm64-musl-{dynamic,static}` | — |
| Linux | arm | 动态 / 静态 | `linux-arm-musl-{dynamic,static}` | armv7 软浮点 ABI |
| Linux | riscv64 | 动态 / 静态 | `linux-riscv64-musl-{dynamic,static}` | — |

### 如何选择

- **不确定选哪个** → 优先 `musl-static`（静态链接，不依赖系统库，拿到即用）
- **要在普通桌面发行版运行** → 选 `gnu`（动态链接 glibc）。
  **注意**：`musl-dynamic` 需要系统提供 `/lib/ld-musl-<arch>.so.1`，
  Ubuntu / Debian / Fedora 默认**没有**这个文件，只有 Alpine 或装了 musl 运行时的系统才能直接运行
- **要完整的串口设备信息**（界面显示 `COM16 — USB-SERIAL CH340` 这类厂商名）→ 选 `gnu` 且**不是** `armel-nolibudev`
- **嵌入式设备 / 容器** → 选 `musl-static`，体积小且无依赖

> **关于 `armel-gnu-nolibudev`**：Ubuntu 未提供 armel（armv7 软浮点）架构的 `libudev`
> 开发包，因此该版本编译时关闭了 `libudev` feature。串口枚举会退化为扫描 `/dev/tty*`，
> 只显示端口名，不显示设备厂商与 VID/PID。功能不受影响，可正常收发数据。

## 功能

- **XMODEM 文件发送**：支持标准 XMODEM（128 字节包）与 XMODEM-1K（1024 字节包）
- **双校验模式**：自动识别接收方的握手信号，支持 CRC-16/XMODEM 与 8 位累加和
- **Web GUI**：前端采用纯 HTML/CSS/JavaScript，无需构建工具，页面内嵌于二进制
- **交互式串口终端**：两种输入模式可选
  - **终端模式**：点击终端后直接敲键盘，**逐键即时发送**，无需回车确认
    - 支持 Enter / Backspace / Tab / Esc / 方向键 / Home / End / PageUp / PageDown
    - `Ctrl+A`~`Ctrl+Z` 映射为控制字符，`Ctrl+C` 发送中断
  - **文本框模式**：整段输入后点发送（或回车），支持 Shift+回车不追加行尾
- **HEX 收发**：显示与发送均可切 HEX；发送支持 `AA BB`、`0xAA,0xBB`、`AABB` 等写法
- **时间戳**：可选在每行输出前添加毫秒级时间戳
- **行尾可选**：CR / LF / CRLF / 无
- **数据导出**：把接收到的全部数据导出为文本 / HEX / 原始二进制
- **10 套界面主题**：6 套深/浅色 + 4 套清新配色，选择后自动记忆（见下文）
- **11 款内置开源字体 + 13 款系统字体**：单一选择框分两段，自动检测可用性（见下文）
- **mini 精简构建**：musl-static，不嵌字体，体积 2.88 MB → **1.24 MB**，功能不变（见下文）
- **完整 ANSI 颜色支持**（见下文）
- **实时进度**：显示包序号、字节数、百分比与重传提示
- **可取消传输**：任意时刻中止发送，并向对端发送 CAN 序列

## 快速开始

```bash
cargo run --release
```

程序默认监听 `127.0.0.1:8080` 并自动打开浏览器。

### 命令行参数

| 参数 | 说明 | 默认值 |
| --- | --- | --- |
| `-H, --host <HOST>` | 监听地址 | `127.0.0.1` |
| `-p, --port <PORT>` | 监听端口 | `8080` |
| `--no-open` | 启动后不自动打开浏览器 | — |
| `-h, --help` | 显示帮助 | — |

```bash
# 监听所有网卡，供局域网其他机器访问
cargo run --release -- --host 0.0.0.0 --port 9000 --no-open
```

## 使用流程

1. 启动程序，浏览器打开界面
2. 在「串口设置」中选择串口与波特率，点击 **打开串口**
3. 让目标设备进入 XMODEM 接收模式（通常发送 `rx` 或类似命令）
4. 在「XMODEM 发送」中选择文件，按需勾选 XMODEM-1K，点击 **开始发送**
5. 观察进度条与日志

### 使用终端

终端工具栏提供以下选项：

| 控件 | 选项 | 说明 |
| --- | --- | --- |
| 输入 | 终端 / 文本框 | 逐键即时发送，或整段输入后发送 |
| 显示 | 文本 / HEX | 接收数据的显示格式 |
| 发送 | 文本 / HEX | 发送数据的解析格式 |
| 时间戳 | 勾选 | 每行输出前添加 `HH:MM:SS.mmm` |
| 本地回显 | 勾选 | 本地回显按键，**默认关闭**（见下方说明） |
| 行尾 | CR / LF / CRLF / 无 | 发送时追加的行尾字符 |
| 导出 | 文本 / HEX / 原始 | 导出格式，点「导出」下载 |

**终端模式**：点击终端区域（或点「聚焦终端」）即可直接敲键盘，每个按键**立即**发往串口。
终端会显示一个插入符（聚焦时为闪烁的竖线，未聚焦时为空心竖线），位置随输出实时更新。
光标是独立的插入点，**不占用也不改变任何字符**。

> **关于「本地回显」**：多数串口设备（如 Linux shell、路由器 CLI）会自己回显收到的字符。
> 此时若再本地回显，每个字符就会显示两遍。因此该选项**默认关闭**，
> 只在设备不回显时才需要勾选。
>
> 关闭本地回显时，**光标位置完全由设备回显流驱动**，本地不会自行移动光标。
> 这是必要的：若本地也跟踪按键，就会与设备状态错位，表现为按方向键后内容被覆盖或丢失。
> 勾选本地回显后，方向键 / Home / End / 退格才会在本地同步生效。

> **提示**：在嵌入式设备上，若快速输入导致回显乱码（设备端收到乱码命令），
> 说明设备处理速度跟不上，可降低波特率或放慢输入。这不是显示问题——
> 设备本身收到的就是乱码。

**文本框模式**：在底部输入框输入内容，回车发送；`Shift+回车` 发送但不追加行尾，
便于发送纯数据。HEX 发送模式下输入十六进制字符串，本地回显会显示实际发出的字节。

**HEX 显示**：每行 16 字节，右侧附 ASCII 对照（不可打印字符显示为 `.`）。
跨数据包的字节会先缓存在一起，凑满一行才输出，因此行不会被拆包切断。

### 数据导出

点「导出」按钮把接收到的数据下载到本地，支持三种格式：

| 格式 | 扩展名 | 内容 |
| --- | --- | --- |
| 文本 | `.log` | 每行带毫秒时间戳，保留原始 ANSI 序列 |
| HEX | `.hex` | 带偏移量的 HEX dump + ASCII 对照 |
| 原始 | `.bin` | 未经处理的原始字节，可用于二进制分析 |

**导出不受屏幕显示行数限制**：终端 DOM 只保留最近 3000 个节点，
早期内容会被裁掉；导出使用独立维护的完整字节记录（上限 8 MB），
因此长时间、大数据量接收后仍能导出全部数据。点「清屏」会同时重置导出记录。

> **提示**：握手超时默认 60 秒，便于在点击发送后再去操作设备。若设备已就绪，可调小该值。
> 用户手动向上滚动时自动滚动会暂停，滚回底部后恢复。

## 项目结构

```
src/
  main.rs      # 入口、命令行参数解析
  xmodem.rs    # XMODEM 协议核心（组包、CRC、发送状态机）
  serial.rs    # 串口枚举、打开/关闭、后台读写线程
  protocol.rs  # 前后端 WebSocket 消息协议、base64 编解码
  web.rs       # axum Web 服务与 WebSocket 处理
static/
  index.html   # 前端界面（内嵌进二进制）
  fonts/       # 终端字体 woff2（内嵌进二进制）
```

### ANSI 颜色支持

设备（如 `ls --color`、`busybox`、各类 shell）会发送 SGR 序列为输出着色。终端支持：

| 类别 | 支持的序列 | 说明 |
| --- | --- | --- |
| 基础属性 | `0` `1` `2` `3` `4` `7` | 重置、粗体、暗色、斜体、下划线、反显 |
| 属性关闭 | `22` `23` `24` `27` | 分别关闭粗体/暗色、斜体、下划线、反显 |
| 标准前景 | `30`-`37` / `39` | 8 色，`39` 恢复默认 |
| 标准背景 | `40`-`47` / `49` | 8 色，`49` 恢复默认 |
| 高亮前景 | `90`-`97` | 亮色 8 色 |
| 高亮背景 | `100`-`107` | 亮色 8 色 |
| 256 色 | `38;5;n` / `48;5;n` | 含 6x6x6 色立方与 24 级灰阶 |
| 真彩色 | `38;2;r;g;b` / `48;2;r;g;b` | 24 位 RGB |
| 冒号写法 | `38:5:n` / `38:2:r:g:b` | 部分设备使用此变体 |

属性采用**累积**语义：`1;32` 会同时得到粗体与绿色，而不是后者覆盖前者。

其他 CSI 序列：`J` 清屏、`K` 清行、`H`/`f` 光标定位、`n` 光标位置查询（仅忽略不回复）、
`h`/`l` 模式设置（忽略）。未实现的序列一律**丢弃**，不会作为文本显示。

### 界面主题

界面右上角可切换主题，选择会写入 `localStorage`，下次打开自动恢复。

| 主题 | 定位 | 特点 |
| --- | --- | --- |
| 深空蓝 | 默认 | 深蓝灰底 + 蓝色强调，长时间使用不易疲劳 |
| 终端绿 | 复古 | 纯黑底 + 荧光绿，经典 CRT 终端观感 |
| 浅色 | 日间 | 浅灰底 + 深色文字，适合明亮环境 |
| 高对比 | 无障碍 | 纯黑底 + 高饱和黄，强光环境或视力辅助 |
| 暖阳 | 护眼 | 暖棕底 + 琥珀色，低蓝光 |
| 紫罗兰 | 个性 | 低亮度紫调 |
| 薄荷 | 清新 | 淡青绿底 + 深青强调，清凉通透 |
| 樱粉 | 清新 | 淡粉底 + 玫红强调，柔和明快 |
| 天青 | 清新 | 淡蓝底 + 深蓝强调，清朗开阔 |
| 柠檬 | 清新 | 淡黄绿底 + 橄榄绿强调，明快清爽 |

实现方式：所有颜色都走 CSS 自定义属性，主题只需覆盖 `:root` 中的变量。
切换通过 `<html data-theme="...">` 完成，`default` 表示不设该属性。

主题同时作用于**终端配色**——ANSI 8 色、亮色 8 色与背景色都随主题变化，
因此浅色主题下 `ls --color` 的输出依然清晰可读。

清新主题（薄荷 / 樱粉 / 天青 / 柠檬）为浅色底，配色经过 WCAG 对比度校验：
正文与背景对比度 ≥ 9.3，次要文字 ≥ 4.7，按钮文字 ≥ 4.99，
均达到 AA 级（≥ 4.5），与已有的「浅色」主题持平或更优。

> 首屏防闪白：`<head>` 中有一段内联脚本，在样式表之前就把主题写到 `<html>` 上，
> 避免先渲染默认主题再跳变。
### 终端字体

界面右上角有一个**字体选择框**（自定义下拉），分成两段：

```
默认（不指定）
── 内置字体 · 离线可用 ──
   JetBrains Mono / Fira Code / Cascadia Code / ...
── 系统字体 · 需本机已安装 ──
   Consolas / Cascadia Mono / Courier New / ...
```

分组标题不可选、带分隔线、用界面字体显示；条目用等宽字体预览字形。
列表限高 320px 并带滚动条。

> **为什么不用原生 `<select>` + `<optgroup>`**：原生 select 的弹出列表
> 由浏览器内部渲染，CSS 几乎无法控制 —— 实测 `optgroup` 的
> `font-weight` / `font-size` 在弹层里**完全不生效**，`option` 的颜色还会
> 盖住 `optgroup`，导致分组标题与普通条目分不清、disabled 分组没有视觉提示、
> 列表高度也不可控。因此改为自绘下拉（`div` + 绝对定位）。

选择写入 `localStorage` 自动记忆。

#### 内置字体（随程序分发，离线可用）

通过 `GET /fonts/<name>.woff2` 提供，**完全离线可用**——本工具面向嵌入式设备场景，
不能依赖 CDN。字体文件位于 `static/fonts/`，由 `src/web.rs` 用 `include_bytes!` 嵌入。

| 字体 | 许可 | 特点 |
| --- | --- | --- |
| JetBrains Mono | SIL OFL 1.1 | 专为代码设计，字形偏圆润 |
| Fira Code | SIL OFL 1.1 | Mozilla 出品，编程连字最丰富 |
| Cascadia Code | SIL OFL 1.1 | 微软出品，Windows Terminal 默认字体 |
| Source Code Pro | SIL OFL 1.1 | Adobe 出品，中性耐看 |
| IBM Plex Mono | SIL OFL 1.1 | IBM 设计语言，辨识度高 |
| Hack | MIT | 基于 Bitstream Vera，小字号清晰 |
| Inconsolata | SIL OFL 1.1 | 窄字宽，同屏可容纳更多列 |
| Ubuntu Mono | Ubuntu Font License | Ubuntu 品牌字体，风格独特 |
| Space Mono | SIL OFL 1.1 | 复古打字机风格 |
| Roboto Mono | Apache-2.0 | Google 出品，Android 生态常见 |
| Cousine | Apache-2.0 | 与 Courier New 度量兼容 |

11 款字体合计约 289 KB，二进制体积因此增加约 0.32 MB。

#### 系统字体（使用本机已安装的字体）

| 字体 | 平台 |
| --- | --- |
| Consolas / Cascadia Mono / Courier New / Lucida Console | Windows |
| Menlo / Monaco / SF Mono | macOS |
| DejaVu Sans Mono / Liberation Mono / Noto Sans Mono | Linux |
| Source Han Mono SC / Sarasa Mono SC / Microsoft YaHei Mono | 中文等宽 |

程序会**自动检测本机是否安装**，未安装的选项置灰并标注「（未安装）」，
避免选了却看不到效果。检测方式是用 canvas 测量文本宽度：
`"字体名", monospace` 与纯 `monospace` 宽度不同即说明字体生效。

> 基准宽度必须用**相同的回退链**（monospace）来测。早期版本拿
> `monospace` / `sans-serif` / `serif` 三个族当基准，结果不存在的字体
> 回退到 `monospace` 后宽度与 `sans-serif` 不同，被误判为「已安装」
> ——已用假字体名实测确认并修正。

#### 两段在 CSS 中仍是隔离的

选择框虽然只有一个，但底层仍是**两个互相隔离的槽**，由不同属性控制：

| 属性 | 槽变量 | 取值来源 |
| --- | --- | --- |
| `data-font` | `--font-slot-builtin` | `@font-face` 家族名（`"XxxTool"`） |
| `data-font-sys` | `--font-slot-system` | 本机字体名 |

选项 `value` 用前缀区分来源，避免两段出现同名 key：

```
none                  不指定，沿用 --mono 默认栈
builtin:<name>        内置字体
system:<name>         系统字体
```

由于选择框一次只能有一个值，**选中某一项时会清空另一维度**，
两个 `localStorage` 键（`xmodem.font` / `xmodem.sysfont`）同步更新。
最终栈为 `内置 -> 系统 -> 通用等宽回退`。

> **实现陷阱**：CSS 变量为空时 `var()` 会替换出空槽位，使整条
> `font-family` 声明失效，最终继承 body 的非等宽字体。
> 因此两槽的默认值必须**非空**（都指向 `--mono`），否则拼接出的列表非法。

> **连字默认关闭**：Fira Code、JetBrains Mono、Cascadia Code 等含编程连字，
> 会把 `->` `!=` `<=` 合并成单个字形。终端必须保证「一个字符 = 一个等宽格」，
> 连字会破坏列对齐，因此统一设置 `font-variant-ligatures: none`。
> 系统字体同样处理，因为用户可能选了带连字的系统字体。

字体只作用于等宽区域（终端、日志、输入框、进度文本），界面其余部分仍用系统 UI 字体。

### 终端渲染模型

终端采用**行缓冲模型**而非简单追加：屏幕由若干行组成，每行是带样式的字符单元数组，
光标位置记录为 `(row, col)`。所有输出先修改缓冲，再由 `requestAnimationFrame` 每帧统一渲染。

这样才能正确处理光标定位与重绘——方向键、`readline` 行编辑、进度条刷新都依赖这些能力：

| CSI 序列 | 功能 |
| --- | --- |
| `A` `B` `C` `D` | 光标上/下/右/左移 |
| `G` `d` | 光标绝对列/行定位 |
| `H` `f` | 光标定位（行列） |
| `J` | 清屏（`0` 光标到末尾，`2`/`3` 整屏） |
| `K` | 清行（`0` 到行尾，`1` 行首到光标，`2` 整行） |
| `P` `@` `X` | 删除字符 / 插入空位 / 擦除字符 |
| `?25h` `?25l` | 显示 / 隐藏光标 |

### 终端渲染性能

串口高频输出时，若每个数据批次都直接操作 DOM，会因频繁的样式重算与强制重排导致界面卡顿。
前端采用以下策略保证流畅：

- **批量渲染**：所有输出先写入行缓冲，由 `requestAnimationFrame` 每帧统一渲染一次
- **样式合并**：渲染时把连续同样式的字符合并为一个 `<span>`，显著减少节点数
- **行数上限**：行缓冲最多 5000 行，超出部分从头部裁剪
- **滚动优化**：仅在用户本来就在底部时才自动滚动，避免打断向上翻阅
- **增量 UTF-8 解码**：用 `TextDecoder` 的 `stream` 模式，避免多字节字符被拆包时出现乱码

### 架构说明

串口读写是阻塞式 API，因此放在独立的 OS 线程中运行，通过 `mpsc` 接收指令、
通过 `tokio::sync::broadcast` 广播事件（终端数据、传输进度、错误）。
异步运行时负责 Web 服务与 WebSocket，两者之间以 channel 解耦。

```
浏览器 ──WebSocket──> axum ──mpsc──> 串口线程 ──serialport──> 设备
   ^                    |                |
   └────broadcast───────┴────────────────┘
```

## XMODEM 协议实现说明

数据包格式：

```
SOH/STX | 包序号 | 255-包序号 | 数据(128/1024) | 校验(1或2字节)
  0x01      n        ~n         ...填充 0x1A...    checksum / CRC16
  0x02
```

- 接收方发送 `'C'` (0x43) 请求 CRC 模式，或 `NAK` (0x15) 请求 checksum 模式
- 每个数据包等待 `ACK` (0x06)；收到 `NAK` 或超时则重传，最多 10 次
- 传输结束发送 `EOT` (0x04)，等待 `ACK` 确认
- 取消时连续发送 3 个 `CAN` (0x18)

## 测试

```bash
cargo test
```

共 32 个单元测试，覆盖：

- CRC-16/XMODEM 标准测试向量（`"123456789"` → `0x31C3`）
- 128/1024 字节包的组包与校验（CRC 与 checksum 两种模式）
- 包解析、序号反码校验、损坏检测
- XMODEM 发送状态机（成功路径、取消、握手超时、连发 NAK 不错位）
- base64 编解码往返与边界情况
- 串口配置校验与串口信息格式化

## 构建

### 本地构建

```bash
# 默认（启用 libudev，Linux glibc 下可显示串口设备厂商信息）
cargo build --release

# 关闭 libudev（串口枚举退化为扫描 /dev/tty*）
cargo build --release --no-default-features

# mini 精简版（体积优先，见下文）
cargo build --profile release-mini --no-default-features --features mini
```

### mini 精简构建

面向存储紧张的嵌入式设备。**功能完全一致**（终端模拟、完整 ANSI 颜色、
XMODEM、导出等都不变），只做两件事：

1. 不嵌入字体（省约 289 KB）
2. 使用体积优先的编译选项（`opt-level = "z"` / `strip` / `panic = "abort"`）

| 构建 | 体积 | 说明 |
| --- | --- | --- |
| 完整版 | **2.88 MB** | 含 11 款内嵌字体 |
| mini 版 | **1.24 MB** | 无字体 |

> 节省 1.64 MB（**57%**）。其中字体只占 0.29 MB，
> 大头来自 `opt-level = "z"` 与 `strip` 对 Rust 依赖的压缩。

**mini 版与完整版共用同一份 `index.html`**，不单独维护精简前端：

- HTML 本身仅 93 KB（gzip 后 27 KB），远小于字体的 289 KB，
  为它单独做一份精简版收益有限，还会与完整版漂移
- 主题切换是纯 CSS，不依赖字体文件，因此 mini 下**依然可用**
- 内置字体选择器在 mini 下没有字体文件，前端会**自动检测并置灰**，
  标注「（不可用）」；系统字体维度不受影响，仍可正常使用

### Cargo features

| feature | 默认 | 说明 |
| --- | --- | --- |
| `libudev` | ✅ 启用 | 通过 udev 获取串口设备的 USB VID/PID、厂商与产品名。仅 Linux glibc 有效；musl 目标自动跳过（`serialport` 用 `not(target_env = "musl")` 门控） |
| `mini` | — | 精简构建：不嵌入字体，配合 `release-mini` profile 使用 |

### 编译 profile

| profile | 用途 | 关键设置 |
| --- | --- | --- |
| `release` | 默认发布 | `opt-level = 3`、`lto = true`、`codegen-units = 1` |
| `release-mini` | mini 精简版 | `opt-level = "z"`、`lto = "fat"`、`panic = "abort"`、`strip = true` |

### CI 构建矩阵

推送代码或 PR 时，`.github/workflows/ci.yml` 会构建以下 14 个目标：

| 平台 | 架构 | 目标三元组 | Runner | 构建方式 |
| --- | --- | --- | --- | --- |
| Windows | x64 | `x86_64-pc-windows-msvc` | `windows-latest` | 原生 |
| gnu | x64 | `x86_64-unknown-linux-gnu` | `ubuntu-latest` | 原生 |
| gnu | arm64 | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | 原生 |
| gnu | armhf | `armv7-unknown-linux-gnueabihf` | `ubuntu-latest` | 交叉 |
| gnu | armel | `armv7-unknown-linux-gnueabi` | `ubuntu-latest` | 交叉（无 libudev） |
| gnu | riscv64 | `riscv64gc-unknown-linux-gnu` | `ubuntu-latest` | 交叉 |
| musl | x64 / arm64 | `*-unknown-linux-musl` | 视架构而定 | 原生，各出动态与静态两个版本 |
| musl | arm / riscv64 | `*-unknown-linux-musl*` | `ubuntu-latest` | 交叉，各出动态与静态两个版本 |

**交叉编译说明**：

- gnu 交叉编译需要目标架构的 `libudev`，CI 从 Ubuntu 仓库下载对应 deb 并解压为 sysroot，
  再用 `PKG_CONFIG_LIBDIR` / `PKG_CONFIG_SYSROOT_DIR` 让 pkg-config 在其中查找
- musl 交叉编译使用 [hvhghv/cross-software](https://github.com/hvhghv/cross-software)
  的 `v15.1.0-musl-gcc` release 工具链（`lto-nodebug` 变体，GCC 15.1.0 / musl 1.2.6），
  无需任何系统库（musl 目标不依赖 libudev）
- 该工具链只提供 **soft-float** 的 `arm-linux-musleabi`，没有 hard-float 变体，
  因此 musl 不构建 `armhf`（`armv7-unknown-linux-musleabihf`）；
  需要硬浮点 ABI 请使用 gnu 版的 `armhf`
- 交叉编译产物无法在 x64 runner 上执行，因此**只在原生目标（x64 / arm64）运行测试**；
  测试为纯逻辑单元测试，与架构无关

### 发布

推送 `v*` 标签（如 `v0.1.0`）会触发 `.github/workflows/release.yml`，
构建全部平台、生成 `SHA256SUMS.txt` 并创建 GitHub Release：

```bash
git tag v0.1.0
git push origin v0.1.0
```

也可在 Actions 页面手动触发 Release workflow 并指定标签名。

## 依赖

| crate | 用途 |
| --- | --- |
| `serialport` | 跨平台串口访问 |
| `tokio` | 异步运行时 |
| `axum` | HTTP 与 WebSocket 服务 |
| `serde` / `serde_json` | 消息序列化 |
| `tracing` | 日志 |
| `thiserror` / `anyhow` | 错误处理 |
| `open` | 自动打开浏览器 |

## 已知限制

- 仅实现 XMODEM 发送方向，接收方向提供了 `parse_packet` / `verify_frame` 解析工具但未接入界面
- 同一时刻只允许一个串口会话
- 前端未实现 YMODEM 的批量文件与 1K 扩展协商
- ANSI 支持为简化实现：颜色、属性、清屏/清行、光标定位已支持；
  未实现全屏光标寻址与光标移动（`A`/`B`/`C`/`D`），因此 `vim`、`top` 等
  需要完整终端模型的程序无法正常显示
