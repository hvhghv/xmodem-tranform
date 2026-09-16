# 终端字体许可说明

本目录下的 woff2 字体均为**开源字体**，可自由分发。它们被内嵌进
`xmodem-tranform` 二进制（见 `src/web.rs` 的 `FONTS` 常量），
通过 `GET /fonts/<name>.woff2` 提供给前端使用。

内嵌的目的是保证**离线环境**（本工具的目标场景是嵌入式设备）也能正常显示终端字体。

## 字体清单

| 文件 | 字体 | 版权所有者 | 许可 |
| --- | --- | --- | --- |
| `jetbrains-mono.woff2` | JetBrains Mono | JetBrains s.r.o. | SIL Open Font License 1.1 |
| `fira-code.woff2` | Fira Code | The Mozilla Foundation | SIL Open Font License 1.1 |
| `cascadia-code.woff2` | Cascadia Code | Microsoft Corporation | SIL Open Font License 1.1 |
| `source-code-pro.woff2` | Source Code Pro | Adobe | SIL Open Font License 1.1 |
| `ibm-plex-mono.woff2` | IBM Plex Mono | IBM Corp. | SIL Open Font License 1.1 |
| `hack.woff2` | Hack | Source Foundry | MIT License |
| `inconsolata.woff2` | Inconsolata | The Inconsolata Project Authors | SIL Open Font License 1.1 |
| `ubuntu-mono.woff2` | Ubuntu Mono | Canonical Ltd. | Ubuntu Font License 1.0 |
| `space-mono.woff2` | Space Mono | Colophon Foundry / Google | SIL Open Font License 1.1 |
| `roboto-mono.woff2` | Roboto Mono | Google Inc. | Apache License 2.0 |
| `cousine.woff2` | Cousine | Google Inc. | Apache License 2.0 |

## 许可要点

### SIL Open Font License 1.1

允许自由使用、研究、修改与再分发（包括嵌入与商业用途），
要求：

- 不得单独出售字体本身
- 修改后的字体不得使用原保留字体名称（Reserved Font Name）
- 再分发时须附带本许可

完整文本：<https://scripts.sil.org/OFL>

### Apache License 2.0

允许自由使用、修改与再分发，须保留版权声明与许可声明。

完整文本：<https://www.apache.org/licenses/LICENSE-2.0>

### MIT License

允许自由使用、修改与再分发，须保留版权声明与许可声明。

完整文本：<https://opensource.org/licenses/MIT>

### Ubuntu Font License 1.0

允许自由使用、修改与再分发（包括嵌入），
要求衍生字体不得使用 "Ubuntu" 名称。

完整文本：<https://ubuntu.com/legal/font-licence>

## 来源

字体文件下载自 [jsDelivr](https://www.jsdelivr.com/) 镜像的
[@fontsource](https://fontsource.org/) 包（Latin 子集），
`hack.woff2` 来自 `hack-font` npm 包。

如需更新字体，请重新从上述来源下载并保持文件名不变，
`src/web.rs` 中的 `FONTS` 常量即按文件名映射 URL 路径。
