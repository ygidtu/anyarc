# anyarc 项目规划

> 状态:规划定稿,等待实施指令
> 前置验证:PoC 已完成,见 `docs/POC-REPORT.md`(2026-10-30)
> 命名:推荐 **anyarc**(crates.io 空闲、发行版无撞名)。曾用名 unp-rs 已废弃(见 §1)。备选:`unpx`、`uarc`(均空闲但较弱,理由见 §1)

## 1. 立项背景与定位

- **目标**:用 Rust 重写 patool 的功能,发布为**单个静态二进制**,运行时零系统依赖(用户无需安装 unrar/7z/p7zip/unar 等任何外部程序),覆盖 patool 支持的绝大多数归档格式。
- **命名**:`anyarc`("any archive";二进制名与仓库名一致)。不用 `unp`(与 Debian 旧包/Perl 脚本撞名,即 patool 前身)、不用 `unarc`(上游 unarc-rs 作者已发布 `unarc-cli`,二进制就叫 `unarc`,且与 Unarrange C 库撞名)、不用 `upack`(crates.io 已被 SIMD bitpacking 库占用,2026-02)。
- **与上游 unarc-cli 的差异化**:unarc-rs 作者自己也做了一个只读 CLI(`unarc extract/list`,偏 BBS 复古格式)。anyarc 的差异化 = patool 完整语义(`create`/`repack`/`diff`/`search` + 白名单写路径)、更广格式层(ar/deb/rpm/cpio/lzip/iso 等自写或接第二实现)、安全落盘策略与限额、迁移 UX(formats 能力表、退出码对齐)。上游库出问题时 anyarc 内可 fork 关键解码器(许可证允许)。
- **非目标**:
  - 不追求 patool 100% 格式对齐(长尾 8 种格式分级处置,见 §5);
  - 不做 RAR **创建**(算法专有,patool 自身也依赖商业 `rar` 二进制,明确放弃);
  - 不做跨格式转码类 repack 的音频部分(ape/flac/shn 互转移出 v1)。

## 2. 硬约束与已定决策

| 决策点 | 结论 |
|---|---|
| 语言 | Rust(生态碾压 Go,见 PoC 报告) |
| 核心依赖 | `unarc-rs`(MIT/Apache-2.0)+ `sevenz-rust2` + `zip`/`tar`/`flate2`/`bzip2`/`xz`/`zstd` 等纯 Rust crate |
| 许可证 | **MIT OR Apache-2.0**;红线:**永不 vendor GPL 的 C 源码**(unalz/xdms 等)。需要 C 时只 vendor BSD/LGPL/公有领域实现,或按公开 spec 用 Rust 重写 |
| 静态发布 | Linux 用 `*-unknown-linux-musl`(全静态);Windows 静态 CRT;macOS 双架构 |
| 格式探测 | 内置 magic-bytes 优先(`unarc_rs::ArchiveFormat::detect`),扩展名兜底与消歧,**不依赖 `file(1)`** |
| 写操作 | `create`/`repack` 走白名单(§6),白名单外格式一律清晰报错 |
| 外部 helper | **彻底移除**该机制(不做 fallback shelling out;PoC 证实无必要) |
| patool 兼容层 | **不做**(2026-10-30 拍板)。仅提供 MIGRATION.md 参数对照表 + 一次性 `anyarc migrate-patool-config` 读旧配置 |
| 长尾格式 | L4/L6 全部转入 **Backlog 再议**(用户判定:需求弱,暂不拍板放弃也不排期);v1 行为 = 识别格式 + 明确报错指引 |
| crates.io 占位 | **不需要**(自用项目,2026-10-30 拍板) |

## 3. PoC 已验证的事实(规划依据)

1. 69 个 patool fixture:35 逐字节 MATCH、5 无参照自检通过(ACE/ADF/DMS/加密ARJ)、真实归档容器通过率 ~85%。
2. RAR(含密码)、7z(含 AES 头加密/分卷/cb7)、全部 tar 家族、arc/arj/zoo/lha/cab、单流 .gz/.bz2/.xz/.zst/.Z 均可纯 Rust 完成。
3. 需要修的前端问题(PoC 实测):
   - **复合流探测**:无扩展名提示的 `*.tar.gz`/`t.taz` 需"解压外层→peek 512B tar header"二跳探测(unarc-rs 已内置 `tar_header_checksum_matches`,在其上封装即可;同时向 unarc-rs 提上游 PR);
   - **FreeARC magic 消歧**:防止 `*.arc` 误入 ARC 解码器;
   - **`next_entry` 必须显式 `skip()`** 目录项,Backend 封装层吸收此细节。
4. 加密 ARJ 案例:参照工具(7z/unar)失败而 unarc-rs 正确 → 对冷门路径质量有信心,但 **ACE/ADF/DMS 需要 fuzz + 真实样本补充验证**。

