//! RAR 专用流式后端(rars crate)。
//!
//! 为什么不用 unarc unified 路径:其 Rar `read()` 从卷头开始整档解码,并把
//! target 之后所有成员的解压结果全量缓存(DecodeCache),solid/多卷大档必然
//! OOM;且分卷打开时对每卷 `read_to_end` 全量进内存。rars 的
//! `extract_volumes_to` 是回调式流式解码(open 返回 Writer),解码数据直写
//! 输出流,内存 O(解码窗口)。
//!
//! 语义与限制:
//! - 每次读取从卷头顺序解码到目标成员(solid 依赖前置成员),目标之前的数据
//!   写入 sink —— 多条目归档逐条提取时 CPU 为 O(n²),单条目/少条目(常见
//!   采样包、安装包)不受影响;
//! - RAR5 symlink/hardlink(redirection)条目按 Special 跳过(v1 未迁移
//!   unarc 的取目标语义);
//! - 权限:host_os == 3(unix)时 file_attr 即 st_mode,Entry.mode 直填。

use std::cell::Cell;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use rars::{ArchiveReadOptions, ArchiveReader, Error as RarsError, ExtractedEntryMeta};
use unarc_rs::unified::ArchiveFormat;

use crate::backend::{ArchiveBackend, Entry, EntryKind, Format, OpenOptions};

pub struct RarBackend {
    volumes: Vec<rars::Archive>,
    members: Vec<MemberInfo>,
    password: Option<String>,
    next: usize, // 下一待返回成员游标(独立于 current,读/skip 消费不回退)
    current: Option<usize>,
    max_entry_size: Option<u64>,
    max_total_size: Option<u64>,
    /// 本次解压消费掉的全部卷文件路径(供 CLI 对其他参数去重)
    volume_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
struct MemberInfo {
    raw_name: Vec<u8>,
    name: String,
    kind: EntryKind,
    size: u64,
    packed: u64,
    mtime: Option<u64>,
    mode: Option<u32>,
    encrypted: bool,
}

impl RarBackend {
    pub fn open(path: &Path, opts: &OpenOptions) -> anyhow::Result<Self> {
        let ropts = ArchiveReadOptions::with_optional_password(opts.password.as_deref().map(str::as_bytes));
        let mut vol_paths = collect_rar_volumes(path);
        let mut volumes: Vec<rars::Archive> = Vec::new();
        if vol_paths.len() == 1 {
            // 先解析首卷;若包体头声明"我是多卷之一"(RAR5 有卷号字段),
            // 按**卷号**扫描同目录兄弟文件串联卷组 —— 不依赖文件名规范
            let a0 = ArchiveReader::read_reader_with_options(BufReader::new(File::open(path)?), ropts)
                .map_err(|e| map_rars_error(e, path))?;
            if declares_volume(&a0) {
                // 卷组串联:gather 内部按"主头卷号→文件名 partN"两级回退
                vol_paths = gather_sibling_volumes(path, &a0, &ropts);
            }
            volumes.push(a0);
            for v in &vol_paths[1..] {
                let a = ArchiveReader::read_reader_with_options(BufReader::new(File::open(v)?), ropts)
                    .map_err(|e| map_rars_error(e, v))?;
                volumes.push(a);
            }
        } else {
            for v in &vol_paths {
                let a = ArchiveReader::read_reader_with_options(BufReader::new(File::open(v)?), ropts)
                    .map_err(|e| map_rars_error(e, v))?;
                volumes.push(a);
            }
        }
        let members = scan_members(&volumes);
        if members.is_empty() {
            anyhow::bail!("RAR: 未解析到任何条目({})", path.display());
        }
        let volume_paths = vol_paths
            .iter()
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
            .collect();
        Ok(Self {
            volumes,
            members,
            password: opts.password.clone(),
            next: 0,
            current: None,
            max_entry_size: opts.max_entry_size,
            max_total_size: opts.max_total_size,
            volume_paths,
        })
    }

