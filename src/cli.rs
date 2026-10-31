use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use clap::Parser;
use unarc_rs::unified::ArchiveFormat;

use crate::backend::{self, ArchiveBackend, EntryKind, Format, OpenOptions};
use crate::detect;
use crate::extract::{link_target_escapes, safe_join};

// ---------------------------------------------------------------- CLI

#[derive(Parser, Debug)]
#[command(name = "anyarc", version, about = "Universal archive tool: extract/test/list/create, zero external dependencies")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(clap::Subcommand, Debug)]
pub enum Cmd {
    /// 解压一个或多个归档
    Extract(ExtractArgs),
    /// 只解码校验,不落盘
    Test(TestArgs),
    /// 列出归档内容
    List(ListArgs),
    /// 打印格式支持表
    Formats,
}

#[derive(clap::Args, Debug)]
pub struct CommonArgs {
    /// 归档路径(多卷只需给出第一个/最后一个,自动聚合)
    pub archives: Vec<PathBuf>,
    /// 密码(缺省:ANYARC_PASSWORD 环境变量,或 tty 隐藏输入)
    #[arg(short = 'p', long)]
    pub password: Option<String>,
    /// 覆盖已存在文件(默认不覆盖并告警)
    #[arg(long)]
    pub overwrite: bool,
    /// 条目名编码修复(如 gbk):未标 UTF-8 的名字按 cp437→指定编码重解码
    #[arg(short = 'e', long)]
    pub encoding: Option<String>,
    /// 单条目解压上限(字节,0=不限;默认 4GiB)
    #[arg(long, value_parser = parse_size)]
    pub max_entry_size: Option<u64>,
    /// 整档解压总量上限(字节,0=不限;默认 32GiB)
    #[arg(long, value_parser = parse_size)]
    pub max_total_size: Option<u64>,
    /// 安静模式
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

fn parse_size(s: &str) -> Result<u64, String> {
    if s == "0" {
        return Ok(0);
    }
    s.parse::<u64>().map_err(|_| format!("无效大小: {s}"))
}

#[derive(clap::Args, Debug)]
pub struct ExtractArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// 输出目录(默认当前目录)
    #[arg(short = 'o', long, default_value = ".")]
    pub out: PathBuf,
}

#[derive(clap::Args, Debug)]
pub struct TestArgs {
    #[command(flatten)]
    pub common: CommonArgs,
}

#[derive(clap::Args, Debug)]
pub struct ListArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// 详细模式(大小/时间/加密/格式)
    #[arg(short = 'l', long)]
    pub long: bool,
    /// JSON 输出
    #[arg(long)]
    pub json: bool,
}

// ---------------------------------------------------------------- 公共流程

/// 探测 + 告警(偏移 zip)。
fn detect_with_warning(path: &Path, quiet: bool) -> anyhow::Result<detect::Detected> {
    let det = detect::detect(path)
        .map_err(|e| anyhow::anyhow!("探测失败 {}: {e}", path.display()))?
        .ok_or_else(|| anyhow::anyhow!("无法识别归档格式: {}", path.display()))?;
    if matches!(det.kind, detect::Known::Unarc(_)) && det.prefix_bytes > 0 && !quiet {
        eprintln!(
            "检测到 {} 字节前置数据(疑似伪装/SFX),条目名偏移已自动修正: {}",
            det.prefix_bytes,
            path.display()
        );
    }
    Ok(det)
}

/// 打开归档;若含加密条目且无密码,交互式获取并做探针验证。
pub fn open_verified(path: &Path, det: &detect::Detected, common: &CommonArgs) -> anyhow::Result<Box<dyn ArchiveBackend>> {
    let mut opts = open_opts(common, path);
    let mut ar = backend::open(path, det, &opts)?;

    // 是否含加密条目:先扫一遍条目(全 skip)
    let has_encrypted = scan_encrypted(ar.as_mut())?;
    if !has_encrypted {
        return Ok(reopen(path, det, &opts)?);
    }

    // 需要密码:CLI → env → tty
    let explicit = opts.password.is_some();
    if opts.password.is_none() {
        opts.password = std::env::var("ANYARC_PASSWORD").ok().filter(|s| !s.is_empty());
    }
    let tty = std::io::stdin().is_terminal();
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        if opts.password.is_none() {
            if !tty {
                anyhow::bail!("归档含加密条目,且无密码可用(用 --password 或 ANYARC_PASSWORD)");
            }
            eprint!("请输入密码 [{}]: ", path.display());
            let p = rpassword::read_password().map_err(|e| anyhow::anyhow!("{e}"))?;
            if p.is_empty() {
                return Err(anyhow::anyhow!("空密码已取消"));
            }
            opts.password = Some(p);
        }
        // 探针:重开 + 顺序就地读第一个加密条目(CRC 校验)
        match probe_password(path, det, &opts) {
            Ok(()) => return Ok(reopen(path, det, &opts)?),
            Err(e) => {
                if explicit {
                    let _ = e;
                    return Err(anyhow::anyhow!("密码错误(解密校验失败)"));
                }
                if attempts >= 3 {
                    let _ = e;
                    return Err(anyhow::anyhow!("密码验证失败(已重试 3 次)"));
                }
                eprintln!("密码无效(解密校验失败),请重试");
                opts.password = None;
            }
        }
    }
}

