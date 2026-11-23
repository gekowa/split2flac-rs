# split2flac (Rust)

按 CUE 把一张整轨镜像切成单曲。切轨行为以 [split2flac v122](https://github.com/ftrvxmtrx/split2flac/tree/491591d21e2f97419e74751864b76b234b0fa2f1) 的实际输出为准；编解码由 FFmpeg 完成。目标平台为 Windows x64、macOS Apple Silicon 和 macOS Intel。不是原 Shell 脚本的包装器。

## 使用

解压对应平台的发布包，保留程序旁边的 `tools/` 目录。先预览轨名，再实际切分：

```powershell
.\split2flac.exe -p 'D:\古典音乐\CD 2\src.flac'
.\split2flac.exe -o 'D:\单曲' 'D:\古典音乐\CD 2\src.flac'
```

```sh
./split2flac -p '/Volumes/Music/古典音乐/CD 2/src.flac'
./split2flac -cue 'album.cue' -o '单曲' 'src.flac'
./split2flac -f mp3 -e '-b:a 192k' -o '导出' '专辑目录'
```

- **输入**：APE、FLAC、TTA、WavPack、WAV；目录输入会递归查找。`-cue` 优先于 `src.flac.cue`、`src.cue` 和内嵌 CUE。非 UTF-8 文本可用 `-cuecharset GBK` 等显式指定。
- **字符集**：`-enca '-L zh'` 或 `-enca '--language=ru'` 给 Rust 探测器指定语言提示，并不调用原版 `enca`。不支持的探测参数会报错；知道编码时优先用 `-cuecharset`，批处理用 `-nask` 关闭交互。
- **输出**：FLAC（默认）、M4A/AAC、MP3、Ogg Vorbis、Opus、WAV。`-of '@artist/{@year - }@album/@track - @title.@ext'` 设置命名；`-nd` 不创建模板子目录。同名输出不覆盖。
- **编解码参数**：`-e` 使用对应 FFmpeg 编码器的参数；`-eh` 查看帮助。FLAC `-8` 对应 `-compression_level 8`，LAME `--preset standard` 与 Vorbis `-q 5` 分别对应 `-q:a 2` 与 `-q:a 5`。FAAC 和 FFmpeg AAC 的质量刻度不同，不保证码流或码率逐字节一致。
- **专辑处理**：`-c` / `-cs` 设置封面及尺寸；`-nc` 禁用封面。`-C` / `-nC` 控制附属文件复制。`-g` 启用对应格式的 ReplayGain 分析，不是 PCM 音量归一化。`-s` 保存配置。`-h` 显示全部选项。
- **原件**：默认不删除。`-D` 在专辑成功后询问；`-D -F` 不询问。`-p` 不写曲目或删除原件。目录中部分专辑失败时，程序继续其余专辑，在工作目录写 `split_failed.txt` 并返回 3。

## 什么叫“切轨一致”

不是把 `mm:ss:ff` 转成小数秒去 seek。原版将 CUE 文本交给 shnsplit；在标准 CD 音频上，`INDEX 01` 指定分界，两个分界之间的实录间隙保留在 PCM 中。首轨非零时可能多出一条 `00` 轨，不能把它并入第一首。显式 `PREGAP` 或 `POSTGAP` 不等于向音频人工补一段静音。

对非 CD 采样率或不同采样帧宽，保留原版字节边界，即使它与按采样率换算的直觉不同。比如 48 kHz、16 位立体声下，`00:01:01` 在原版切于第 44,688 个采样帧，而非 48,640。无损结果按音频负载比较，不比较 WAV 头、标签顺序或 FLAC 压缩字节。

## 从源码运行

```sh
cargo build --release --locked
cargo run --release --locked -- -h
```

开发运行可在 `PATH` 中提供 `ffmpeg`、`ffprobe`，或通过 `SPLIT2FLAC_TOOLS` 指定工具目录。FFmpeg 需支持目标格式，如 `libmp3lame`、`libvorbis` 和 `libopus`；不能仅凭有 `ffmpeg` 命令假定编码器齐全。发布包使用随包的原生工具。

## 验证

```sh
# 仅在需要重新生成原版基准时获取参考镜像
docker pull --platform linux/amd64 mikenye/split2flac@sha256:47e6cb8d835a05f08efae20f7bf196552754df09e9dc957cc473b870f4dbc8ba
cargo test --locked
python3 tests/oracle/generate.py --check
python3 tests/compare.py --bin target/release/split2flac
```

仅重建基准需要 Docker。`tests/oracle/expected.json` 记录原版退出状态、输出文件名、音频属性及逐轨 PCM SHA-256；只读比较不需要安装 shntool、cuetools 或运行原脚本。原生验收入口为 `packaging/check_release.py`。

## 构建离线包

`packaging/tools.lock.json` 固定了 FFmpeg、LAME、Vorbis、Ogg、Opus 和 zlib 的源码版本与校验和；两个辅助程序从这些源码构建。ReplayGain 分析与分格式写标签在 Rust 程序内执行，不需要另装 `mp3gain`、`aacgain` 或 `metaflac`。

以 Apple Silicon 为例（打包需要 Python 3.12+、C 工具链、make 和 pkg-config；使用发布包不需要）：

```sh
T=aarch64-apple-darwin
rustup component add rust-docs
cargo fetch --locked --target "$T"
python3 packaging/package.py --target "$T" --fetch-only
python3 packaging/package.py --target "$T" --offline
python3 packaging/check_release.py "dist/split2flac-$T.tar.gz" --target "$T"
```

压缩包及 `.sha256` 校验文件在 `dist/`。Windows ZIP 和两个 macOS tar.gz 分开分发；请勿只复制主程序。普通 CLI 使用不需要网络、Python 或管理员权限。