    /// 限额语义:整读进内存(read_current)受 --max-entry-size 约束(防 OOM);
    /// 流式直写(read_current_to,内存 O(窗口))受 --max-total-size 约束(磁盘防护)。
    fn check_size(&self, size: u64, streaming: bool) -> anyhow::Result<()> {
        let (cap, flag) = if streaming {
            (self.max_total_size.unwrap_or(u64::MAX), "--max-total-size")
        } else {
            (self.max_entry_size.unwrap_or(u64::MAX), "--max-entry-size")
        };
        if size > cap {
            anyhow::bail!("单个文件太大:解压后 {size} 字节超过上限 {cap}(可用 {flag} 调整,0=不限)");
        }
        Ok(())
    }
}

fn map_rars_error(e: RarsError, path: &Path) -> anyhow::Error {
    match e {
        RarsError::NeedPassword => {
            anyhow::anyhow!("RAR 头部已加密:需要密码(--password / ANYARC_PASSWORD)")
        }
        _ => anyhow::anyhow!("RAR {}: {e}", path.display()),
    }
}

/// 包体头是否声明"属于多卷集"(RAR4 也真,但无卷号字段,只能靠命名规范)
fn declares_volume(a: &rars::Archive) -> bool {
    match a {
        rars::Archive::Rar50Plus(x) => x.main.is_volume(),
        rars::Archive::Rar15To40(x) => x.main.is_volume(),
        rars::Archive::Rar13(x) => x.main.is_volume(),
        _ => false,
    }
}

/// 卷号仅 RAR5 头字段可得(RAR4 卷序只存在于文件名约定,走 collect_rar_volumes)
fn volume_number(a: &rars::Archive) -> Option<u64> {
    match a {
        rars::Archive::Rar50Plus(x) if x.main.is_volume() => x.main.volume_number,
        _ => None,
    }
}

fn rar_debug_enabled() -> bool {
    std::env::var_os("ANYARC_DEBUG").is_some()
}

/// 文件是否以 RAR 魔数开头(读前 7 字节;Rar!\x1a\x07 + 01/02 版本字节)
fn is_rar_magic_file(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 7];
    matches!(f.read_exact(&mut magic), Ok(()) if &magic[..6] == b"Rar!\x1a\x07" && matches!(magic[6], 0x01 | 0x02))
}

/// 从文件名提取 `partN` 序号(如 xx.part1.任意后缀 → 1)。
/// WinRAR 少卷档主头不写卷号字段,文件名序号是可靠回退;扩展名被改坏也有效。
fn part_number_in_name(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    let pos = name.find("part")?;
    let digits: String = name[pos + 4..].chars().take_while(|c| c.is_ascii_digit()).collect();
    (!digits.is_empty()).then(|| digits.parse().ok())?
}

/// 卷集 stem:文件名 "partN" 之前的前缀(小写),用于排除同目录其他卷组
fn volume_stem(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    let pos = name.find("part")?;
    Some(name[..pos].to_string())
}

