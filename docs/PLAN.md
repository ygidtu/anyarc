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
5. **带前置数据的 zip(用户 `decrypt_zip.py` 场景,2026-10-30 补充实验)**:构造 3KB 假 MP4 前缀 + zip 体实测——`file(1)` 只报 MP4(patool 必失败的真实盲区);info-zip `unzip` 也 FAIL;而 **unarc-rs 后端强制按 Zip 打开时偏移计算正确、条目全部读出**(zip-rs 同样 OK)。同时验证 WinZip AES-256(method 99)与 ZipCrypto 的密码解密均通过。唯一缺口是 `ArchiveFormat::detect()` 无 EOCD 尾部扫描(带/不带路径提示均返回 None)→ 前端补探测即可,详见 §5 L2 与 §6。

## 4. 架构设计

```
anyarc/
├── src/
│   ├── main.rs          # 仅入口,解析参数 → commands
│   ├── cli.rs           # clap 定义,与 patool 参数对齐
│   ├── detect.rs        # magic+扩展名两级探测、复合流二跳、EOCD 反向扫描(polyglot/前置数据 zip)
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
| L1 待补验 | ace cba adf dms | unarc-rs(无参照) | 变异 fuzz 10800 次 CRASH=0/TIMEOUT=0(poc/fuzz_mutate.py) |
| L2 前端修复项 | 无扩展名 tar.gz/tbz/xz/zst、taz 消歧、freearc 误判 | 二跳探测 | M1 |
| L2 偏移 zip | **带前置数据的 zip**(SFX、伪装成 mp4/exe 的 polyglot 档;`decrypt_zip.py` 场景) | 前端 EOCD 反向扫描(尾部 ≤128KB)→ CD 校验 → 按 Zip 强制打开(后端偏移处理已实测可用);含 WinZip AES-99/ZipCrypto 密码路径 | M1 |
| L3 自写容器 | a(ar)、deb、rpm、cpio(newc+bin/hpbin)、iso9660+Joliet | 纯 Rust 手写(结构简单;iso 不用 GPL C) | **M2-a 完成**(2026-10-31,与 7z 对拍 MATCH) |

> **M2 对拍语义(2026-10-31 实测 7z,fixture `t.deb`/`t.rpm`)**:
> `deb` → 单条 `data.tar`(取 `data.tar.*` 成员并按内层压缩解压、剥扩展名;`debian-binary`/`control.tar.*` 不暴露);
> `rpm` → 单条 `{N-V-R.arch}.cpio`(payload 解压为 cpio 流,不二次展开);
> `ar` → 全部成员原样;`cpio`/`iso` → 目录树原样(目录条目矩阵中忽略)。

> **M2-a 验收(2026-10-31)**:矩阵 130 fixture:MATCH=77、NOREF-OK=13、MISMATCH=0、FAIL=40(全部为预期 Backlog/未实现);
> CLI e2e 133 文件:OK=90、拒绝=41、BAD 仅 `p.arc(.foo)`(unarc-rs 上游 ARC 密码 CRC 已知限制);
> 容器 10 fixture(含 .foo 孪生)全部 MATCH;`t.udf` 经 UDF 锚点探测路由 Backlog。

> **M2-b1 验收(2026-10-31)**:单流 lzma(.lzma/.tar.lzma)/lzip(.lz)/lz4 frame(.lz4)接入;
> 复合(tar 在内)按 `looks_tar` 自动展开为目录树,单文件按 `single_stem` 命名(支持 .foo 伪装剥离);
> 矩阵 MATCH=81(+4)、NOREF-OK=19(+6)、MISMATCH=0、FAIL=30;lz fixture 与 7z/patool 内容对拍一致
> (t.txt.lz→t.txt sha 73475cb4…;t.tar.lz→tar 树)。手写 lzip varint 解析曾把 payload props 字节
> 误并入 bds(LEB 无 continuation 位歧义)→ 改用 lzma-rust2 标准 LzipReader。

> **M2-b2 验收(2026-10-31)**:`create` 写路径白名单落地(tar 家族 12 变体、zip(deflate+WinZip AES-256)、
> 7z(LZMA2)、单流 gz/bz2/xz/zst/lzma/lz/lz4;临时文件+rename;无 `--overwrite` 拒覆盖;目录输入按
> basename 作归档根;symlink 走 tar `append_link`/zip `add_symlink`)。7z 26.04 实测互读 tar*/zip/7z/lzma 全绿,
> `.lz/.lz4` 互读失败系 7z 自身无 LZIP/LZ4 格式项(`7z i` 实证),非我方产物问题。7z 创建密码(7zAES)缺编码器
> → 清晰报错。list 改为免密码探针(加密条目名可列,`*` 标记)。回归:单测 12 全绿;矩阵 130 例
> MATCH=81/NOREF-OK=19/MISMATCH=0/FAIL=30 不变;e2e OK=100 BAD=2(仅 p.arc 上游);fuzz 复跑通过。

> **M2-b3 加固(2026-10-31)**:复现并修复 unarc 0.8.1 gzip 单流只解**第一个成员**的语义缺陷
> (多成员 .gz 实测 6B vs 12B 全量,不符 gzip/7z/patool):`.gz` 改路由自写 `MultiGzDecoder`
> (detect `Container::Gz`;复合 tar-in-gz 由 looks_tar 等价处理,`taz` 二跳自然退役)。
> 回归:多成员/单成员/不同级别 gz 9 例全对;矩阵 81/19/0/30 不变;e2e OK=100;fuzz_headers 增 t.txt.gz 头区。

> **M3-a 验收(2026-10-31)**:`repack`/`diff`/`search` 落地。create.rs 重构为内存条目模型
> (`WriteEntry{File|Dir|Symlink}`+mode+mtime),repack 读 L1–L3 任意源(密码/env)写白名单目标;
> 目标加密与源密码分离(`--encrypt`);7z 目标拒 symlink(7z 外部属性缺位)、全目标拒 hardlink/special(v1);
> zip 写 mtime(civil_from_unix + MS-DOS 打包),7z 写 mtime(NtTime ticks);tar 全类型条目 append_data/append_link。
> 实证:tgz→zip-AES→解回、p.zip→7z(env 密码)、gz→zst(sha 73475cb4…)、rar→tar、tar→tar.gz、
> tar→zip(含 symlink)全部 diff 一致;diff 语义=条目名+sha256(0/1/2 退出),search=glob/子串(grep 退出)。
> 修 diff/repack 循环 Dir/Symlink 未 skip 死循环(trait 契约:读或 skip 后才前进)。

> **M3-c/M3-d 验收(2026-10-31)**:extract 的 HardLink 条目改真实硬链接(`fs::hard_link` 到
> 捐赠者解出路径,失败降级写内容;hl.tar/hl.tar.gz inode 一致✓);`diff`/`search` 增 `--json`。
> cpio newc 两处存量 bug 修复:①while 条件 `&data[pos..pos+6] == b"0707"`(6 字节切片 vs 4 字节
> 数组恒不等)→ newc 条目流从未被解析过(list 空);②namesize/chksum 字段偏移(标准 namesize@94
> chksum@102,误写 @86/@94)。并给 newc/bin/hpbin 全部接入 (dev,ino) 硬链接二段归并(GNU cpio
> 把数据挂在后出现的 dup 名下,dup 排前;按"带数据的捐赠者"选主而非出现顺序),extract 加 pass 2
> 延后硬链接(目标未出现时先记,主循环后补链)。回归:单测 14(新增 newc hardlink 合成用例);
> 矩阵 81/19/0/30 不变;e2e OK=100 BAD=2(仅 p.arc 上游);fuzz_mutate 3060 + newc 定向 fuzz
> 2400 CRASH=0;hpbin fixture t.cpio 行为不变。

> **M3-e decrypt_zip 场景终验(2026-10-31)**:对照 poc 语义逐项验证——错密码探针(exit 1
> “密码错误”,连续 -p 猜密码可行)、SFX/前置字节 zip(EOCD 定位+偏移告警)、GBK 名修复
> (真 GBK fixture:list/extract `-e gbk` 输出“中文目录/文件.txt”;UTF-8 名 zip 正确解码)。
> 加固:`--encoding` 现仅对 **cp437 可逆**的名字修复(`cp437::try_encode_str`;不可逆即原样保留),
> 防 `-e` 误伤已正确的 UTF-8 名。单测 +2(encoding 守卫/glob 语义)共 16;矩阵/e2e 不变;
> fuzz_headers 32666 + fuzz_mutate 2040 CRASH=0。

> **M3-L5 音频容器(2026-10-31)**:detect 增 Flac/Ape/Shn 魔数(`fLaC`/"MAC "/`ajkg`)+
> 扩展名路由;extract/list/test 语义 = 解码为单条目 `{stem}.wav`(patool 对齐;有 RIFF
> 信封的 SHN 保留原 chunk 布局仅修正 RIFF/data size)。实现:FLAC=claxon(流式样本
> 限额);APE=ape-decoder(仅 3.95+,旧版清晰报错,info 预估预检限额);SHN=**自研解码器
> `src/shn.rs`**(oxideav-shorten 0.0.3 与 0.0.4 对 GNU shorten 实产 t.shn 均报
> OverflowingUvar——其命令表/位流模型与真实 v2 流不符,弃用;自写 clean-room 实现,
> 依据 TR.156/公开格式事实,字段语义经 GNU 产物字节级校准,未复制任何第三方代码;
> v0/v1 与 u-law/alaw 有损 filetype 明确报错)。oracle:t.flac→t.flac.wav、
> t.ape→t.wav、t.shn→t.wav 全部 **cmp 字节全等**。单测 +2(shn oracle 对照 +
> 畸形输入不 panic)共 18。矩阵 MATCH=81/NOREF-OK=25(音频 6 行 FAIL→NOREF-OK;
> 7z/unar 无音频 ref)/MISMATCH=0/FAIL=24。e2e(新增 `poc/e2e.sh` 固化脚本):
> total=133 OK=105 REFUSED=26 BAD=2(仅 p.arc 上游)。注:unarc 对无密 t.arc 在
> ANYARC_PASSWORD 环境下会误解密致 extract CRC 报错(上游怪癖,e2e 已按 p.* 才带密码;
> 列入 M5 upstream 报告素材)。fuzz_mutate 增 t.flac/t.ape/t.shn 与 fuzz_headers 增
> 音频头区复跑:mutate 10800(N=200×54)+ headers 36918,CRASH=0 / TIMEOUT=0。

> **M4-a 发布工程(2026-10-31)**:
> ① `anyarc completions <bash|zsh|fish|elvish|powershell>`(clap_complete)+ `anyarc man`
> (clap_mangen,roff → `anyarc man | man -l -`),零外部依赖不变;两者对 BrokenPipe
> (`| head`)静默容忍(panic=abort 下必须);
> ② release profile 调优(lto=fat/codegen-units=1/strip/panic=abort)→ gnu 6.2 MB、
> musl 6.3 MB(验收 ≤25 MB);x86_64-unknown-linux-musl **static-pie,ldd 无依赖**,
> 矩阵/ e2e/ fuzz(headers 36918 + mutate 2160)在新 profile 下回归全绿(CRASH=0);
> ③ cargo-dist 0.32(`dist init/generate/plan/build/linkage`):`dist-workspace.toml`
> 七目标矩阵(gnu/musl × x86_64/aarch64 + win-msvc + macOS 双架构)+ shell/powershell
> 安装器 + CHANGELOG include;本地 `dist build --target x86_64-unknown-linux-musl`
> 产物 2.5 MB(xz),解包 anyarc 直接跑 matrix 130 例 = 81/25/0/24 一致;
> ④ README 重写 + CHANGELOG.md 新建。
> 待办:仓库无 GitHub remote → `release.yml` 已生成但 CI 真机矩阵(Windows/macOS/
> aarch64 交叉)未验证;`Cargo.toml` repository=TODO 占位,换真实地址后 `dist generate`
> 刷新;`dist build` 产物含 matrix 工具(cargo-dist 0.32 无法按 bin 过滤,publish=false/
> required-features 均无效;合计 <12 MB 可接受,M4-b 或拆 examples 再议)。

> **M4-b 进度条(2026-10-31)**:indicatif(纯 Rust,~100KB)接入 extract/test/
> create/repack 四个耗时路径;`src/progress.rs` 提供 EntryBar/ExtractBar,
> 构造顺序 new→set_style→set_prefix→tick(防默认样式 `{bar} {pos}/{len}` 闪现,
> 曾出现 "0/18446744073709551615" 噪声帧);未知总数样式仅 spinner+pos。
> 门控:stderr TTY && !quiet(非 TTY/`-q` 完全静默,stdout 与 --json 不受影响);
> create 读取阶段二段式(plan_entries_with_map→read_file_entries,>8 条目才画)。
> musl 产物 6.4 MB(验收不变);回归:单测 18 全绿、矩阵 81/25/0/24、e2e 105/26/2、
> fuzz_mutate 3240 CRASH=0。genozip-rs(progress.rs 手写 ANSI 方案)整合评估见 §10。

| L3 接 crate | lzma 单流(.lzma)、lzip(.lz)、lz4(.lz4) | lzma-rs(raw,MIT)+ lzma-rust2(LzipReader,Apache-2.0)+ lz4_flex(MIT);**bzip3 排除**(绑定 LGPL-3 C 库,转 Backlog) | **M2-b1 完成**(2026-10-31) |
| L4 spec 重写 | udf、chm | 按公开规范纯 Rust 实现 | **Backlog 再议**(2026-10-30:需求弱,不排期)。2026-10-31:混合介质(ISO9660+UDF)已可按标准 AVDP(`BEA01VRS`@LBA256/512)与 genisoimage 内嵌指针(LBA18/19)识别,路由 Backlog 报错而非误走 ISO 解析) |
| L5 音频容器 | flac(`claxon`)、ape(`ape-decoder`)、shn(自研 `src/shn.rs`) | crate 接入 + 自写位流解码,只读 | **M3-L5 完成**(2026-10-31,oracle 字节全等) |
| L6 Backlog | alz(ARK,GPL 冲突)、freearc(codec 许可证差)、**bzip3(官方实现为 LGPL-3 C 绑定,2026-10-31 排除)**、zpaq/shar/wav(本就外部工具语义)、vhd/wim(磁盘镜像) | **Backlog 再议**;过渡行为 = 识别格式 + 报错指引 | 不排期 |
| 永久禁区 | **RAR create** | 无算法/许可证 | 文档明示 |

## 6. 命令与能力(与 patool 对齐)

| 命令 | v1 能力 |
|---|---|
| `anyarc extract [-o DIR] [--password P] [--overwrite] [--encoding enc] <arch>...` | L1–L3 全格式;分卷自动聚合;**前置数据 zip 自动 EOCD 定位并告警偏移量** |
| `anyarc test [-v]` | 同上,只解码校验 CRC 不落盘 |
| `anyarc list [-l/--json]` | 条目名/大小/时间/方法/加密标记;加密条目无密码也可列出(zip 中央目录明文;7z 头加密则 open 即要求密码)。注:`-t` 早期草拟,实际由 `-l` 覆盖 |
| `anyarc create <out> <files>...` | **白名单**:tar(.gz/.bz2/.xz/.zst/.lzma/.lz/.lz4 及各缩写)、zip(deflate/WinZip AES-256)、7z(LZMA2)、单流 gz/bz2/xz/zst/lzma/lz/lz4(恰好 1 输入)。密码仅 zip AES(7zAES 编码器缺位→清晰报错);RAR 创建永久禁区;先写 `*.anyarc-tmp` 再 rename,无 `--overwrite` 拒绝覆盖。7z 26.04 实测互读:tar*/zip/7z/lzma ✓;`.lz/.lz4` 非我方缺陷(7z 格式表无 LZIP/LZ4) |
| `anyarc repack <in> <out>` | in 按 L1–L3 读,out 按 create 白名单;源密码 `-p`/env,目标加密 `--encrypt`(仅 zip AES);hardlink/special 条目明确拒绝(v1) |
| `anyarc diff <a> <b>` | 条目名+内容哈希差异(sha256;dir/link 按指纹);exit 0=一致 / 1=有差异 / 2=用法 |
| `anyarc search <pattern> <arch>...` | 条目名匹配:`*`/`?` 走 glob,否则大小写敏感子串(`-i` 忽略);exit 0=命中 / 1=未命中(grep 语义) |
| `anyarc formats` / `--version` | 能力表输出 |

退出码:0 成功 / 1 运行时错误 / 2 用法错误;`-q/-v`  verbosity 与 patool 对齐;全局 `--json` 输出(list/search 机器可读)。

从 `decrypt_zip.py` 并入的 UX 语义(实测需求,已定):
- **密码交互**:`--password` 缺省时从 tty 隐藏读取(也支持 `UNP_PASSWORD`→更名 `ANYARC_PASSWORD` 环境变量);
- **密码探针**:extract 前先用最小条目做一次真实解密,密码错误立即失败,不半途解出残包;
- **文件名编码修复**:`--encoding gbk` 等——未标 UTF-8 flag(0x800)的条目名按 cp437→指定编码重解码;默认仍按 zip 规范(cp437/UTF-8);
- **进度输出**:大档按 条目数+字节百分比 刷行(仅 tty;`--json` 或管道时关闭);
- **默认输出目录**:`-o` 缺省解到当前目录(patool 语义);伪装档/前置数据 zip 解包时 stderr 固定告警:`检测到 N 字节前置数据(疑似伪装/SFX),条目名偏移已自动修正`。

## 7. 测试与验收

1. **Golden 语料**:patool `tests/data` 全部 fixture + `poc/ref/*.manifest` 哈希清单作为固定测试集(脚本已生成,直接搬进 CI);
2. **真实语料扩展**:每格式收集 ≥50 个公开真实档案(BBS 老格式用 unarc-rs/icy_board 语料),与 7z/unar/bsdtar 三方输出对拍;
3. **Fuzz**:`cargo-fuzz` 对 L1 待补验(ACE/ADF/DMS/ARJ/RAR)与自写容器(ar/deb/rpm/cpio)做解码健壮性,标准:不 panic、不超限;
4. **差分测试**:`anyarc list/extract` vs `patool`(系统工具全装)在语料上的结果一致性;
5. **CLI 集成测试**:移植 patool `tests/test_*.py` 场景到 `assert_cmd`;
6. **平台矩阵**:linux-gnu/musl × x86_64/aarch64、windows-msvc、macOS,CI 全跑。
7. **偏移/伪装 zip 专项**(源自 `decrypt_zip.py` 场景):自建 fixture 集——假 MP4/EXE 前置 + zip 体、WinZip AES-256(method 99)、ZipCrypto、GBK 文件名、SFX(带 stub)。基准用 **python zipfile**(实测 info-zip `unzip` 对该类文件也 FAIL,不可作此用例基准);验收:检测出"zip(前置数据 N 字节)"、解出条目与基准逐字节一致、错误前置(伪 EOCD)不误判。

## 8. 里程碑

| 里程碑 | 内容 | 验收标准 | 预估 |
|---|---|---|---|
| **M1 骨架** | CLI + Backend trait + detect(含二跳修复)+ L1 全量 + 安全落盘 | PoC 矩阵 100% 绿(除已知 FAIL);`anyarc extract` 日常格式可用 | 2–3 周 |
| **M2 容器补全** | L3 自写容器 + crate 接入;`create`/`test`/`list` 白名单 | fixture 全绿;deb/rpm/cpio 与 7z 对拍一致 | 2–3 周 |
| **M3 收尾** | L5 音频只读;`repack`/`diff` | patool 高频格式对齐度 ~85%(加权;Backlog 项若再议通过则补排期) | 3–4 周 |
| **M4 发布工程** | cargo-dist 多平台矩阵、man page、shell 补全、变更日志、进度条 | 单文件 ≤25 MB,`ldd` 无依赖 | **进行中**(M4-a/b 完成:补全/man/musl 静态/dist 矩阵/indicatif 进度;真实 CI 矩阵待 remote) |
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
bash poc/ref_extract.sh                    # 生成参照 manifest(需 7z/unar/tar/unzip/cabextract)
cargo build --release
./target/release/matrix                    # 130 例兼容性矩阵(= poc/results.md 的生成器)
python3 poc/fuzz_mutate.py [N]             # 字节翻转 fuzz(CRASH/TIMEOUT 应为 0)
```
