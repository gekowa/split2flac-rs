# split2flac-rs 跨平台实现计划

> 执行记录：五项工作已完成。基准重建及三平台原生验收见[私有 CI 45137575219](https://github.com/gekowa/ultimate-flac-native-acceptance-20261123/actions/runs/45137575219)。用户原始 CUE 与私有参考清单仅供本地测试。

**Goal:** Windows x64 和 macOS（Apple Silicon、Intel）都能离线处理完整专辑，CUE 输出逐轨匹配 split2flac v122。

**Architecture:** Windows 与 macOS 共享 Rust CUE 解析、切点计算和 PCM 分发；FFmpeg/ffprobe 统一编解码。Linux 上运行原版生成一份独立清单，各发布目标只读这份清单，无平台专属“正确答案”。

**Tech Stack:** Rust stable / Cargo.lock、FFmpeg CLI、Python 3 标准库（测试和打包）、固定 digest 的 Docker 参考环境、Windows 与 macOS 原生 CI。

**Spec:** [split2flac-rs 跨平台移植设计](../specs/2026-11-23-split2flac-rs-design.md)

## Global Constraints

- 原版 commit `491591d21e2f97419e74751864b76b234b0fa2f1`；原版和 Rust 都以同一整轨镜像为输入。五入六出；不删除切轨前音频、不合成不在源 PCM 中的静音，除非该特定参考用例表明上游如此处理。
- 采用有界内存和整数运算，不用 FFmpeg 时间戳 seek，不把有损 PCM 作为无损基准。除旧编码器私有参数的兼容边界，功能见设计，不把默认格式当作唯一验收路径。
- Windows 10/11 x64、macOS arm64 和 x86_64 原生、可离线使用、Unicode 路径、碰撞不覆盖；`-D` 仅在专辑完整成功后生效。仓库中不放用户专辑音频，不上传用户样本。

## Review Focus

- 同时出现显式 PREGAP/POSTGAP 和 INDEX 00 时，不把声明时长再加到真实样本上。
- 首轨非零时多出的 00 轨不能漏；非整 CD 帧结尾的尾音不能被截掉。
- 实际采样率、PCM 帧宽不等于 CD 时，不按每秒 75 帧的直觉重新解释原版字节位置。
- Windows 保留名、macOS 卷上的等价 Unicode 名称、大小写与模板碰撞按目标卷处理；保住原图像及已有文件。
- 损坏的解码输入或中途退出的编码器，不得把短暂的 EOF 当成成功，也不得执行删源。

---

### 1. 建立独立原版基准和真实专辑用例

**Files:** `tests/oracle/generate.py`（固定容器、输入生成及清单生成）、`tests/oracle/cases.json`（输入定义）、`tests/oracle/expected.json`（只由原版写入）、`tests/compare.py`（所有平台共用的只读比较器）。用户资料来源 `samples/Ultimate Tchaikovky CD2/{album.cue,result.md}`。

- [x] 固定 38 组原版观察，其中 12 组区分无间隙、第一/中间/末轨的声明间隙、相邻 PRE+POST、录入的 INDEX 00、录入+声明、INDEX 02、首轨非零和单轨特例。
- [x] 记录错误、非 CD、查找顺序和内嵌 CUE 等案例的输入、退出码、输出路径、音频属性和逐轨 PCM SHA-256。比较器的负向检查拒绝移动切点、漏掉 `00.wav`、删源和多余产物等变化。
- [x] 私有 CUE 与合成整轨音频：原版、Rust 的七首和音频开头的 `00.wav` 逐轨匹配。用时长表独立复核七首；没有原录音，不声称验证了原盘 EOF。
- [x] Linux 原版容器重建清单通过；原生三平台只读同一清单，未上传用户样本。

### 2. CUE 解析、发现和兼容分界

**Files:** `src/cue.rs`, `src/source.rs`, `src/layout.rs`, `tests/cue_layout.rs`。`cue::parse(text: &str) -> Result<Sheet>` 提供专辑/轨字段与索引；`layout::plan(sheet: &Sheet, frame_bytes: u32, total_frames: Option<u64>) -> Result<Vec<Segment>>` 提供各物理段的编号和解码后字节偏移。原版对某些非常规声道/位深组合确实会在采样帧中间切 WAV，不能为了对齐擅自移动切点。`source::load` 处理显式、同名和内嵌 CUE 及字符集。

- [x] Rust 实现 CUE 发现、轨字段及整数 PCM 字节分界。对 48 kHz 立体声/16 位、96 kHz 立体声/24 位、44.1 kHz 单声道/16 位分别保留参考切点 44,688、29,792、89,376 采样帧。
- [x] 将轨数预检查、单轨特例、越界及最后一段为空与原版分开对照。异常索引不排序；非整采样帧 WAV 保留原负载字节。
- [x] `tests/cue_layout.rs` 与完整 CLI 清单覆盖来源优先级、字符集、轨表及上述边界。

### 3. 编解码及无损取样

**Files:** `src/media.rs`, `src/split.rs`, `tests/media.rs`。`PcmFormat` 记录采样率、存储帧宽、有效位深和声道；`split::run` 接受打开的解码流、`Layout`、每轨的编码目标。`src/media.rs` 负责探测、解码与编码子进程的退出状态。

- [x] `tests/media.rs` 对照五种真实输入、无损采样精度及有损轨的独立 PCM 切片编码结果。
- [x] 源图像顺序解码一次，按字节分发。检查解码器/编码器退出状态；损坏输入与编码失败不发布未完成轨。
- [x] 使用包内 FFmpeg 运行媒体回归，并在原生矩阵运行 5 × 6 种真实输入输出组合。

### 4. CLI、专辑文件与元数据

**Files:** `src/cli.rs`, `src/album.rs`, `src/naming.rs`, `src/gain/`, `src/split.rs`, `tests/album_cli.rs`, `tests/cli_config.rs`, `tests/gain.rs`。

- [x] 完成原 CLI、配置、命名模板、批处理、格式别名、补全、封面及附属文件处理；说明 `-e` 和 `-enca` 的兼容边界。
- [x] 回归覆盖预览不写、失败不删、成功后删除、冲突及递归失败清单。
- [x] 按格式实现 ReplayGain，用不同响度的两轨和单轨验证标签、分组及解码音频保持不变。
- [x] 三个平台运行完整 CLI、音频和增益回归。

### 5. 原生发布包与跨平台验收

**Files:** `packaging/package.py`（按目标组包）、`packaging/tools.lock.json`（按目标锁定下载）、`.github/workflows/release-test.yml`（原生运行矩阵）、`README.md`。

- [x] 锁定编解码源码、SHA-256 和许可证；核验 Windows PE 导入和 macOS 架构与 dylib。
- [x] 分别生成 Windows x64 ZIP、macOS 两架构 tar.gz，包括对应源码、所需工具、命令别名和使用说明。
- [x] 在 Windows Server 2022 x64、macOS 15 Apple Silicon 和 macOS 15 Intel 的原生 CI 解包，从 Unicode 路径、无系统编解码工具的搜索路径执行 38 个 CUE 基准及 30 个格式组合。CI 的 Windows 构建使用 MSVC；本地交付的 Windows ZIP 为独立的 gnullvm 构建，不声称二者归档字节相同。

私有验收仓库仅同步项目代码和匿名测试数据。设计、计划以及包含用户资料的测试输入和观察记录均保留在本地。
