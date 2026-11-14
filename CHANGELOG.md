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
- M4-c+ macOS 打包自动剔除垃圾文件:`create`/`repack` 在 macOS 运行时默认跳过
  Finder 元数据 `.DS_Store` 与 AppleDouble 伴生文件 `._*`(目录整棵不下探;
  显式传入的路径同样剔除);predicate 跨平台可测,Linux/CI 对拍行为不变
- M5 RAR 真流式后端(rars crate):绕开 unarc unified 的 Rar read 整档解码
  + 全量缓存缺陷(solid/分卷大档必然 OOM;unarc 打开分卷时还会整卷进内存)。
  回调式流式解码直写输出流,内存 O(解码窗口);成员头预枚举 → RAR 单条目
  大档(3GB+)走 pv 全功能进度条,多条目走 进度条+百分比+ETA;
  unix 权限从头解析(RAR5 host_os=1 / legacy=3)。
  限制:solid 多成员逐条提取为 O(n²) 重扫描(单条目场景无影响);
  RAR5 symlink/hardlink(redirection)按特殊条目跳过
- M5+ `extract --recursive/-r`(显式开启):解出的文件若仍是压缩包则继续向内解压、
  移除中间压缩包,一次直达最内层叶子;新增 `--smart` 智能扁平——解压内容被同名
  顶层目录重复包裹(book/book/)时自动合并一层
- M5++ RAR 分卷智能识别(不依赖文件名):首卷主头无卷号字段(WinRAR 行为)时,
  回退按文件名 partN 序号串联卷组(扩展名改坏也能解),并要求同 stem 前缀防误连;
  同一卷集的多个文件作参数自动去重;解压失败清理半成品输出
- M6 测试门禁与需求验收矩阵:
  - 新增 `tests/regression.rs`:17 个 CLI 二进制级用例,文件头维护
    "需求 ↔ 用例"对照表;新需求须同批补用例
  - GitHub Actions CI(Linux/Windows/macOS 三平台矩阵,`cargo test --release` 全绿门禁)
  - 本地 pre-push hook(`.githooks/pre-push`;克隆后 `git config core.hooksPath .githooks`
    启用,push 前自动跑全量测试)
  - README 重写为 GitHub 风格(badge/目录/特性表/快速上手/格式矩阵)
- M4 发布工程:
  - `anyarc completions <bash|zsh|fish|elvish|powershell>` 生成 shell 补全脚本  - `anyarc man` 输出 roff man page(`anyarc man | man -l -`)
  - release profile 静态化调优(LTO/strip/panic=abort);
    x86_64-unknown-linux-musl 全静态二进制 6.3 MB,`ldd` 无依赖
  - cargo-dist 配置与多平台发布矩阵(见 `dist.toml` / `.github/workflows/release.yml`)
- `poc/e2e.sh`:133 fixture CLI 端到端冒烟固化脚本

### 修复
- Info-ZIP 分卷 zip(.z01/.z02/…+.zip 末卷)支持:此前 `.zNN` 扩展名直接
  "无法识别",末卷 `.zip` 因文件头是数据流中段、中央目录虚拟偏移越出末卷
  尺寸而无法归类;现按扩展名路由 + 末卷 EOCD/兄弟卷存在性智能识别,
  任传一卷自动串联完整卷组(实测 18GB/26 文件卷集可列可解)。
  实现:自建虚拟串联流并原地修补 EOCD/ZIP64/CD 的磁盘号与相对偏移字段
  (兼容 WinRAR 与 Info-ZIP 两种偏移约定);条目元数据只读中央目录,
  加密条目的密码提示流程在解压前即可正确触发;数据读取全流式
- 注:Info-ZIP `zip -s` 3.0 的产物本身偏移错乱(官方 unzip 亦无法解压,
  首卷开头连 PK 魔数都被切坏),此类坏档解压时给出"制作工具写坏偏移"
  的明确报错,已属极限兼容
- 流式解压的大文件不再被 `--max-entry-size`(默认 4 GiB)误杀:该限额只约束
  "整体读入内存"的老路径(防 OOM);流式直写改由 `--max-total-size`(默认
  32 GiB)兜底,并在 extract 增加跨条目累计总量预算
- CLI 用户可见文案全面通俗化(去掉"落盘/条目/边信道"等黑话,报错信息
  附带调整指引)
- RAR 分卷名收集:RAR5 新式 `stem.partNNN.rar` 漏了 `part` 段(此前只会
  收到首卷,跨卷成员直接报错)
- 解压落盘不还原归档权限位(`.sh`/可执行脚本解出变 644):`Entry` 新增
  `mode` 字段,自写容器(tar/cpio/ar)从头部直填;unarc 后端(不提供 mode)
  经 header 边信道补齐 — zip 中央目录 external_attr(含 zip64)、tar 家族
  流式扫头部(Tgz/Tbz/Tzst/Txz 复合流多一次外层解码,自写容器则跳过);
  mode==0 退化数据不还原防 000 不可访问,--overwrite 重写前自动补写位;
  repack 目标归档同样携带源 mode(目录默认 755)
- 空/截断 cpio 输入 panic(index out of range)改为报错退出
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
