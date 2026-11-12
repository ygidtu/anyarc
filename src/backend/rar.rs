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
use std::io::{self, BufReader, Write};
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
        let vols = collect_rar_volumes(path);
        let ropts = ArchiveReadOptions::with_optional_password(opts.password.as_deref().map(str::as_bytes));
        let mut volumes = Vec::new();
        for v in &vols {
            let f = File::open(v).map_err(|e| anyhow::anyhow!("RAR 卷 {}: {e}", v.display()))?;
            let a = ArchiveReader::read_reader_with_options(BufReader::new(f), ropts)
                .map_err(|e| map_rars_error(e, v))?;
            volumes.push(a);
        }
        let members = scan_members(&volumes);
        if members.is_empty() {
            anyhow::bail!("RAR: 未解析到任何条目({})", path.display());
        }
        Ok(Self {
            volumes,
            members,
            password: opts.password.clone(),
            next: 0,
            current: None,
            max_entry_size: opts.max_entry_size,
        })
    }

    fn check_size(&self, size: u64) -> anyhow::Result<()> {
        if let Some(cap) = self.max_entry_size
            && size > cap
        {
            anyhow::bail!("条目解压尺寸 {size} 超过上限 {cap}(--max-entry-size)");
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
        self.check_size(info.size)?;
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
}