fn open_opts(common: &CommonArgs, path: &Path) -> OpenOptions {
    let mut o = OpenOptions {
        password: common.password.clone().or_else(|| std::env::var("ANYARC_PASSWORD").ok().filter(|s| !s.is_empty())),
        ..Default::default()
    };
    if let Some(v) = common.max_entry_size {
        o.max_entry_size = if v == 0 { None } else { Some(v) };
    }
    if let Some(v) = common.max_total_size {
        o.max_total_size = if v == 0 { None } else { Some(v) };
    }
    // 单流压缩格式:解压目标名 = 去扩展名(与 patool 语义一致)
    let det_fmt = ArchiveFormat::from_path(path);
    if let Some(f) = det_fmt.filter(|f| is_single_stream(*f)) {
        o.single_file_name = single_target_name(path, f);
    }
    o
}

fn is_single_stream(f: ArchiveFormat) -> bool {
    matches!(f, ArchiveFormat::Gz | ArchiveFormat::Bz2 | ArchiveFormat::Xz | ArchiveFormat::Zst | ArchiveFormat::Z)
}

/// 单流格式解压后的目标文件名:按扩展名剥离(.tgz → .tar 保留;foo.gz → foo)。
fn single_target_name(path: &Path, _f: ArchiveFormat) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let lower = name.to_lowercase();
    let lower = lower.strip_suffix(".foo").unwrap_or(&lower);
    for suf in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.Z", ".tgz", ".tbz2", ".txz", ".tzst"] {
        if lower.ends_with(suf) {
            return Some(name[..name.len() - suf.len()].to_string());
        }
    }
    if let Some(stem) = lower.rsplit_once('.') {
        let stem_len = stem.0.len();
        if !stem.1.is_empty() && stem_len > 0 {
            return Some(name[..stem_len].to_string());
        }
    }
    None
}

fn scan_encrypted(ar: &mut dyn ArchiveBackend) -> anyhow::Result<bool> {
    while let Some(e) = ar.next_entry()? {
        if e.encrypted {
            return Ok(true);
        }
        ar.skip_current()?;
    }
    Ok(false)
}

fn reopen(path: &Path, det: &detect::Detected, opts: &OpenOptions) -> anyhow::Result<Box<dyn ArchiveBackend>> {
    backend::open(path, det, opts)
}

/// 密码探针:重新打开并顺序就地读第一个 <1MiB 加密条目(CRC 校验)。
/// 不能"记录候选稍后再读":ARJ 等顺序流后端 skip 过位置后无法回读。
fn probe_password(path: &Path, det: &detect::Detected, opts: &OpenOptions) -> anyhow::Result<()> {
    let mut probe_opts = opts.clone();
    probe_opts.verify_crc = true;
    let mut ar = reopen(path, det, &probe_opts)?;
    while let Some(e) = ar.next_entry()? {
        if e.encrypted && e.size < 1 << 20 {
            ar.read_current()?; // 错误密码 → 解密/CRC 失败
            return Ok(());
        }
        ar.skip_current()?;
    }
    Ok(())
}

// ---------------------------------------------------------------- 名字修复

/// 应用 --encoding:cp437 字节 → 指定编码。仅对含非 ASCII 字符的名字生效。
fn fixed_name(raw: &str, enc: Option<&'static encoding_rs::Encoding>) -> String {
    let Some(enc) = enc else { return raw.to_string() };
    if raw.is_ascii() {
        return raw.to_string();
    }
    // 名字含非 ASCII 才处理;先按 cp437 编回字节
    let bytes = crate::cp437::encode_str(raw);
    let (cow, _, _) = enc.decode(&bytes);
    cow.into_owned()
}

// ---------------------------------------------------------------- 命令实现

pub fn cmd_extract(args: &ExtractArgs) -> anyhow::Result<u8> {
    let enc = args.common.encoding.as_deref().map(lookup_encoding).transpose()?;
    let mut failures = 0;
    for path in &args.common.archives {
        let det = detect_with_warning(path, args.common.quiet)?;
        let mut ar = open_verified(path, &det, &args.common)?;
        if let Err(e) = extract_one(ar.as_mut(), path, args, enc) {
            eprintln!("错误: {}: {e:#}", path.display());
            failures += 1;
        }
    }
    Ok(if failures == 0 { 0 } else { 1 })
}