/// 按卷号串联:收集目录内所有"声明为卷"的 RAR 文件;
/// 全体有主头卷号 → 按卷号;否则按文件名 partN;再不行放弃(保持原报错)。
fn gather_sibling_volumes(path: &Path, first: &rars::Archive, ropts: &ArchiveReadOptions<'_>) -> Vec<PathBuf> {
    struct Vol {
        p: PathBuf,
        no: Option<u64>,
        part: Option<u64>,
    }
    let mut vols = vec![Vol { p: path.to_path_buf(), no: volume_number(first), part: part_number_in_name(path) }];
    let stem = volume_stem(path);
    if let Some(dir) = path.parent()
        && let Ok(rd) = std::fs::read_dir(dir)
    {
        for e in rd.flatten() {
            let p = e.path();
            if p == *path || !is_rar_magic_file(&p) {
                continue;
            }
            // 只接受同卷组(stem 前缀一致)的兄弟,避免误连同目录其他多卷包
            let same_group = match (&stem, volume_stem(&p)) {
                (Some(s), Some(ps)) => ps.as_str() == s.as_str(),
                (None, _) => true, // 本卷无 part 序号时不做前缀限制
                (Some(_), None) => false,
            };
            if !same_group {
                continue;
            }
            let Ok(f) = File::open(&p) else { continue };
            match ArchiveReader::read_reader_with_options(BufReader::new(f), *ropts) {
                Ok(a) => {
                    if declares_volume(&a) {
                        if rar_debug_enabled() {
                            eprintln!("[rar-debug] 兄弟卷 {} 主头卷号={:?} 文件名序号={:?}", p.display(), volume_number(&a), part_number_in_name(&p));
                        }
                        vols.push(Vol { p: p.clone(), no: volume_number(&a), part: part_number_in_name(&p) });
                    }
                }
                Err(err) => {
                    if rar_debug_enabled() {
                        eprintln!("[rar-debug] 解析失败 {}: {err}", p.display());
                    }
                }
            }
        }
    }
    // 统一编号源:主头卷号齐全优先,否则文件名 partN 齐全
    let use_no = vols.iter().all(|v| v.no.is_some());
    let use_part = !use_no && vols.iter().all(|v| v.part.is_some());
    let key = |v: &Vol| if use_no { v.no } else if use_part { v.part } else { None };
    let pairs: Vec<(u64, PathBuf)> = vols.iter().filter_map(|v| key(v).map(|k| (k, v.p.clone()))).collect();
    if pairs.len() < 2 {
        return vec![path.to_path_buf()];
    }
    let by_no: std::collections::BTreeMap<u64, PathBuf> = pairs.into_iter().collect();
    let my_key = key(&vols[0]);
    if rar_debug_enabled() {
        eprintln!("[rar-debug] 卷图 {by_no:?} 本卷key={my_key:?}");
    }
    volume_chain(&by_no, my_key.unwrap_or(0))
}

/// 从卷号 map 中取包含 cur_no 的连续段,升序路径列表。
fn volume_chain(by_no: &std::collections::BTreeMap<u64, PathBuf>, cur_no: u64) -> Vec<PathBuf> {
    let mut lo = cur_no;
    while lo > 0 && by_no.contains_key(&(lo - 1)) {
        lo -= 1;
    }
    let mut out = Vec::new();
    let mut i = lo;
    while let Some(p) = by_no.get(&i) {
        out.push(p.clone());
        i += 1;
    }
    out
}

fn scan_members(volumes: &[rars::Archive]) -> Vec<MemberInfo> {
    let mut out: Vec<MemberInfo> = Vec::new();
    for v in volumes {
        for m in v.members() {
            let raw = m.meta.name.clone();
            // 跨卷同名 = 同一成员的续块(头只在起始卷),防御性去重
            if out.iter().any(|e| e.raw_name == raw) {
                continue;
            }
            let meta = &m.meta;
            let name = String::from_utf8_lossy(&raw).replace('\\', "/");
            let kind = if meta.is_directory {
                EntryKind::Directory
            } else if meta.is_redirection {
                // RAR5 symlink/hardlink:内容重定向,暂不支持迁移
                EntryKind::Special
            } else {
                EntryKind::File
            };
            // host_os 编号随格式族不同:RAR5 官方 spec 1=Unix;legacy(RAR1.x-4.x)3=Unix
            let unix = match meta.family {
                rars::ArchiveFamily::Rar50Plus => meta.host_os == Some(1),
                _ => meta.host_os == Some(3),
            };
            let mode = unix.then_some((meta.file_attr & 0o7777) as u32);
            // file_time:RAR5 = unix 秒;legacy = DOS 时间
            let mtime = match meta.family {
                rars::ArchiveFamily::Rar50Plus => meta.file_time.map(u64::from),
                _ => meta.file_time.map(dos_time_to_epoch),
            };
            out.push(MemberInfo {
                raw_name: raw,
                name,
                kind,
                size: if meta.is_directory { 0 } else { meta.unpacked_size },
                packed: meta.packed_size,
                mtime,
                mode,
                encrypted: meta.is_encrypted,
            });
        }
    }
    out
}

