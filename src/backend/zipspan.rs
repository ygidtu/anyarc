//! Info-ZIP 风格分卷 zip(.z01…+.zip 末卷)后端。
//!
//! zip crate(含经 unarc 的间接使用)明确拒绝 multi-disk EOCD。这里自建
//! "虚拟串联流":按 z01→z02→…→zip 顺序把各卷拼成单一逻辑文件,并把
//! EOCD / ZIP64-EOCD / CD 条目中的"磁盘号"字段原地打补丁清零 —— 等价于
//! `unzip` 警告的 "assuming all parts have been concatenated" 语义。
//! 数据读取走 zip crate 流式 reader,大条目内存 O(块窗口)。

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::backend::{ArchiveBackend, Entry, EntryKind, Format, OpenOptions};

/// 虚拟串联读:跨卷定位 + patch 替换(按虚拟偏移打洞改写字节)
pub struct SpannedReader {
    vols: Vec<(File, u64, u64)>, // (句柄, 虚拟起始, 长度)
    total: u64,
    pos: u64,
    patches: Vec<(u64, Vec<u8>)>, // 按偏移升序、互不重叠
}

impl SpannedReader {
    fn vol_at(&self, pos: u64) -> Option<usize> {
        self.vols.iter().position(|(_, s, l)| pos >= *s && pos < s + l)
    }
}

impl Read for SpannedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.total {
            return Ok(0);
        }
        let vi = self
            .vol_at(self.pos)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "spanned gap"))?;
        let (vstart, vlen) = (self.vols[vi].1, self.vols[vi].2);
        let in_vol = self.pos - vstart;
        let room = vlen - in_vol;
        let n = std::cmp::min(buf.len() as u64, room) as usize;
        let pos0 = self.pos;
        let f = &mut self.vols[vi].0;
        f.seek(SeekFrom::Start(in_vol))?;
        let got = f.read(&mut buf[..n])?;
        // patch 覆盖(补丁数量小,线性匹配足够)
        for (poff, bytes) in &self.patches {
            let pend = poff + bytes.len() as u64;
            let lo = std::cmp::max(*poff, pos0);
            let hi = std::cmp::min(pend, pos0 + got as u64);
            for a in lo..hi {
                buf[(a - pos0) as usize] = bytes[(a - poff) as usize];
            }
        }
        self.pos = pos0 + got as u64;
        Ok(got)
    }
}

impl Seek for SpannedReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let np = match to {
            SeekFrom::Start(p) => p,
            SeekFrom::End(d) => (self.total as i64 + d).max(0) as u64,
            SeekFrom::Current(d) => (self.pos as i64 + d).max(0) as u64,
        };
        self.pos = np;
        Ok(np)
    }
}

impl SpannedReader {
    fn read_at(&mut self, off: u64, n: usize) -> io::Result<Vec<u8>> {
        self.seek(SeekFrom::Start(off))?;
        let mut v = vec![0u8; n];
        self.read_exact(&mut v)?;
        Ok(v)
    }
}

pub struct ZipSpanBackend {
    archive: zip::ZipArchive<BufReader<SpannedReader>>,
    members: Vec<Entry>,
    next: usize,
    current: Option<usize>,
    password: Option<String>,
    max_entry_size: Option<u64>,
    max_total_size: Option<u64>,
    volume_paths: Vec<PathBuf>,
}

const EOCD_MAGIC: &[u8; 4] = b"PK\x05\x06";
const Z64_LOCATOR_MAGIC: &[u8; 4] = b"PK\x06\x07";
const Z64_EOCD_MAGIC: &[u8; 4] = b"PK\x06\x06";

/// 在虚拟流尾部窗口找 EOCD 绝对偏移
fn find_eocd(r: &mut SpannedReader) -> io::Result<Option<u64>> {
    let total = r.total;
    if total < 22 {
        return Ok(None);
    }
    let win = std::cmp::min(total, 1024 * 1024);
    let start = total - win;
    let tail = r.read_at(start, win as usize)?;
    let mut j = tail.len().saturating_sub(4);
    loop {
        if &tail[j..j + 4] == EOCD_MAGIC && j + 22 <= tail.len() {
            let comment_len = u16::from_le_bytes([tail[j + 20], tail[j + 21]]) as usize;
            if j + 22 + comment_len == tail.len() {
                return Ok(Some(start + j as u64));
            }
        }
        if j == 0 {
            return Ok(None);
        }
        j -= 1;
    }
}

/// 打开分卷 zip:串联 + 磁盘字段清零补丁
fn zdbg(msg: &str) {
    if std::env::var_os("ANYARC_DEBUG").is_some() {
        eprintln!("[zipspan] {msg}");
    }
}