## 4. 架构设计

```
anyarc/
├── src/
│   ├── main.rs          # 仅入口,解析参数 → commands
│   ├── cli.rs           # clap 定义,与 patool 参数对齐
│   ├── detect.rs        # magic+扩展名两级探测、复合流二跳
│   ├── backend/
│   │   ├── mod.rs       # trait Backend { list / test / extract / create? }
│   │   ├── unarc.rs     # unarc-rs 封装(rar/7z/zip/ace/arj/zoo/arc/lha/cab/tar家族/…)
│   │   ├── sevenz.rs    # sevenz-rust2 封装(create/复验)
│   │   ├── containers.rs# 自写:ar / deb / rpm / cpio(纯 Rust,各<500行)
│   │   └── single.rs    # 单流压缩封装(gz/bz2/xz/zst/Z/lzma/…)
│   ├── extract.rs       # 落盘策略:Zip-slip 防护、符号链接、覆盖确认、大小/条目限额
│   ├── repack.rs        # 临时目录解压→按白名单重打包
│   ├── diff.rs          # 两个归档的文件名+哈希比对(v1),文本 unified diff(v2)
│   └── format_table.rs  # 格式注册表:magic、扩展名、能力位(read/write/encrypt/volume/solid)
├── tests/               # golden fixture 测试,复用 patool tests/data + poc/ref/*.manifest
└── poc/                 # 保留 PoC harness 与参照脚本(验收工具链)
```

### 4.1 Backend trait(核心抽象)

```rust
pub trait Backend {
    fn open(path: &Path, opts: &OpenOptions) -> Result<Archive>; // opts: 密码、限额
    // Archive: 顺序迭代条目(solid 格式要求顺序读,禁止并行)
}
pub struct Capabilities { read: bool, write: bool, password: bool, multi_volume: bool, solid: bool }
```

要点:
- 统一条目模型:`{ name, kind, size, mtime?, crc_ok }`,路径分隔符在 Backend 层归一化为 `/`;
- solid 归档(7z solid block、ACE solid)必须整档顺序解码,`skip_with_options` 保留字典;
- 限额默认:单条目 4 GiB、整档 32 GiB、条目数 1M,`--max-*` 可调(DoS 防护,unarc-rs 已有 limits API,统一透传)。

### 4.2 安全策略(必须进 v1)

- Zip-slip:`..`/绝对路径条目默认拒绝 + `--safe-extract` 为默认行为;
- 符号/硬链接:tar 内链接条目不逃逸目标目录;
- 解压炸弹:大小限额 + 流式读取(不落临时缓冲再解压);
- 密码:仅命令行/stdin(`--password`、`UNP_PASSWORD` 环境变量),不进 argv 日志。

## 5. 格式支持矩阵(v1 = 发布即含)

| 层 | 格式 | 路线 | 状态 |
|---|---|---|---|
| L1 已验证 | zip jar apk epub cbz、rar cbr(含密码)、7z cb7 001(含AES)、tar 全家族、gz bz2 xz zst Z、arc arj zoo lha lzh cab | unarc-rs | PoC 通过 |
| L1 待补验 | ace cba adf dms | unarc-rs(无参照) | PoC 自检过;M1 内做 fuzz+真实语料验证 |
| L2 前端修复项 | 无扩展名 tar.gz/tbz/xz/zst、taz 消歧、freearc 误判 | 二跳探测 | M1 |
| L3 自写容器 | a(ar)、deb、rpm、cpio | 纯 Rust 手写(结构简单) | M2 |
| L3 接 crate | lzma 单流(.lzma)、lzip(.lz)、lz4、bzip3、iso9660(.iso) | `lzma-rust2`/`iso9660`/`lz4`/`bzip3` crate | M2 |
| L4 spec 重写 | udf、chm | 按公开规范纯 Rust 实现 | **Backlog 再议**(2026-10-30:需求弱,不排期) |
| L5 音频容器 | flac(`claxon`)、ape(`mac`)、shn(`shorten`) | crate 接入,只读 | M3 |
| L6 Backlog | alz(ARK,GPL 冲突)、freearc(codec 许可证差)、zpaq/shar/wav(本就外部工具语义)、vhd/wim(磁盘镜像) | **Backlog 再议**;过渡行为 = 识别格式 + 报错指引 | 不排期 |
| 永久禁区 | **RAR create** | 无算法/许可证 | 文档明示 |

## 6. 命令与能力(与 patool 对齐)