fn extract_one(
    ar: &mut dyn ArchiveBackend,
    path: &Path,
    args: &ExtractArgs,
    enc: Option<&'static encoding_rs::Encoding>,
) -> anyhow::Result<()> {
    let dest = &args.out;
    std::fs::create_dir_all(dest)?;
    let single = matches!(ar.format(), Format::Unarc(f) if is_single_stream(f));
    let mut n_file = 0usize;
    let mut n_dir = 0usize;
    let mut n_skipped = 0usize;
    let mut n_overwrite_skip = 0usize;
    let show_progress = crate::extract::is_tty() && !args.common.quiet;
    let mut seq = 0usize;

    while let Some(entry) = ar.next_entry()? {
        seq += 1;
        let name = fixed_name(&entry.name, enc);
        match entry.kind {
            EntryKind::Directory => {
                if let Some(p) = safe_join(dest, &name) {
                    if !single {
                        std::fs::create_dir_all(&p)?;
                        n_dir += 1;
                    }
                } else {
                    n_skipped += 1;
                }
                ar.skip_current()?;
            }
            EntryKind::File => {
                let Some(p) = safe_join(dest, &name) else {
                    n_skipped += 1;
                    ar.skip_current()?;
                    continue;
                };
                if p.exists() && !args.common.overwrite {
                    n_overwrite_skip += 1;
                    ar.skip_current()?;
                    continue;
                }
                if show_progress {
                    eprint!("\r解压 [{seq}]: {name}");
                }
                let data = ar.read_current()?; // 读后条目已消费
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&p, &data)?;
                n_file += 1;
            }
            EntryKind::Symlink => {
                let Some(target) = entry.link_target else {
                    n_skipped += 1;
                    ar.skip_current()?;
                    continue;
                };
                let Some(p) = safe_join(dest, &name) else {
                    n_skipped += 1;
                    ar.skip_current()?;
                    continue;
                };
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if link_target_escapes(dest, p.parent().unwrap_or(dest), &target) {
                    eprintln!("警告: 符号链接逃逸已跳过: {name} -> {target}");
                    n_skipped += 1;
                    ar.skip_current()?;
                    continue;
                }
                #[cfg(unix)]
                {
                    let _ = std::fs::remove_file(&p);
                    std::os::unix::fs::symlink(&target, &p)?;
                }
                #[cfg(not(unix))]
                {
                    let _ = target;
                    n_skipped += 1;
                }
                ar.skip_current()?;
            }
            EntryKind::HardLink => {
                // M1:硬链接条目按"读内容写文件"降级(保持可读性),后续换真硬链接
                let Some(p) = safe_join(dest, &name) else {
                    n_skipped += 1;
                    ar.skip_current()?;
                    continue;
                };
                if p.exists() && !args.common.overwrite {
                    n_overwrite_skip += 1;
                    ar.skip_current()?;
                    continue;
                }
                let data = ar.read_current()?;
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&p, &data)?;
                n_file += 1;
            }
            EntryKind::Special => {
                n_skipped += 1;
                ar.skip_current()?;
            }
        }
    }
    if show_progress {
        eprintln!("\r解压完成: {} 个文件, {} 个目录({})", n_file, n_dir, path.display());
    }
    if n_skipped > 0 {
        eprintln!("警告: {} 个不安全/特殊条目已跳过(Zip-slip 防护)", n_skipped);
    }
    if n_overwrite_skip > 0 && !args.common.quiet {
        eprintln!("提示: {} 个已存在文件未覆盖(用 --overwrite 强制)", n_overwrite_skip);
    }
    Ok(())
}

pub fn cmd_test(args: &TestArgs) -> anyhow::Result<u8> {
    let mut failures = 0;
    for path in &args.common.archives {
        let det = detect_with_warning(path, args.common.quiet)?;
        let mut ar = open_verified(path, &det, &args.common)?;
        match test_one(ar.as_mut()) {
            Ok(files) => {
                if !args.common.quiet {
                    println!("OK: {} ({files} 个条目全部解码+CRC校验通过)", path.display());
                }
            }
            Err(e) => {
                eprintln!("错误: {}: {e:#}", path.display());
                failures += 1;
            }
        }
    }
    Ok(if failures == 0 { 0 } else { 1 })
}