pub fn open(vols: &[PathBuf], opts: &OpenOptions) -> anyhow::Result<ZipSpanBackend> {
    anyhow::ensure!(vols.len() > 1, "分卷 zip 需要至少两个卷文件");
    zdbg(&format!("vols={:?}", vols.iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy()).collect::<Vec<_>>()));
    let mut files = Vec::new();
    let mut off = 0u64;
    for p in vols {
        let f = File::open(p).map_err(|e| anyhow::anyhow!("分卷 {}: {e}", p.display()))?;
        let len = f.metadata()?.len();
        files.push((f, off, len));
        off += len;
    }
    let mut r = SpannedReader { vols: files, total: off, pos: 0, patches: Vec::new() };

    zdbg("reader built, finding EOCD...");
    let eocd = find_eocd(&mut r)?.ok_or_else(|| anyhow::anyhow!("分卷 zip:尾部找不到 EOCD(下载不完整或卷序不对)"))?;
    let e = r.read_at(eocd, 22)?;
    let mut cd_size = u32::from_le_bytes(e[12..16].try_into()?) as u64;
    let mut cd_off = u32::from_le_bytes(e[16..20].try_into()?) as u64;
    let mut entries = u16::from_le_bytes(e[10..12].try_into()?) as u64;

    // 补丁:EOCD 磁盘号(this @+4 / cd-start @+6,u16×2 → 4B 清零);
    // this-disk 条目数(@+8 u16)对齐 total(@+10)
    r.patches.push((eocd + 4, vec![0u8; 4]));
    r.patches.push((eocd + 8, e[10..12].to_vec()));

    // ZIP64:哨兵 → locator → zip64 EOCD。
    // 偏移字段约定不一(WinRAR=虚拟绝对;Info-ZIP zip-s=本卷(末卷)相对):
    // 多解释尝试,以 zip64 EOCD 魔数命中为准。
    if cd_off == 0xFFFF_FFFF || cd_size == 0xFFFF_FFFF || entries == 0xFFFF {
        anyhow::ensure!(eocd >= 20, "分卷 zip:文件过短");
        let loc = r.read_at(eocd - 20, 20)?;
        if &loc[0..4] == Z64_LOCATOR_MAGIC {
            // locator:disk_with_cd(u32@+4)与 number_of_disks(u32@+16)清零
            r.patches.push((eocd - 20 + 4, vec![0u8; 4]));
            r.patches.push((eocd - 20 + 16, vec![0u8; 4]));
            let z64_field = u64::from_le_bytes(loc[8..16].try_into()?);
            let tail_start = r.vols.last().map(|(_, s, _)| *s).unwrap_or(0);
            // 候选:虚拟绝对 / 末卷相对
            let mut z64_abs = None;
            for cand in [z64_field, tail_start + z64_field] {
                if cand + 4 <= r.total && r.read_at(cand, 4)? == Z64_EOCD_MAGIC {
                    z64_abs = Some(cand);
                    break;
                }
            }
            if let Some(z64p) = z64_abs {
                let z = r.read_at(z64p, 56)?;
                // zip64 EOCD:this disk(@16)+ cd start disk(@20)清零;
                // files-on-this-disk(@24)对齐 files-total(@32)
                r.patches.push((z64p + 16, vec![0u8; 8]));
                r.patches.push((z64p + 24, z[32..40].to_vec()));
                // 关键:locator 的 z64 记录偏移(@+8)与 zip64 EOCD 的 cd 偏移(@+48)
                // 在 Info-ZIP 分卷里按"本卷相对"写,zip crate 按虚拟绝对 seek 会
                // 走错位置 → 原地改写为虚拟绝对偏移
                r.patches.push((eocd - 20 + 8, z64p.to_le_bytes().to_vec()));
                entries = u64::from_le_bytes(z[32..40].try_into()?);
                cd_size = u64::from_le_bytes(z[40..48].try_into()?);
                let cd_field = u64::from_le_bytes(z[48..56].try_into()?);
                // CD 规范位置:紧跟 Central Directory 的下一记录就是 zip64 EOCD
                let structural = z64p.saturating_sub(cd_size);
                if structural + 4 <= r.total && r.read_at(structural, 4)? == *b"PK\x01\x02" {
                    cd_off = structural;
                } else {
                    let mut found = None;
                    for cand in [cd_field, tail_start + cd_field] {
                        if cand + 4 <= r.total && r.read_at(cand, 4)? == *b"PK\x01\x02" {
                            found = Some(cand);
                            break;
                        }
                    }
                    cd_off = found.unwrap_or(structural);
                }
                r.patches.push((z64p + 48, cd_off.to_le_bytes().to_vec()));
            }
        }
    }
    zdbg(&format!("eocd@{eocd} cd_off={cd_off} cd_size={cd_size} entries={entries} total={}", r.total));
    anyhow::ensure!(
        cd_off + cd_size <= r.total,
        "分卷 zip:中央目录越界(缺卷或顺序不对?期望 {}/{},共 {})",
        cd_off,
        cd_off + cd_size,
        r.total
    );

    // CD 内每个条目的 disk start(u16 @+34)清零(分卷条目会记录所在盘号)
    {
        let cd = r.read_at(cd_off, cd_size as usize)?;
        let mut i = 0usize;
        let mut n = 0u64;
        while i + 46 <= cd.len() && n < entries {
            if &cd[i..i + 4] != b"PK\x01\x02" {
                break;
            }
            if u16::from_le_bytes([cd[i + 34], cd[i + 35]]) != 0 {
                r.patches.push((cd_off + i as u64 + 34, vec![0u8; 2]));
            }
            let name_len = u16::from_le_bytes([cd[i + 28], cd[i + 29]]) as usize;
            let extra_len = u16::from_le_bytes([cd[i + 30], cd[i + 31]]) as usize;
            let cmt_len = u16::from_le_bytes([cd[i + 32], cd[i + 33]]) as usize;
            i += 46 + name_len + extra_len + cmt_len;
            n += 1;
        }
    }
    r.patches.sort_by_key(|(o, _)| *o);

    zdbg(&format!("CD scanned, patches={}, opening zip crate archive...", r.patches.len()));
    let archive = zip::ZipArchive::new(BufReader::with_capacity(256 * 1024, r))
        .map_err(|e| anyhow::anyhow!("分卷 zip 解析失败(可能缺卷/下载不完整): {e}"))?;
    zdbg(&format!("archive.len={}", archive.len()));
    // 元数据走 CD(by_index_data):不触 local header、不解密 —— 密码提示流程
    // (scan_encrypted)才能先于读取看到 encrypted 标志
    let members: Vec<Entry> = (0..archive.len())
        .filter_map(|i| {
            let fd = match archive.by_index_data(i) {
                Ok(fd) => fd,
                Err(e) => {
                    zdbg(&format!("by_index_data({i}) 失败: {e}"));
                    return None;
                }
            };
            let name = match fd.name() {
                Ok(n) => n.into_owned(),
                Err(e) => {
                    zdbg(&format!("name({i}) 失败: {e}"));
                    return None;
                }
            };
            Some(Entry {
                name,
                kind: if fd.is_dir() { EntryKind::Directory } else { EntryKind::File },
                size: fd.size(),
                compressed_size: fd.compressed_size(),
                method: format!("{:?}", fd.compression()),
                mtime_epoch: None, // zip::DateTime 无公开分解 API;extract 也不还原 mtime
                encrypted: fd.encrypted(),
                link_target: None,
                mode: fd.unix_mode(),
            })
        })
        .collect();
    zdbg(&format!("zip crate ok, members={}", members.len()));
    let volume_paths = vols
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();
    Ok(ZipSpanBackend {
        archive,
        members,
        next: 0,
        current: None,
        password: opts.password.clone(),
        max_entry_size: opts.max_entry_size,
        max_total_size: opts.max_total_size,
        volume_paths,
    })
}