| 命令 | v1 能力 |
|---|---|
| `anyarc extract [-o DIR] [--password P] [--overwrite] <arch>...` | L1–L3 全格式;分卷自动聚合 |
| `anyarc test [-v]` | 同上,只解码校验 CRC 不落盘 |
| `anyarc list [-l/-t]` | 条目名/大小/时间 |
| `anyarc create <out> <files>...` | **白名单**:tar(.gz/.bz2/.xz/.zst)、zip(deflate/AES)、7z(LZMA2/AES)、bz2/gz/zst/xz 单流。密码仅 zip(AES)/7z |
| `anyarc repack <in> <out>` | in 按 L1–L3 读,out 按 create 白名单 |
| `anyarc diff <a> <b>` | 条目名+内容哈希差异(v1);文本级 diff(v2) |
| `anyarc search <pattern> <arch>...` | 条目名匹配(v1);解流内容 grep(v2) |
| `anyarc formats` / `--version` | 能力表输出 |

退出码:0 成功 / 1 运行时错误 / 2 用法错误;`-q/-v`  verbosity 与 patool 对齐;全局 `--json` 输出(list/search 机器可读)。

## 7. 测试与验收

1. **Golden 语料**:patool `tests/data` 全部 fixture + `poc/ref/*.manifest` 哈希清单作为固定测试集(脚本已生成,直接搬进 CI);
2. **真实语料扩展**:每格式收集 ≥50 个公开真实档案(BBS 老格式用 unarc-rs/icy_board 语料),与 7z/unar/bsdtar 三方输出对拍;
3. **Fuzz**:`cargo-fuzz` 对 L1 待补验(ACE/ADF/DMS/ARJ/RAR)与自写容器(ar/deb/rpm/cpio)做解码健壮性,标准:不 panic、不超限;
4. **差分测试**:`anyarc list/extract` vs `patool`(系统工具全装)在语料上的结果一致性;
5. **CLI 集成测试**:移植 patool `tests/test_*.py` 场景到 `assert_cmd`;
6. **平台矩阵**:linux-gnu/musl × x86_64/aarch64、windows-msvc、macOS,CI 全跑。

## 8. 里程碑

| 里程碑 | 内容 | 验收标准 | 预估 |
|---|---|---|---|
| **M1 骨架** | CLI + Backend trait + detect(含二跳修复)+ L1 全量 + 安全落盘 | PoC 矩阵 100% 绿(除已知 FAIL);`anyarc extract` 日常格式可用 | 2–3 周 |
| **M2 容器补全** | L3 自写容器 + crate 接入;`create`/`test`/`list` 白名单 | fixture 全绿;deb/rpm/cpio 与 7z 对拍一致 | 2–3 周 |
| **M3 收尾** | L5 音频只读;`repack`/`diff` | patool 高频格式对齐度 ~85%(加权;Backlog 项若再议通过则补排期) | 3–4 周 |
| **M4 发布工程** | cargo-dist 多平台矩阵、man page、shell 补全、变更日志 | 单文件 ≤25 MB,`ldd` 无依赖 | 1 周 |
| **M5 上游反哺** | unarc-rs PR(复合探测、FreeArc magic)、测试语料共享 | PR 合并 | 持续 |

**Backlog(再议池,无排期)**:udf、chm、alz、freearc、zpaq、shar、wav、vhd、wim。
触发再议的条件:真实使用中出现需求 / 出现可靠纯 Rust 实现或宽松许可证 C 库 / 上游 fuzz 资源顺路覆盖。

## 9. 风险登记

| 风险 | 等级 | 缓解 |
|---|---|---|
| ACE/ADF/DMS 逆向实现遇真实档案兼容性不足 | 中 | M1 起 fuzz+真实语料;失败档案给清晰错误+报告渠道;这三类 patool 原本也靠 helper(Linux 上基本不可用),不会比现状差 |
| unarc-rs 单一维护者 | 中 | 关键路径(sevenz/zip/tar)有独立第二实现可切换;fork 预案 |
| musl 下个别 crate 编译问题(如 chrono/lzma) | 低 | CI 每日构建提前暴露;必要时 gnu 静态链接兜底 |
| 二进制体积(全套解码器) | 低 | 实测 PoC ~10–20 MB 可接受;按层裁剪 feature 提供 slim 构建 |
| 与 patool 行为差异导致迁移摩擦 | 低 | CLI 参数/退出码对齐 + `formats` 能力表明示 + MIGRATION.md |

## 10. 待决问题(全部已处理,2026-10-30 用户拍板)

1. 命名 → **anyarc**(§1);
2. `--patool-compat` 兼容层 → **不做**,仅 MIGRATION.md + 配置迁移命令;
3. L6 放弃清单 → **不拍板"放弃",转 Backlog 再议**(需求弱;过渡行为=识别+报错指引);
4. udf/chm 排期 → 同上,**转 Backlog 再议**,M3 因此瘦身至 3–4 周;
5. crates.io 占位 → **不需要**(自用项目)。

## 11. 快速复现 PoC

```bash
cd /home/zym/code/anyarc
bash poc/ref_extract.sh            # 生成参照 manifest(需 7z/unar/tar/unzip/cabextract)
cargo run --release                # 输出 130 行兼容性矩阵
```
