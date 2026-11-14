# anyarc

[![CI](https://github.com/ygidtu/anyarc/actions/workflows/ci.yml/badge.svg)](https://github.com/ygidtu/anyarc/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/ygidtu/anyarc?include_prereleases)](https://github.com/ygidtu/anyarc/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0)
[![Rust](https://img.shields.io/badge/rust-1.95%2B-orange)](https://www.rust-lang.org/)

**一个静态二进制搞定归档:patool 的 Rust 重写,运行时零外部依赖**
(不需要系统里装 `unrar` / `7z` / `unar` / `patool`)。

<details>
<summary>目录</summary>

- [特性](#特性)
- [安装](#安装)
- [快速上手](#快速上手)
- [命令一览](#命令一览)
- [格式支持](#格式支持)
- [开发:测试门禁](#开发测试门禁)
- [相关文档](#相关文档)

</details>

## 特性

| | |
|---|---|
| 🧵 **流式解压 + pv 风格进度** | `解压 ████ 92% 124.9 MiB/135.1 MiB 19.2 MiB/s ETA 1s [6s]`,大文件内存 O(窗口) |
| 🪆 **递归解压** `-r` | `zip → tar.gz → tar → 文件` 一次直达最内层,中间压缩包自动清除 |
| 🧹 **智能整理** `--smart` | 合并重复的同名包裹目录(`novel/novel/` → `novel/`) |
| 📦 **分卷支持** | RAR 分卷按卷头卷号/partN 序号智能串联(文件名改坏也能解);Info-ZIP 分卷 zip(`.z01`+`.zip`)虚拟串联;7z/zip 官方分卷 |
| 🔑 **权限还原** | tar/zip/rar/cpio 内的 unix mode(可执行位等)如实还原 |
| 🔐 **密码** | `-p` / `ANYARC_PASSWORD` / 交互式隐藏输入;WinZip AES、RAR5 |
| 🛡️ **安全落盘** | Zip-slip 路径逃逸拒绝、默认不覆盖、大小限额、失败不留半成品 |
| 🖥️ **跨平台** | Linux(gnu/musl 全静态)、macOS(Intel+ARM universal)、Windows,单文件即用 |

## 安装

从 [Releases](https://github.com/ygidtu/anyarc/releases) 下载对应平台的压缩包,解压后
单文件放入 `PATH`。macOS 首次运行若被 Gatekeeper 拦截:

```bash
xattr -d com.apple.quarantine ./anyarc
```

自行构建(需要 Rust 1.95+):

```bash
cargo build --release                                    # 本机
cargo build --release --target x86_64-unknown-linux-musl # 全静态 ~6 MB,ldd 无依赖
```

## 快速上手

```bash
# 递归解开"压缩包套压缩包",并合并重复目录
anyarc extract --recursive --smart album.zip -o out/

# 分卷 RAR:只给第一卷(part2 会被自动找到,文件名被改过也没关系)
anyarc extract game.part1.rar

# 分卷 zip:给末卷或任一 .zNN 均可
anyarc extract comic.zip

# 带密码解压
anyarc extract -p "密码" archive.7z
```

## 命令一览

| 命令 | 说明 |
|---|---|
| `anyarc extract [-r] [--smart] [-o DIR] [-p PW] [--overwrite] [--encoding gbk] ARCHIVE…` | 解压;`-r` 递归,`--smart` 整理目录结构 |
| `anyarc test ARCHIVE…` | 只解码校验 CRC,不写文件 |
| `anyarc list [-l \| --json] ARCHIVE…` | 列出内容 |
| `anyarc create OUT FILES…` | 创建(白名单:tar 家族 / zip / 7z / 单流压缩;**RAR 创建为永久禁区**) |
| `anyarc repack IN OUT` | 任意支持格式 → create 白名单格式 |
| `anyarc diff A B` | 条目名 + sha256 对比(退出码 0=一致 / 1=差异) |
| `anyarc search PATTERN ARCHIVE…` | glob 或子串匹配文件名(退出码 0=命中) |
| `anyarc formats` | 完整能力表 |
| `anyarc completions SHELL` | bash / zsh / fish / elvish / powershell 补全脚本 |
| `anyarc man` | roff 手册:`anyarc man \| man -l -` |

常用旗标:`-q` 静默;`--max-entry-size`(默认 4 GiB,限制单文件整体读入内存的路径)、
`--max-total-size`(默认 32 GiB,解压总量磁盘预算);`0` = 不限。
进度:TTY 自动显示(stderr),非 TTY 或 `-q` 关闭;stdout 与 `--json` 永不受影响。
退出码:`0` 成功 / `1` 运行时错误 / `2` 用法错误。

## 格式支持

**读取/解压:**

| 类别 | 格式 |
|---|---|
| 现代归档 | zip(jar/epub/apk/cbz、SFX、分卷 `.zNN`、WinZip AES/ZipCrypto)、7z(分卷/头加密)、rar4/5(分卷智能串联)、tar 家族(.gz/.bz2/.xz/.zst/.Z/.lz/.lzma/.lz4) |
| 单流压缩 | gz(多成员) / bz2 / xz / zst / Z / lzma-alone / lzip / lz4 |
| 系统/容器 | ar(.a) / deb / rpm / cpio(newc/odc/bin) / ISO 9660(含 Joliet) |
| BBS 时代 | arc / arj / lha / zoo / cab / ace / adf / dms / hyp / ice / uc2 / sq / lynx 等 |
| 音频(只读解码为 WAV) | flac / monkey's audio(3.95+) / shorten(v2/v3) |

**创建:** 见 `create` 白名单(RAR 创建为永久禁区,算法闭源无合法实现)。

**识别 + 明确报错(Backlog):** alz、freearc、zpaq、shar、vhd、wim、udf、chm、bzip3。

完整逐项矩阵:`anyarc formats`,对拍验收:`docs/POC-REPORT.md`。

## 开发:测试门禁

> **约定:任何修改,所有测试必须全绿才能 push。**

```bash
cargo test --release     # 单元 42 + 需求验收集成套件 tests/regression.rs(17 用例)
```

- **CI**:[`.github/workflows/ci.yml`](.github/workflows/ci.yml) 在 Linux / Windows /
  macOS 三平台跑同一命令,push/PR 必须绿。
- **本地钩子**:克隆仓库后执行一次
  `git config core.hooksPath .githooks`,
  此后 `git push` 前自动跑 `cargo test --release`,不通过直接拒绝推送。
- 更深验收(需要系统参照工具,非门禁必须):`./target/release/matrix`(133 fixture
  逐字节对拍)、`./poc/e2e.sh`(CLI 冒烟)、`python3 poc/fuzz_mutate.py 200`(健壮性)。

`tests/regression.rs` 顶部维护着 **需求 ↔ 用例对照表**,新增需求时请按同一模式补充用例。

## 相关文档

- [PLAN](docs/PLAN.md) — 规划与验收标准
- [POC 报告](docs/POC-REPORT.md) — patool 全 fixture 逐字节兼容性矩阵
- [CHANGELOG](CHANGELOG.md) — 变更日志

## License

MIT OR Apache-2.0