fn test_one(ar: &mut dyn ArchiveBackend) -> anyhow::Result<usize> {
    let mut files = 0usize;
    while let Some(e) = ar.next_entry()? {
        if e.kind == EntryKind::Directory {
            ar.skip_current()?;
            continue;
        }
        ar.read_current()?;
        files += 1;
    }
    Ok(files)
}

pub fn cmd_list(args: &ListArgs) -> anyhow::Result<u8> {
    let enc = args.common.encoding.as_deref().map(lookup_encoding).transpose()?;
    for path in &args.common.archives {
        let det = detect_with_warning(path, args.common.quiet)?;
        let mut ar = open_verified(path, &det, &args.common)?;
        if args.json {
            println!("{{\"archive\":\"{}\",\"format\":\"{}\",\"entries\":[", path.display(), ar.format().name());
        }
        let mut first = true;
        while let Some(e) = ar.next_entry()? {
            let name = fixed_name(&e.name, enc);
            if args.json {
                if !first {
                    println!(",");
                }
                first = false;
                print!(
                    "  {{\"name\":\"{}\",\"kind\":\"{}\",\"size\":{},\"encrypted\":{}{}",
                    json_escape(&name),
                    kind_str(e.kind),
                    e.size,
                    e.encrypted,
                    time_json(e.mtime_epoch)
                );
            } else if args.long {
                // 解压后尺寸未知(deb/rpm 内嵌压缩流)时退显 packed 尺寸
                let shown = if e.size > 0 { e.size } else { e.compressed_size };
                println!(
                    "{:>10}  {}  {:<10} {}{}",
                    shown,
                    time_str(e.mtime_epoch),
                    e.method,
                    if e.encrypted { "*" } else { " " },
                    name
                );
            } else {
                println!("{name}");
            }
            ar.skip_current()?;
        }
        if args.json {
            println!("\n]}}");
        }
    }
    Ok(0)
}

fn kind_str(k: EntryKind) -> &'static str {
    match k {
        EntryKind::File => "file",
        EntryKind::Directory => "dir",
        EntryKind::Symlink => "symlink",
        EntryKind::HardLink => "hardlink",
        EntryKind::Special => "special",
    }
}

/// epoch → UTC 日历(Howard Hinnant civil_from_days)。
fn epoch_civil(e: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (e / 86400) as i64;
    let sod = e % 86400;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, (sod / 3600) as u32, (sod % 3600 / 60) as u32, (sod % 60) as u32)
}

fn time_str(t: Option<u64>) -> String {
    match t {
        Some(e) => {
            let (y, m, d, hh, mm, _) = epoch_civil(e);
            format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
        }
        None => "-".to_string(),
    }
}

fn time_json(t: Option<u64>) -> String {
    match t {
        Some(e) => {
            let (y, m, d, hh, mm, ss) = epoch_civil(e);
            format!(",\"mtime\":\"{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}\"")
        }
        None => String::new(),
    }
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn lookup_encoding(name: &str) -> anyhow::Result<&'static encoding_rs::Encoding> {
    encoding_rs::Encoding::for_label(name.as_bytes())
        .ok_or_else(|| anyhow::anyhow!("未知编码标签: {name}(试试 gbk / big5 / shift_jis / windows-1251)"))
}

pub fn cmd_formats() -> anyhow::Result<u8> {
    // 与 detect 的兜底/黑名单保持一致的能力表
    const ROWS: &[(&str, &str, &str)] = &[
        ("zip jar apk epub cbz xpi odt", "read", "含前置数据 zip(SFX/polyglot)、分卷 .z01、WinZip AES-99、ZipCrypto"),
        ("rar cbr", "read", "RAR4/RAR5;含密码;分卷 .rNN/.partNNN 自动聚合"),
        ("7z cb7 001", "read", "含 AES 头加密、分卷 .00N"),
        ("tar tgz taz tbz2 txz tzst tar.* cbt", "read", "全部 tar 家族"),
        ("gz bz2 xz zst Z", "read", "单流压缩"),
        ("arc arj zoo lha lzh", "read", "BBS 复古格式(arj 分卷 .aNN 支持)"),
        ("cab", "read", "MS Cabinet"),
        ("ace cba", "read", "无外部参照,变异 fuzz 通过"),
        ("adf hdf dms", "read", "Amiga 镜像"),
        ("a deb rpm cpio iso", "read", "自写容器;deb=data.tar,rpm=payload.cpio,iso=9660/Joliet"),
        ("freearc alz zpaq shar vhd wim udf chm lzma", "检测但不支持", "Backlog 再议;报错并给指引"),
    ];
    println!("{:<44} {:<14} 说明", "扩展名", "能力");
    for (ext, cap, note) in ROWS {
        println!("{ext:<44} {cap:<14} {note}");
    }
    Ok(0)
}
