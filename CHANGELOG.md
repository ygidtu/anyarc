# 变更日志

本项目遵循 Keep a Changelog 格式与语义化版本。

## [未发布]

### 新增
- M3-L5 音频容器只读解码:FLAC(claxon)、Monkey's Audio 3.95+(ape-decoder)、
  Shorten v2/v3(自研 clean-room 解码器);输出统一为 `{stem}.wav`,
  与 GNU shorten/flac 产物字节级对拍一致
- M4-b 进度条:extract/test/create/repack 接入 indicatif(TTY 自动启用,
  `-q`/非 TTY 关闭;stderr 输出,stdout/`--json` 不受影响)
- M4-b+ pv 风格单文件进度条:单流解压后端新增流式接口 `read_current_to`,
  自写容器(gz/lzma/lzip/lz4)改为读时惰性流式解压(开档不再全量驻留内存);
  单文件作业显示 进度条/百分比/已解压字节/速率/ETA(总长按 gzip 尾 ISIZE、
  LZMA 头尺寸域、lz4 帧头估算;未知或超出估算时自动降级为无总长模式);
  多条目落盘进度改为人读字节+速率,unarc 后端经默认分块写平滑推进
- M4-c 递归解压 `extract --recursive/-r`:解出的文件若仍是可识别归档则就地
  继续向内解压、移除中间归档,一次性直达最内层叶子(如 zip→tar.gz→tar→leaf);
  按内容探测停叶子,深度上限 16 层 + 自引用/自嵌条目跳过防护(流式读会截断自身源)
- M4 发布工程:
  - `anyarc completions <bash|zsh|fish|elvish|powershell>` 生成 shell 补全脚本
  - `anyarc man` 输出 roff man page(`anyarc man | man -l -`)
  - release profile 静态化调优(LTO/strip/panic=abort);
    x86_64-unknown-linux-musl 全静态二进制 6.3 MB,`ldd` 无依赖
  - cargo-dist 配置与多平台发布矩阵(见 `dist.toml` / `.github/workflows/release.yml`)
- `poc/e2e.sh`:133 fixture CLI 端到端冒烟固化脚本

### 修复
- 多成员 gzip 解压(unarc-rs 0.8.1 缺陷)改走自写 MultiGzDecoder(flate2)
- cpio newc 硬链接条目两阶段 (dev,ino) 归并

## [0.1.0] - 2026-10-31

首个功能完整版本(M1–M3):

- 读:zip/7z/rar/tar 家族/ar/deb/rpm/cpio/iso/lzma/lzip/lz4/gz/bz2/xz/zst/Z
  与 BBS 旧格式(arc/arj/lha/zoo/cab/ace/adf/dms/hyp/ice/uc2/sq/lynx 等,经 unarc-rs),
  分卷自动聚合,前置数据(SFX/polyglot)zip 自动定位
- 写(create/repack):tar 家族、zip(deflate/WinZip AES-256)、7z(LZMA2)、
  单流 gz/bz2/xz/zst/lzma/lz/lz4;RAR 创建为永久禁区(无合规实现)
- 命令:extract / test / list / create / repack / diff / search / formats;
  密码 `-p`/`ANYARC_PASSWORD`/tty 隐藏输入;`--encoding` GBK 等名字修复
- 安全落盘:路径逃逸拒绝、覆盖保护(默认告警跳过)、解压限额
  (`--max-entry-size` 4 GiB / `--max-total-size` 32 GiB)
- Backlog(识别 + 清晰报错):alz、freearc、zpaq、shar、wav、vhd、wim、udf、chm、bzip3