impl ArchiveBackend for ZipSpanBackend {
    fn format(&self) -> Format {
        Format::Unarc(unarc_rs::unified::ArchiveFormat::Zip)
    }

    fn next_entry(&mut self) -> anyhow::Result<Option<Entry>> {
        let Some(m) = self.members.get(self.next) else {
            self.current = None;
            return Ok(None);
        };
        let idx = self.next;
        self.next += 1;
        self.current = Some(idx);
        Ok(Some(m.clone()))
    }

    fn read_current(&mut self) -> anyhow::Result<Vec<u8>> {
        if let (Some(i), Some(cap)) = (self.current, self.max_entry_size)
            && self.members[i].size > cap
        {
            anyhow::bail!("单个文件太大:解压后 {} 字节超过上限 {cap}(可用 --max-entry-size 调整,0=不限)", self.members[i].size);
        }
        let mut buf = Vec::new();
        self.read_current_to(&mut buf)?;
        Ok(buf)
    }

    fn read_current_to(&mut self, w: &mut dyn io::Write) -> anyhow::Result<u64> {
        let idx = self.current.ok_or_else(|| anyhow::anyhow!("无当前条目(先 next_entry)"))?;
        let info = self.members[idx].clone();
        let cap = self.max_total_size.unwrap_or(u64::MAX);
        if info.size > cap {
            anyhow::bail!("单个文件太大:解压后 {} 字节超过上限 {cap}(可用 --max-total-size 调整,0=不限)", info.size);
        }
        if info.kind != EntryKind::File {
            return Ok(0);
        }
        let rd: Box<dyn Read> = if info.encrypted {
            let pwd = self
                .password
                .clone()
                .ok_or_else(|| anyhow::anyhow!("加密条目 {},需要密码", info.name))?;
            Box::new(
                self.archive
                    .by_index_decrypt(idx, pwd.as_bytes())
                    .map_err(|e| anyhow::anyhow!("读取条目 {} 失败:{e}(分卷结构可能异常,如制作工具写坏偏移)", info.name))?,
            )
        } else {
            Box::new(
                self.archive
                    .by_index(idx)
                    .map_err(|e| anyhow::anyhow!("读取条目 {} 失败:{e}(分卷结构可能异常,如制作工具写坏偏移)", info.name))?,
            )
        };
        crate::container::copy_chunks(rd, w)
    }

    fn skip_current(&mut self) -> anyhow::Result<()> {
        self.current = None;
        Ok(())
    }

    fn provides_mode(&self) -> bool {
        true
    }

    fn total_uncompressed(&self) -> Option<u64> {
        Some(self.members.iter().map(|e| e.size).sum())
    }

    fn consumed_paths(&self) -> Vec<PathBuf> {
        self.volume_paths.clone()
    }
}