/// DOS 时间(0x… YY/MM/DD HH:MM:SS 打包)→ epoch 秒(与 unarc 的 dos_epoch 同式)。
fn dos_time_to_epoch(d: u32) -> u64 {
    let (mut y, mo, dd) = (i64::from((d >> 25) & 0x7f) + 1980, i64::from((d >> 21) & 0x0f), i64::from((d >> 16) & 0x1f));
    if y < 1980 || mo == 0 || dd == 0 {
        return 0;
    }
    let (hh, mm, ss) = (i64::from((d >> 11) & 0x1f), i64::from((d >> 5) & 0x3f), i64::from(d & 0x1f) * 2);
    y -= i64::from(mo <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = 153 * (mo + if mo > 2 { -3 } else { 9 }) + 2;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy / 5 + dd - 1;
    let days = era * 146097 + doe - 719468;
    (days as u64) * 86400 + (hh * 3600 + mm * 60 + ss) as u64
}

/// RAR 分卷文件收集:xxx.partN.rar(RAR5 新式)/ xxx.rNN(RAR4 旧式)/ xxx.NNN。
fn collect_rar_volumes(path: &Path) -> Vec<PathBuf> {
    let mut vols = vec![path.to_path_buf()];
    let Some(dir) = path.parent() else {
        return vols;
    };
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return vols;
    };
    let lower = name.to_lowercase();
    // RAR5 新式:stem.partNNN.rar(N≥2 连续)
    if let Some(pos) = lower.rfind(".part") {
        let rest = &lower[pos + 5..];
        if let Some((num, ext)) = rest.split_once('.')
            && ext == "rar"
            && !num.is_empty()
            && num.chars().all(|c| c.is_ascii_digit())
        {
            let digits = num.len();
            let stem = &name[..pos];
            let mut i: u32 = num.parse().unwrap_or(1) + 1;
            loop {
                let cand = dir.join(format!("{stem}.part{:0width$}.rar", i, width = digits));
                if !cand.is_file() {
                    break;
                }
                vols.push(cand);
                i += 1;
            }
            return vols;
        }
    }
    // RAR4 旧式:stem.rar → stem.r00, r01, …
    if let Some(stem) = name.strip_suffix(".rar") {
        for i in 0..100u32 {
            let cand = dir.join(format!("{stem}.r{i:02}"));
            if cand.is_file() {
                vols.push(cand);
            } else {
                break;
            }
        }
        // 数值分卷:stem.001, .002 …(首卷是 .rar 时不常见,兜底)
        if vols.len() == 1 {
            for i in 1..1000u32 {
                let cand = dir.join(format!("{stem}.{i:03}"));
                if cand.is_file() {
                    vols.push(cand);
                } else {
                    break;
                }
            }
        }
    }
    vols
}

/// 生命周期桥:extract 回调要求 Box<dyn Write>('static),而我们的目标是
/// 调用栈上的 `&mut dyn Write`(生命周期 '1)。裸指针 + transmute 抹平
/// 生命周期;实际有效性由本函数栈帧纪律保证(extract_* 为同步单线程调用,
/// 回调对象全部先于 w 失效)。
struct PtrWrite(*mut (dyn Write + 'static));

impl Write for PtrWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // SAFETY:见 struct 注释
        unsafe { (*self.0).write(buf) }
    }
    fn flush(&mut self) -> io::Result<()> {
        // SAFETY:见 struct 注释
        unsafe { (*self.0).flush() }
    }
}

impl ArchiveBackend for RarBackend {
    fn format(&self) -> Format {
        Format::Unarc(ArchiveFormat::Rar)
    }

