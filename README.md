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
cargo test --locked
python3 tests/oracle/generate.py --check
python3 tests/compare.py --bin target/release/split2flac
```

仅重建基准需要 Docker；比较已冻结的原版结果不需要安装 shntool、cuetools 或运行原脚本。原版脚本及参考容器摘要见[设计](docs/superpowers/specs/2026-11-23-split2flac-design.md)，实现与原生发布验收见[计划](docs/superpowers/plans/2026-11-23-split2flac.md)。
