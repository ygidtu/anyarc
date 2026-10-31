# anyarc

单个静态二进制的归档文件管理工具,patool 的 Rust 重写 —— 运行时零外部依赖
(无需 unrar/7z/unar/patool 等系统工具)。许可证 MIT OR Apache-2.0。

- 规划与验收:`docs/PLAN.md`
- 可行性 PoC 报告:`docs/POC-REPORT.md`(对 patool 全部 fixture 的逐字节兼容性矩阵)
- 变更日志:`CHANGELOG.md`

## 构建

```bash
cargo build --release                              # 本机
cargo build --release --target x86_64-unknown-linux-musl   # 全静态(~6 MB,ldd 无依赖)
```

## 命令

```bash
anyarc extract [-o DIR] [-p PW] [--overwrite] [--encoding gbk] <arch>...
anyarc test <arch>...                # 只解码校验,不落盘
anyarc list [-l|--json] <arch>...
anyarc create <out> <files>...       # 白名单:tar 家族/zip/7z/单流压缩
anyarc repack <in> <out>             # 读任意支持格式,写 create 白名单
anyarc diff <a> <b>                  # 条目名+sha256 对比(exit 0=一致/1=差异)
anyarc search <pattern> <arch>...    # glob 或子串(grep 语义,exit 0=命中)
anyarc formats                       # 格式支持能力表
anyarc completions bash|zsh|fish|elvish|powershell
anyarc man                           # roff man page:anyarc man | man -l -
```

- 密码:`-p` / `ANYARC_PASSWORD` / tty 隐藏输入
- 安全落盘:路径逃逸拒绝、默认不覆盖、`--max-entry-size`(默认 4 GiB)/
  `--max-total-size`(默认 32 GiB)
- 退出码:0 成功 / 1 运行时错误 / 2 用法错误

## 支持格式(摘要)

读:zip(jar/epub/apk/cbz、SFX 前置数据、WinZip AES/ZipCrypto)、7z(含分卷/头加密)、
rar4/5、tar 家族(.gz/.bz2/.xz/.zst/.Z/.lz/.lzma/.lz4)、单流 gz/bz2/xz/zst/Z/lzma/lzip/lz4、
ar/deb/rpm/cpio/iso,以及 arc/arj/lha/zoo/cab/ace/adf/dms/hyp/ice/uc2/sq/lynx 等 BBS 格式;
音频只读解码为 WAV:flac、ape(3.95+)、shn(v2/v3)。

写:见 `create` 白名单(RAR 创建为永久禁区)。

Backlog(识别 + 清晰报错,不实现):alz、freearc、zpaq、shar、wav、vhd、wim、udf、chm、bzip3。

## 测试

```bash
cargo test --release          # 单元/对拍测试
./target/release/matrix       # 130 fixture 逐字节验收矩阵(vs poc/ref/*.manifest)
./poc/e2e.sh                  # CLI 端到端冒烟
python3 poc/fuzz_mutate.py 200; python3 poc/fuzz_headers.py   # 健壮性(标准:CRASH=0)
```