    fn next_entry(&mut self) -> anyhow::Result<Option<Entry>> {
        let Some(m) = self.members.get(self.next) else {
            self.current = None;
            return Ok(None);
        };
        let idx = self.next;
        self.next += 1;
        self.current = Some(idx);
        Ok(Some(Entry {
            name: m.name.clone(),
            kind: m.kind,
            size: m.size,
            compressed_size: m.packed,
            method: "Rar".into(),
            mtime_epoch: m.mtime,
            encrypted: m.encrypted,
            link_target: None,
            mode: m.mode,
        }))
    }

    fn read_current(&mut self) -> anyhow::Result<Vec<u8>> {
        if let Some(i) = self.current {
            self.check_size(self.members[i].size, false)?; // 整读进内存:entry 限额
        }
        let mut buf = Vec::new();
        self.read_current_to(&mut buf)?;
        Ok(buf)
    }

    /// 流式解码:回调直写 w。目标成员数据写完后,下一个成员回调即触发
    /// Cancelled 提前结束(避免后续成员继续解码进内存)。
    fn read_current_to(&mut self, w: &mut dyn Write) -> anyhow::Result<u64> {
        let idx = self.current.take().ok_or_else(|| anyhow::anyhow!("无当前条目(先 next_entry)"))?;
        let info = self.members[idx].clone();
        if info.kind != EntryKind::File {
            return Ok(0);
        }
        self.check_size(info.size, true)?;
        let pwd = self.password.as_deref().map(str::as_bytes);
        let target = info.raw_name.clone();
        let seen = Cell::new(false);
        let written = Cell::new(0u64);
        // SAFETY:把 '1 生命周期的 trait object 指针抹成 'static —— 实际有效性
        // 由调用栈帧纪律保证(extract_* 同步调用,回调对象先于 w 失效)
        let w_ptr: *mut (dyn Write + 'static) =
            unsafe { std::mem::transmute(w as *mut dyn Write) };
        let written_ptr = &written as *const Cell<u64>;
        // 'static Writer 要求:计数器也走裸指针(SAFETY 同下,栈帧内有效)
        struct CountingRaw(PtrWrite, *const Cell<u64>);
        impl Write for CountingRaw {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let n = self.0.write(buf)?;
                // SAFETY:计数 Cell 活于调用栈帧
                let c = unsafe { &*self.1 };
                c.set(c.get() + n as u64);
                Ok(n)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.0.flush()
            }
        }
        // 单卷:extract_with_control 官方 Stop 语义(正常返回,不与内部
        // rayon pipeline 的错误取消路径竞态——实证走 Error::Cancelled 会让
        // producer 持续分配导致内存失控)
        let res = if self.volumes.len() == 1 {
            let ropts = ArchiveReadOptions::with_optional_password(pwd);
            let mut decide = |member: &rars::ArchiveMember| -> Result<rars::ExtractionDecision, RarsError> {
                if !seen.get() && member.meta.name == target {
                    seen.set(true);
                    // SAFETY:同上
                    return Ok(rars::ExtractionDecision::Extract(Box::new(CountingRaw(
                        PtrWrite(w_ptr),
                        written_ptr,
                    ))));
                }
                if seen.get() {
                    return Ok(rars::ExtractionDecision::Stop);
                }
                Ok(rars::ExtractionDecision::Extract(Box::new(io::sink())))
            };
            self.volumes[0].extract_with_control(ropts, &mut decide).map(|_| ())
        } else {
            // 多卷:control API 仅单卷;退化为完整解码(目标写透,其余 sink,
            // 内存 O(1);无提前终止的额外 CPU 仅在多成员大档出现)
            let mut open = |meta: &ExtractedEntryMeta| -> Result<Box<dyn Write>, RarsError> {
                if !seen.get() && meta.name == target {
                    seen.set(true);
                    // SAFETY:同上
                    Ok(Box::new(CountingRaw(PtrWrite(w_ptr), written_ptr)))
                } else {
                    Ok(Box::new(io::sink()))
                }
            };
            rars::extract_volumes_to(&self.volumes, pwd, &mut open)
        };
        match res {
            Ok(_) => {}
            Err(e) => anyhow::bail!("rar: {e}"),
        }
        Ok(written.get())
    }

