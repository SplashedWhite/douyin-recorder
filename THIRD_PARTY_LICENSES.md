# 第三方许可证声明

本项目使用了以下第三方组件，特此声明：

## FFmpeg

- **组件**：FFmpeg
- **用途**：直播流录制和视频格式转换
- **许可证**：LGPL 2.1+ 或 GPL 2.0+（取决于编译配置）
- **源代码获取**：https://ffmpeg.org/download.html
- **使用方式**：作为独立可执行文件（sidecar）打包，通过进程间调用使用

本项目以 sidecar 方式打包 FFmpeg 可执行文件，通过命令行调用实现录制功能。FFmpeg 作为独立进程运行，与本项目代码无直接链接关系。

### FFmpeg 许可证信息

FFmpeg 的许可证取决于其编译配置：

- **默认配置**：LGPL 2.1+
- **启用 GPL 编解码器**（如 x264、x265 等）：GPL 2.0+

如需了解 FFmpeg 的具体编译配置和许可证详情，请访问：
- https://ffmpeg.org/legal.html
- https://ffmpeg.org/doxygen/trunk/md_LICENSE.html

### 源代码提供

根据 GPL/LGPL 许可证要求，FFmpeg 源代码可从以下地址获取：
- 官方网站：https://ffmpeg.org/download.html
- Git 仓库：https://git.ffmpeg.org/ffmpeg.git

---

本项目（douyin-recorder）本身采用 MIT 许可证，详见 [LICENSE](LICENSE) 文件。
