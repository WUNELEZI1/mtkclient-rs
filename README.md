\# zyb-client



一个基于 Rust 开发的 MTK（联发科）芯片底层调试与烧录工具客户端。



\## 项目简介



zyb-client 是一款面向 MTK 平台设备的底层调试与烧录工具，通过与 \[mtkclient](https://github.com/bkerler/mtkclient) 库交互，实现对联发科芯片设备的 EDL/Preloader 模式烧录、固件分析、 bootloader 解锁等功能。该工具内置了 Windows 平台所需的驱动自动化注入能力，降低了配置门槛。



\## 功能特性



\- \*\*多模式烧录\*\*：支持 EDL（紧急下载）、Preloader（预加载器）两种主流烧录模式

\- \*\*自动驱动\*\*：集成 zadig.exe、UsbDk、devcon.exe，Windows 环境下自动注入 WinUSB 驱动

\- \*\*固件管理\*\*：支持 scatter.txt 分区表解析与自定义固件加载

\- \*\*安全调试\*\*：提供 EMI Debug 模块用于底层内存调试

\- \*\*跨平台潜力\*\*：核心代码采用 Rust 实现，具备良好的跨平台扩展能力



\## 环境要求



| 组件 | 最低版本 | 说明 |

|------|----------|------|

| Rust | 1.70+ | 编译环境 |

| Python | 3.8+ | mtkclient 依赖 |

| libusb | 1.0+ | USB 通信库 |

| Windows | 10/11 | 驱动工具运行平台 |



\## 安装部署



bash

```

\# 1. 克隆仓库

git clone https://gitee.com/WUNELEZI1/zyb-client.git

cd zyb-client



\# 2. 安装 Rust 依赖

cargo build --release



\# 3. 安装 Python 依赖（mtkclient）

pip install mtkclient



\# 4. 运行工具

cargo run --release

```



\## 目录结构



```

zyb-client/

├── src/                    # Rust 源代码

├── assets/

│   ├── driver/            # Windows 驱动工具

│   │   ├── zadig.exe     # 驱动注入工具

│   │   ├── UsbDk/        # USB Development Kit

│   │   └── devcon.exe    # 设备控制工具

│   └── firmware/         # 固件文件

│       ├── scatter.txt   # 分区表配置

│       └── emi\_debug.bin # EMI 调试固件

├── Cargo.toml            # Rust 项目配置

└── README.md             # 项目文档

```



\## 使用示例



\### 1. 进入 EDL 模式



bash

```

\# 通过特定按键组合或命令触发设备进入 EDL 模式

\# 工具会自动检测并连接设备

cargo run -- --mode edl

```



\### 2. 烧录固件



bash

```

\# 使用 scatter 文件烧录完整固件

cargo run -- --flash --scatter assets/firmware/scatter.txt

```



\### 3. 驱动配置（Windows）



bash

```

\# 自动安装 WinUSB 驱动

.\\assets\\driver\\zadig.exe

```



\## 注意事项



\- ⚠️ 刷机操作存在风险，请提前备份重要数据

\- ⚠️ 部分功能需要设备已解锁 Bootloader

\- ⚠️ Windows 驱动安装可能需要管理员权限

\- ⚠️ 请确保 USB 连接稳定，避免中途断连导致设备变砖



\## 技术栈



\- \*\*语言\*\*：Rust

\- \*\*依赖库\*\*：mtkclient（Python）、libusb

\- \*\*驱动工具\*\*：Zadig、UsbDk、devcon



\## 贡献指南



欢迎提交 Issue 和 Pull Request！



1\. Fork 本仓库

2\. 创建特性分支 (`git checkout -b feature/xxx`)

3\. 提交更改 (`git commit -m 'Add xxx'`)

4\. 推送分支 (`git push origin feature/xxx`)

5\. 创建 Pull Request



\## 许可证



本项目仅供学习和研究使用，请勿用于商业用途。使用本工具造成的任何损失，作者不承担责任。



\---



如果需要补充其他内容（如更详细的使用命令、配置说明等），请告诉我！