    fn skip_current(&mut self) -> anyhow::Result<()> {
        self.current = None; // 游标前进;solid 解码推迟到下次读取时按序重扫
        Ok(())
    }

    fn is_single_file(&self) -> bool {
        self.members.len() == 1 && self.members[0].kind == EntryKind::File
    }

    fn provides_mode(&self) -> bool {
        true // 头部 host_os=unix 时直填 Entry.mode
    }

    fn total_uncompressed(&self) -> Option<u64> {
        Some(self.members.iter().map(|m| m.size).sum())
    }

    fn consumed_paths(&self) -> Vec<PathBuf> {
        self.volume_paths.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rars::ArchiveVersion;
    use rars::builder::Builder as ArchiveBuilder;

    /// 造单卷 RAR50 fixture:目录 + 755 脚本 + 644 数据
    fn build_rar(path: &Path, solid: bool) {
        let mut b = ArchiveBuilder::new(ArchiveVersion::Rar50).solid(solid);
        b.add_directory(b"bin".to_vec(), Some(1_700_000_000), Some(0o755)).unwrap();
        b.add_bytes(b"bin/tool.sh".to_vec(), b"#!/bin/sh\necho ok\n".to_vec(), Some(1_700_000_000), Some(0o755)).unwrap();
        b.add_bytes(b"bin/data.bin".to_vec(), (0..1000u32).map(|i| (i % 251) as u8).collect(), Some(1_700_000_000), Some(0o644)).unwrap();
        b.write_to_path(path, None).unwrap();
    }

    fn extract_all(path: &Path) -> Vec<(String, u32, Vec<u8>)> {
        let opts = OpenOptions::default();
        let mut be = RarBackend::open(path, &opts).unwrap();
        let mut out = Vec::new();
        while let Some(e) = be.next_entry().unwrap() {
            let mut buf = Vec::new();
            let n = be.read_current_to(&mut buf).unwrap();
            assert_eq!(n, e.size, "流式写出字节数应等于条目声明大小");
            out.push((e.name, e.mode.unwrap_or(0), buf));
        }
        out
    }

    #[test]
    fn rar_stream_roundtrip_and_modes() {
        let dir = std::env::temp_dir().join(format!("anyarc_rar_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.rar");
        build_rar(&p, false);
        let got = extract_all(&p);
        assert_eq!(got.len(), 3, "目录+2文件");
        assert_eq!(got[0].0, "bin");
        assert_eq!(got[1].0, "bin/tool.sh");
        assert_eq!(got[1].1, 0o755, "unix mode 应从头解析");
        assert_eq!(got[1].2, b"#!/bin/sh\necho ok\n"); // 18 字节
        assert_eq!(got[2].1, 0o644);
        assert_eq!(got[2].2.len(), 1000);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rar_solid_stream_roundtrip() {
        let dir = std::env::temp_dir().join(format!("anyarc_rarsolid_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.rar");
        build_rar(&p, true);
        let got = extract_all(&p);
        assert_eq!(got.len(), 3);
        assert_eq!(got[1].2, b"#!/bin/sh\necho ok\n");
        assert_eq!(got[2].2.len(), 1000);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rar_totals_and_single_file_flag() {
        let dir = std::env::temp_dir().join(format!("anyarc_rarmeta_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("m.rar");
        build_rar(&p, false);
        let opts = OpenOptions::default();
        let be = RarBackend::open(&p, &opts).unwrap();
        // 目录 0 + 两文件(18 + 1000)
        assert_eq!(be.total_uncompressed(), Some(1018));
        assert!(!be.is_single_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rar_volume_pattern_collects_parts() {
        let dir = std::env::temp_dir().join(format!("anyarc_rarvol_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["a.part1.rar", "a.part2.rar", "a.part3.rar"] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        let vols = collect_rar_volumes(&dir.join("a.part1.rar"));
        assert_eq!(vols.len(), 3);
        // RAR4 老式
        for n in ["b.rar", "b.r00", "b.r01"] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        assert_eq!(collect_rar_volumes(&dir.join("b.rar")).len(), 3);
        // 单卷
        std::fs::write(dir.join("solo.rar"), b"x").unwrap();
        assert_eq!(collect_rar_volumes(&dir.join("solo.rar")).len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rar_caps_entry_vs_streaming() {
        // 语义:--max-entry-size 只挡整读进内存(read_current);
        // 流式直写(read_current_to)受 --max-total-size 约束 —— 4.84GB
        // 大条目在 7GB 机器上流式解不再被 4GiB entry cap 误杀。
        let dir = std::env::temp_dir().join(format!("anyarc_rarcap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c.rar");
        build_rar(&p, false); // tool.sh 18B / data.bin 1000B

        // entry cap=10:整读报错,流式放行
        let opts = OpenOptions { max_entry_size: Some(10), ..Default::default() };
        let mut be = RarBackend::open(&p, &opts).unwrap();
        be.next_entry().unwrap().unwrap(); // bin(dir)
        be.skip_current().unwrap();
        be.next_entry().unwrap().unwrap(); // tool.sh
        assert!(be.read_current().is_err(), "整读应受 entry cap 约束");
        let mut buf = Vec::new();
        assert!(be.read_current_to(&mut buf).is_ok(), "流式不受 entry cap 约束");
        drop(be);

        // total cap=5:流式也拦(磁盘防护)
        let opts2 = OpenOptions { max_total_size: Some(5), ..Default::default() };
        let mut be2 = RarBackend::open(&p, &opts2).unwrap();
        be2.next_entry().unwrap().unwrap();
        be2.skip_current().unwrap();
        be2.next_entry().unwrap().unwrap();
        let mut buf2 = Vec::new();
        assert!(be2.read_current_to(&mut buf2).is_err(), "流式受 total cap 约束");
        std::fs::remove_dir_all(&dir).unwrap();
    }

}

#[cfg(test)]
mod chain_tests {
    use super::volume_chain;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[test]
    fn contiguous_chain_only() {
        let m: BTreeMap<u64, PathBuf> = [
            (0, PathBuf::from("v0")),
            (1, PathBuf::from("v1")),
            (2, PathBuf::from("v2")),
            (7, PathBuf::from("lonely7")),
        ]
        .into_iter()
        .collect();
        assert_eq!(volume_chain(&m, 1), vec![PathBuf::from("v0"), PathBuf::from("v1"), PathBuf::from("v2")]);
        assert_eq!(volume_chain(&m, 7), vec![PathBuf::from("lonely7")]);
        // 中间断链:从 1 起只能取 1,2
        let m2: BTreeMap<u64, PathBuf> = [(1, "v1"), (2, "v2"), (4, "v4")]
            .into_iter()
            .map(|(n, p)| (n, PathBuf::from(p)))
            .collect();
        assert_eq!(volume_chain(&m2, 1), vec![PathBuf::from("v1"), PathBuf::from("v2")]);
        // 卷 1 传入但卷 0 存在 → 从头串起(允许从任意卷切入)
        assert_eq!(volume_chain(&m, 0).len(), 3);
    }
}

#[cfg(test)]
mod name_tests {
    use super::{part_number_in_name, volume_stem};
    use std::path::Path;

    #[test]
    fn part_number_survives_broken_extension() {
        // 用户把扩展名改坏(r删除ar),partN 序号仍可从文件名取得
        let p = Path::new("foxb31-32.part2.r删除ar");
        assert_eq!(part_number_in_name(p), Some(2));
        assert_eq!(volume_stem(p).as_deref(), Some("foxb31-32."));
        assert_eq!(part_number_in_name(Path::new("SF2.PART10.RAR")), Some(10));
        assert_eq!(part_number_in_name(Path::new("plain.rar")), None);
        assert_eq!(volume_stem(Path::new("plain.rar")), None);
    }
}
