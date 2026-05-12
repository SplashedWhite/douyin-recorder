<div align="center">

# 抖音直播录制工具

**一款简洁优雅的抖音直播录制桌面工具**

监控直播间 | 实时录制 | FLV 转 MP4 | 多画质选择

![Tauri 2](https://img.shields.io/badge/Tauri-2.x-blue?logo=tauri)
![Vue 3](https://img.shields.io/badge/Vue-3.x-brightgreen?logo=vuedotjs)
![Rust](https://img.shields.io/badge/Rust-2021-orange?logo=rust)
![License](https://img.shields.io/badge/License-MIT-yellow)

</div>

---

## 功能特性

- **直播间监控** - 添加抖音直播间链接，实时查看主播开播状态
- **一键录制** - 开播后点击录制，自动下载直播流
- **多画质支持** - 蓝光 / 高清 / 标清 / 流畅，自由选择
- **FLV 转 MP4** - 录制完成后一键转换，支持自动转换
- **代理支持** - 可配置 HTTP 代理，适应不同网络环境
- **本地存储** - 数据保存在本地 SQLite 数据库，隐私安全
- **轻量桌面应用** - 基于 Tauri，体积小、启动快、资源占用低

## 截图

<div align="center">

> 主界面：直播间列表 + 录制任务管理
>
> ![screenshot1](assets\screenshot1.png)
>
> ![screenshot2](assets\screenshot2.png)

</div>

---

## 下载安装（普通用户）

> 如果你不懂编程，直接看这里就够了！

### 第一步：下载

前往 [Releases](https://github.com/SplashedWhite/douyin-recorder/releases) 页面，下载最新版本的安装包：

| 系统 | 下载文件 |
|------|---------|
| **Windows** | `douyin-recorder_x.x.x_x64-setup.exe` |

### 第二步：安装

- **Windows**：双击 `.exe` 安装包，按提示完成安装

### 第三步：使用

1. 打开应用，在顶部输入框粘贴抖音直播间链接（格式：`https://live.douyin.com/房间号`）
2. 点击"添加"按钮，房间会出现在监控列表中
3. 等待主播开播（状态显示为绿色"直播中"），点击"录制"按钮
4. 录制完成后，在任务列表点击"转换为 MP4"即可

录制文件默认保存在 `DouyinRecordings` 文件夹（可在设置中修改）。

> **提示**：如果网络正常访问抖音，无需额外配置代理。如果添加直播间失败，请确认使用的是完整的 `https://live.douyin.com/房间号` 格式链接。

---

## 应用设置

点击右上角齿轮图标打开设置面板：

| 设置项 | 说明 | 默认值 |
|--------|------|--------|
| **代理地址** | HTTP 代理 (如 `http://127.0.0.1:7890`) | 空 |
| **Cookie** | 浏览器 Cookie，用于绕过反爬 | 空 |
| **画质偏好** | 蓝光 / 高清 / 标清 / 流畅 | 高清 |
| **录制目录** | 录制文件保存位置 | `~/DouyinRecordings` |
| **数据库路径** | SQLite 数据库文件位置 | `~/.douyin-recorder/` |
| **自动转 MP4** | 录制停止后自动转换格式 | 关闭 |
| **24 小时制** | 时间显示格式 | 开启 |

## 常见问题

**Q: 为什么添加直播间失败？**
A: 请确认使用的是 `https://live.douyin.com/房间号` 格式的完整链接，短链接暂不支持。

**Q: 录制的文件在哪里？**
A: 默认保存在用户目录下的 `DouyinRecordings` 文件夹，可在设置中修改。

**Q: 需要配置代理吗？**
A: 如果你的网络环境可以正常访问抖音，则不需要配置代理。

**Q: 支持哪些画质？**
A: 支持蓝光 (FULL_HD1)、高清 (HD1)、标清 (SD1)、流畅 (SD2)，会根据主播推流自动匹配。

---

## 许可证

[MIT](LICENSE)

本项目使用了 [FFmpeg](https://ffmpeg.org/) 作为录制引擎，详见 [第三方许可证声明](THIRD_PARTY_LICENSES.md)。

---

## 开发指南

> 以下内容面向开发者，普通用户无需关注。

### 技术栈

| 层级 | 技术 |
|------|------|
| **前端** | Vue 3 + TypeScript + Element Plus + Pinia |
| **后端** | Rust + Tauri 2 |
| **数据库** | SQLite (rusqlite) |
| **录制引擎** | FFmpeg (sidecar) |
| **构建工具** | Vite 6 + pnpm |

### 环境要求

- [Node.js](https://nodejs.org/) (>= 18)
- [pnpm](https://pnpm.io/)
- [Rust](https://www.rust-lang.org/tools/install) (rustup + cargo)
- [Tauri CLI](https://tauri.app/) (开发依赖，会自动安装)
- [FFmpeg](https://ffmpeg.org/) (录制引擎，需要单独下载)

### FFmpeg 配置

本项目使用 FFmpeg 作为录制引擎，以 sidecar 方式打包。由于 FFmpeg 二进制文件较大（约 200MB），不包含在 git 仓库中，需要手动下载并放置。

**下载 FFmpeg：**

1. 访问 FFmpeg 官方下载页面：https://ffmpeg.org/download.html
2. 选择适合你操作系统的版本：
   - **Windows**：下载 Windows 版本（推荐使用 [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) 的构建版本）
   - **macOS**：使用 Homebrew 安装 (`brew install ffmpeg`) 或下载静态构建版本
   - **Linux**：使用包管理器安装或下载静态构建版本

**放置 FFmpeg：**

将下载的 FFmpeg 可执行文件放置到以下目录：

```
src-tauri/binaries/
```

**文件命名规则：**

FFmpeg 二进制文件需要按照 Tauri sidecar 的命名规则放置：

| 操作系统 | 目标平台 | 文件名 |
|---------|---------|--------|
| Windows | x86_64 | `ffmpeg-x86_64-pc-windows-msvc.exe` |
| Windows | x86 (32位) | `ffmpeg-i686-pc-windows-msvc.exe` |
| macOS | Intel | `ffmpeg-x86_64-apple-darwin` |
| macOS | Apple Silicon | `ffmpeg-aarch64-apple-darwin` |
| Linux | x86_64 | `ffmpeg-x86_64-unknown-linux-gnu` |
| Linux | ARM64 | `ffmpeg-aarch64-unknown-linux-gnu` |

**示例（Windows）：**

```bash
# 创建 binaries 目录（如果不存在）
mkdir -p src-tauri/binaries

# 将下载的 ffmpeg.exe 重命名并移动到正确位置
mv ffmpeg.exe src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe
```

**验证配置：**

```bash
# 测试 FFmpeg 是否可用
./src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe -version
```

> **注意**：如果你只需要在当前平台开发和构建，只需下载对应平台的 FFmpeg 版本即可。如果需要跨平台构建，需要下载所有目标平台的版本。

### 安装与运行

```bash
# 1. 克隆项目
git clone https://github.com/SplashedWhite/douyin-recorder.git
cd douyin-recorder

# 2. 安装依赖
pnpm install

# 3. 启动开发模式
pnpm tauri dev
```

### 构建发布版本

```bash
pnpm tauri build
```

构建完成后，安装包将输出到 `src-tauri/target/release/bundle/` 目录。

### 项目结构

```
douyin-recorder/
├── src/                        # 前端源码
│   ├── components/             # Vue 组件
│   │   ├── RoomList.vue        #   直播间列表
│   │   ├── TaskList.vue        #   录制任务列表
│   │   └── Settings.vue        #   设置面板
│   ├── stores/                 # Pinia 状态管理
│   └── types/                  # TypeScript 类型定义
├── src-tauri/                  # 后端源码
│   ├── src/
│   │   ├── lib.rs              #   核心逻辑 & Tauri 命令
│   │   ├── parser.rs           #   抖音 API 解析
│   │   ├── recorder.rs         #   FFmpeg 录制管理
│   │   ├── database.rs         #   SQLite 数据库层
│   │   └── settings.rs         #   配置管理
│   ├── binaries/               # FFmpeg sidecar 二进制（需手动下载）
│   └── tauri.conf.json         # Tauri 配置
├── package.json
└── vite.config.ts
```
