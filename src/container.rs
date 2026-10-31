//! M2 自写容器后端:ar / deb / rpm / cpio / iso9660。
//!
//! 与 7z 对拍语义(见 PLAN §5 注记):
//! - ar:全部成员原样;
//! - deb:只暴露 `data.tar`(取 `data.tar.*` 成员,按内层压缩解压并剥扩展名);
//! - rpm:payload 解压为单条 `{NAME-VER-REL.ARCH}.cpio`;
//! - cpio:目录树(newc 070701/070702);
//! - iso:ISO9660 目录树(含 Joliet 名字),条目按存储名(去 `;1`)。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::backend::{ArchiveBackend, Entry, EntryKind, Format, OpenOptions};
use crate::detect::{Container, Detected};

/// 条目计划总量封顶(防御畸形/模糊测试档造成的内存爆炸与环路)。
const MAX_ENTRIES: usize = 1 << 20;

/// 数据取回描述:读时才打开/解压,避免大档驻留内存。
enum Fetch {
    /// 文件内原始字节区间
    Raw { off: u64, len: u64 },
    /// 压缩成员:起点 + 压缩格式(解压后返回)
    Comp { off: u64, len: u64, comp: Comp },
    /// 已内联的数据(如 symlink 目标)
    Inline(Vec<u8>),
}

#[derive(Debug, Clone, Copy)]
pub enum Comp {
    Gzip,
    Bzip2,
    Xz,
    Zstd,
    LzmaAlone,
    Lzip,
    Lz4,
}

pub struct ContainerBackend {
    fmt: Format,
    path: PathBuf,
    plan: Vec<(Entry, Fetch)>,
    next: usize,
    current: Option<usize>,
    max_entry_size: Option<u64>,
}

impl ContainerBackend {
    fn new(fmt: Format, path: &Path, plan: Vec<(Entry, Fetch)>, max_entry_size: Option<u64>) -> Self {
        Self { fmt, path: path.to_path_buf(), plan, next: 0, current: None, max_entry_size }
    }
    /// 条目大小硬上限:先对文件尺寸,再对解压限额。
    fn check_size(&self, len: u64) -> anyhow::Result<()> {
        if let Some(m) = self.max_entry_size
            && len > m
        {
            anyhow::bail!("条目解压尺寸 {len} 超过上限 {m}(--max-entry-size)");
        }
        Ok(())
    }
}

impl ArchiveBackend for ContainerBackend {
    fn format(&self) -> Format {
        self.fmt
    }
    fn next_entry(&mut self) -> anyhow::Result<Option<Entry>> {
        if self.next >= self.plan.len() {
            return Ok(None);
        }
        let i = self.next;
        self.next += 1;
        self.current = Some(i);
        Ok(Some(self.plan[i].0.clone()))
    }
    fn read_current(&mut self) -> anyhow::Result<Vec<u8>> {
        let i = self.current.take().ok_or_else(|| anyhow::anyhow!("无当前条目"))?;
        let flen = std::fs::metadata(&self.path)?.len();
        match &self.plan[i].1 {
            Fetch::Inline(b) => Ok(b.clone()),
            Fetch::Raw { off, len } => {
                if *off > flen || *len > flen - *off {
                    anyhow::bail!("条目数据超出文件范围");
                }
                self.check_size(*len)?;
                let mut f = File::open(&self.path)?;
                f.seek(SeekFrom::Start(*off))?;
                let mut buf = vec![0u8; *len as usize];
                f.read_exact(&mut buf)?;
                Ok(buf)
            }
            Fetch::Comp { off, len, comp } => {
                if *off > flen || *len > flen - *off {
                    anyhow::bail!("条目数据超出文件范围");
                }
                let mut f = File::open(&self.path)?;
                f.seek(SeekFrom::Start(*off))?;
                let mut raw = vec![0u8; *len as usize];
                f.read_exact(&mut raw)?;
                let cap = self.max_entry_size.unwrap_or(u64::MAX);
                let out = decompress(*comp, &raw, cap)?;
                self.check_size(out.len() as u64)?;
                Ok(out)
            }
        }
    }
    fn skip_current(&mut self) -> anyhow::Result<()> {
        self.current = None; // 随机访问后端:跳过即释放游标
        Ok(())
    }
}

pub(crate) fn decompress(comp: Comp, raw: &[u8], cap: u64) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    match comp {
        Comp::Gzip => {
            let mut d = flate2::read::GzDecoder::new(raw).take(cap + 1);
            d.read_to_end(&mut out)?;
        }
        Comp::Bzip2 => {
            let mut d = bzip2::read::BzDecoder::new(raw).take(cap + 1);
            d.read_to_end(&mut out)?;
        }
        Comp::Zstd => {
            let mut d = zstd::stream::read::Decoder::new(raw)?.take(cap + 1);
            d.read_to_end(&mut out)?;
        }
        Comp::Xz => {
            // 借用 unarc 的纯 Rust XZ 解码器(带限额)
            let cur = std::io::Cursor::new(raw.to_vec());
            let mut a = unarc_rs::xz::XzArchive::new(cur).map_err(|e| anyhow::anyhow!("{e}"))?;
            out = a.read_with_limit(Some(cap)).map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Comp::LzmaAlone => {
            let mut r = std::io::BufReader::new(std::io::Cursor::new(raw));
            let mut cw = Capped { out: &mut out, cap, overflow: false };
            if let Err(e) = lzma_rs::lzma_decompress(&mut r, &mut cw) {
                if cw.overflow {
                    anyhow::bail!("解压尺寸超过上限 {cap}(--max-entry-size)");
                }
                anyhow::bail!("lzma: {e}");
            }
        }
        Comp::Lz4 => {
            let mut d = lz4_flex::frame::FrameDecoder::new(std::io::Cursor::new(raw)).take(cap.saturating_add(1));
            d.read_to_end(&mut out)?;
        }
        Comp::Lzip => {
            // 标准 lzip 容器解析走 lzma-rust2(tukaani xz-java 移植,Apache-2.0;
            // 与 sevenz-rust2 同源)。手写 LEB varint 会把 payload props 字节并入
            // bds 读数(实测 fixture 失败),故弃用。
            let mut d = lzma_rust2::LzipReader::new(std::io::Cursor::new(raw)).take(cap + 1);
            d.read_to_end(&mut out)?;
        }
    }
    if out.len() as u64 > cap {
        anyhow::bail!("解压尺寸超过上限 {cap}(--max-entry-size)");
    }
    Ok(out)
}

/// detect 已确定容器种类,这里分派解析器。
pub fn open(path: &Path, det: &Detected, opts: &OpenOptions) -> anyhow::Result<Box<dyn ArchiveBackend>> {
    let container = match det.kind {
        crate::detect::Known::Container(c) => c,
        _ => unreachable!("container::open 只处理 Container"),
    };
    let data = std::fs::read(path)?;
    let plan: Vec<(Entry, Fetch)> = match container {
        Container::Ar => {
            parse_ar(&data)?.into_iter().map(|(e, off, len)| (e, Fetch::Raw { off, len })).collect()
        }
        Container::Deb => parse_deb(&data)?,
        Container::Rpm => parse_rpm(&data)?,
        Container::Cpio => parse_cpio(&data)?,
        Container::Iso => parse_iso(&data)?,
        // 单流压缩:先整体解压(限额),再判定是 tar 复合还是单条目
        Container::LzmaAlone | Container::Lzip | Container::Lz4 => {
            let comp = match container {
                Container::LzmaAlone => Comp::LzmaAlone,
                Container::Lzip => Comp::Lzip,
                _ => Comp::Lz4,
            };
            let cap = opts.max_entry_size.unwrap_or(u64::MAX);
            let bytes = decompress(comp, &data, cap)?;
            if looks_tar(&bytes) {
                plan_tar(&bytes)?
            } else {
                let name = opts
                    .single_file_name
                    .clone()
                    .or_else(|| single_stem(path))
                    .unwrap_or_else(|| "output".into());
                vec![(
                    Entry {
                        name,
                        kind: EntryKind::File,
                        size: bytes.len() as u64,
                        compressed_size: data.len() as u64,
                        method: comp_label(comp).into(),
                        mtime_epoch: None,
                        encrypted: false,
                        link_target: None,
                    },
                    Fetch::Inline(bytes),
                )]
            }
        }
    };
    let fmt = match container {
        Container::Ar => Format::Ar,
        Container::Deb => Format::Deb,
        Container::Rpm => Format::Rpm,
        Container::Cpio => Format::Cpio,
        Container::Iso => Format::Iso,
        Container::LzmaAlone => Format::LzmaAlone,
        Container::Lzip => Format::Lzip,
        Container::Lz4 => Format::Lz4,
    };
    Ok(Box::new(ContainerBackend::new(fmt, path, plan, opts.max_entry_size)))
}

// ------------------------------------------------------------------ ar

/// 解析 GNU/BSD ar:返回 (Entry 草稿, 数据偏移, 长度)。跳过符号表/长名表。
fn parse_ar(data: &[u8]) -> anyhow::Result<Vec<(Entry, u64, u64)>> {
    if data.len() < 8 || &data[..8] != b"!<arch>\n" {
        anyhow::bail!("ar: 魔数不符");
    }
    let mut pos = 8usize;
    let mut longnames: Option<Vec<u8>> = None;
    let mut out = Vec::new();
    while pos + 60 <= data.len() {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        let hdr = &data[pos..pos + 60];
        if &hdr[58..60] != b"`\n" {
            break; // 头损坏:按 7z 的宽容策略停在最后一个合法成员
        }
        let raw_name = &hdr[0..16];
        let mtime = ascii_num(&hdr[16..28]).unwrap_or(0);
        let mode = ascii_num_oct(&hdr[40..48]).unwrap_or(0o644);
        let size = ascii_num(&hdr[48..58]).unwrap_or(0) as usize;
        let body = pos + 60;
        if body + size > data.len() {
            anyhow::bail!("ar: 成员数据越界");
        }

        // 名称解析(GNU "//" 表 + BSD "#1/len")
        let (name, name_extra): (String, usize) = if raw_name.starts_with(b"//") {
            longnames = Some(data[body..body + size].to_vec());
            (String::new(), 0)
        } else if raw_name.starts_with(b"/") || raw_name.starts_with(b"__.SYMDEF") {
            (String::new(), 0) // 符号表:跳过
        } else if let Some((n, off)) = raw_name
            .iter()
            .position(|&c| c == b'/')
            .filter(|&i| i + 1 < 16)
            .map(|i| (&raw_name[..i], &raw_name[i + 1..]))
            .filter(|(_, off)| off.first().is_some_and(|c| c.is_ascii_digit()))
        {
            let _ = n;
            let table = longnames.as_ref().ok_or_else(|| anyhow::anyhow!("ar: GNU 长名表缺失"))?;
            let off: usize = std::str::from_utf8(off)
                .unwrap_or("")
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("ar: 长名偏移无效"))?;
            if off >= table.len() {
                anyhow::bail!("ar: 长名偏移越界");
            }
            let end = table[off..].iter().position(|&c| c == b'/' || c == b'\n').map(|e| off + e).unwrap_or(table.len());
            (String::from_utf8_lossy(&table[off..end]).into_owned(), 0)
        } else if raw_name.starts_with(b"#1/") {
            // BSD "#1/len":名字内联在数据前部
            let len: usize = std::str::from_utf8(&raw_name[3..])
                .unwrap_or("")
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .map_err(|_| anyhow::anyhow!("ar: BSD 长名长度无效"))?;
            if len > size {
                anyhow::bail!("ar: BSD 内联名长度超出成员数据");
            }
            let n = String::from_utf8_lossy(&data[body..body + len]).trim_end().to_string();
            (n, len)
        } else {
            (String::from_utf8_lossy(raw_name).trim_end().trim_end_matches('/').to_string(), 0)
        };

        if !name.is_empty() {
            let kind = if mode & 0o170000 == 0o120000 {
                EntryKind::Symlink
            } else if mode & 0o170000 == 0o040000 {
                EntryKind::Directory
            } else {
                EntryKind::File
            };
            out.push((
                Entry {
                    name,
                    kind,
                    size: (size - name_extra) as u64,
                    compressed_size: (size - name_extra) as u64,
                    method: "Stored".into(),
                    mtime_epoch: (mtime > 0).then_some(mtime),
                    encrypted: false,
                    link_target: None,
                },
                (body + name_extra) as u64,
                (size - name_extra) as u64,
            ));
        }
        pos = body + size + (size % 2);
    }
    Ok(out)
}

// ------------------------------------------------------------------ deb

fn parse_deb(data: &[u8]) -> anyhow::Result<Vec<(Entry, Fetch)>> {
    // 与 7z 对拍:只暴露 data.tar(.*) → 解压为 "data.tar"
    let members = parse_ar_with_offsets(data)?;
    for (name, off, len) in members {
        match deb_data_comp(&name) {
            Some(fetch) => {
                let method = match fetch {
                    None => "Stored",
                    Some(comp) => comp_label(comp),
                };
                let entry = Entry {
                    name: "data.tar".into(),
                    kind: EntryKind::File,
                    size: 0,
                    compressed_size: len,
                    method: method.into(),
                    mtime_epoch: None,
                    encrypted: false,
                    link_target: None,
                };
                let fetch = match fetch {
                    None => Fetch::Raw { off, len },
                    Some(comp) => Fetch::Comp { off, len, comp },
                };
                return Ok(vec![(entry, fetch)]);
            }
            None => continue,
        }
    }
    anyhow::bail!("deb: 未找到 data.tar.* 成员")
}

/// 命中 `data.tar(.gz|.bz2|.xz|.zst)` 时返回 Some(压缩格式,None=原样)。
fn deb_data_comp(name: &str) -> Option<Option<Comp>> {
    let n = name.trim_end_matches('/');
    let Some(rest) = n.strip_prefix("data.tar") else { return None };
    match rest {
        "" => Some(None),
        ".gz" => Some(Some(Comp::Gzip)),
        ".bz2" => Some(Some(Comp::Bzip2)),
        ".xz" => Some(Some(Comp::Xz)),
        ".zst" => Some(Some(Comp::Zstd)),
        _ => None,
    }
}

/// ar 成员清单(名, 数据偏移, 长度),不区分特殊成员。
fn parse_ar_with_offsets(data: &[u8]) -> anyhow::Result<Vec<(String, u64, u64)>> {
    let mut pos = 8usize;
    let mut out = Vec::new();
    while pos + 60 <= data.len() {
        let hdr = &data[pos..pos + 60];
        if &hdr[58..60] != b"`\n" {
            break;
        }
        let name = String::from_utf8_lossy(&hdr[0..16]).trim_end().to_string();
        let size = ascii_num(&hdr[48..58]).unwrap_or(0);
        out.push((name, (pos + 60) as u64, size));
        pos = pos + 60 + size as usize + (size as usize % 2);
    }
    Ok(out)
}

// ------------------------------------------------------------------ rpm

fn parse_rpm(data: &[u8]) -> anyhow::Result<Vec<(Entry, Fetch)>> {
    if data.len() < 96 || &data[..4] != b"\xed\xab\xee\xdb" {
        anyhow::bail!("rpm: 魔数不符");
    }
    let mut pos = 96usize;
    // 可选 signature header:同 magic,若其后(8 对齐处)还有第二个 header 区,则本区为 signature
    if data.len() >= pos + 20 && &data[pos..pos + 3] == b"\x8e\xad\xe8" {
        let (n1, h1) = read_region(data, pos)?;
        let next = (pos + 16 + 16 * n1 + h1 + 7) & !7;
        if data.len() >= next + 3 && &data[next..next + 3] == b"\x8e\xad\xe8" {
            pos = next;
        }
    }
    if data.len() < pos + 16 || &data[pos..pos + 3] != b"\x8e\xad\xe8" || data[pos + 3] != 1 {
        anyhow::bail!("rpm: 缺少主 header");
    }
    let (nindex, hsize) = read_region(data, pos)?;
    let index = &data[pos + 16..pos + 16 + 16 * nindex];
    let store_off = pos + 16 + 16 * nindex;
    let store = &data[store_off..store_off + hsize];
    let payload_off = store_off + hsize;

    let get_str = |tag: u32| -> Option<String> {
        for i in 0..nindex {
            let e = &index[i * 16..i * 16 + 16];
            if u32::from_be_bytes(e[0..4].try_into().unwrap()) == tag {
                let off = u32::from_be_bytes(e[8..12].try_into().unwrap()) as usize;
                if off >= store.len() {
                    return None;
                }
                let end = store[off..].iter().position(|&c| c == 0).map(|p| off + p).unwrap_or(store.len());
                return Some(String::from_utf8_lossy(&store[off..end]).into_owned());
            }
        }
        None
    };

    let comp = match &data[payload_off..] {
        p if p.starts_with(b"\x1f\x8b") => Comp::Gzip,
        p if p.starts_with(b"BZh") => Comp::Bzip2,
        p if p.starts_with(b"\xfd7zXZ\x00") => Comp::Xz,
        p if p.starts_with(b"\x28\xb5\x2f\xfd") => Comp::Zstd,
        _ => anyhow::bail!("rpm: 未知 payload 压缩格式"),
    };
    let n = get_str(1000).unwrap_or_default();
    let v = get_str(1001).unwrap_or_default();
    let r = get_str(1002).unwrap_or_default();
    let a = get_str(1022).unwrap_or_else(|| "noarch".into());
    let entry = Entry {
        name: format!("{n}-{v}-{r}.{a}.cpio"),
        kind: EntryKind::File,
        size: 0,
        compressed_size: (data.len() - payload_off) as u64,
        method: format!("{comp:?}"),
        mtime_epoch: None,
        encrypted: false,
        link_target: None,
    };
    Ok(vec![(entry, Fetch::Comp { off: payload_off as u64, len: (data.len() - payload_off) as u64, comp })])
}

/// header 区:返回 (index 条数, store 字节数);校验 magic。
fn read_region(data: &[u8], at: usize) -> anyhow::Result<(usize, usize)> {
    let h = &data[at..at + 16];
    let nindex = u32::from_be_bytes(h[8..12].try_into().unwrap()) as usize;
    let hsize = u32::from_be_bytes(h[12..16].try_into().unwrap()) as usize;
    if at + 16 + 16 * nindex + hsize > data.len() {
        anyhow::bail!("rpm: header 区越界");
    }
    Ok((nindex, hsize))
}

// ------------------------------------------------------------------ cpio

fn parse_cpio(data: &[u8]) -> anyhow::Result<Vec<(Entry, Fetch)>> {
    if data.starts_with(b"070701") || data.starts_with(b"070702") {
        return parse_cpio_newc(data);
    }
    // 二进制 cpio:0707(bin)/ 070707(hpbin),16 位字段 LE/BE
    match data[..2] {
        [0xC7, 0x01] => parse_cpio_bin(data, false, false),   // 0x01C7 LE
        [0x01, 0xC7] => parse_cpio_bin(data, true, false),    // BE
        [0xC7, 0x71] => parse_cpio_bin(data, false, true),    // 0x71C7 LE
        [0x71, 0xC7] => parse_cpio_bin(data, true, true),     // BE
        _ => anyhow::bail!("cpio: 魔数不符(newc/bin/hpbin 之外不支持)"),
    }
}

/// newc/070702 ASCII 头。
fn parse_cpio_newc(data: &[u8]) -> anyhow::Result<Vec<(Entry, Fetch)>> {
    if data.len() < 110 {
        anyhow::bail!("cpio: 文件过短");
    }
    let hex = |s: &[u8]| u32::from_str_radix(std::str::from_utf8(s).unwrap_or("0"), 16).unwrap_or(0);
    let mut pos = 0usize;
    let mut out = Vec::new();
    while pos + 110 <= data.len() && &data[pos..pos + 6] == b"0707" {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        let mode = hex(&data[pos + 14..pos + 22]);
        let mtime = hex(&data[pos + 46..pos + 54]);
        let mut filesize = hex(&data[pos + 54..pos + 62]) as u64;
        let namesize = hex(&data[pos + 86..pos + 94]) as usize;
        if namesize == 0 || pos + 110 + namesize > data.len() {
            anyhow::bail!("cpio: 名字长度无效");
        }
        let chksum = hex(&data[pos + 94..pos + 102]);
        let mut name = String::from_utf8_lossy(&data[pos + 110..pos + 110 + namesize - 1]).into_owned();
        let doff = align4(pos + 110 + namesize);
        if filesize == 0xFFFFFFFF {
            filesize = chksum as u64; // newc CRC 模式(070702):c_filesize 在 c_chksum
        }
        if doff + filesize as usize > data.len() {
            anyhow::bail!("cpio: 数据越界");
        }
        if name == "TRAILER!!!" {
            break;
        }
        name = name.trim_start_matches("./").to_string();
        let ifmt = mode & 0o170000;
        let kind = match ifmt {
            0o040000 => EntryKind::Directory,
            0o120000 => EntryKind::Symlink,
            0o100000 => EntryKind::File,
            _ => EntryKind::Special,
        };
        let fetch = if kind == EntryKind::Symlink {
            Fetch::Inline(data[doff..doff + filesize as usize].to_vec())
        } else if kind == EntryKind::File {
            Fetch::Raw { off: doff as u64, len: filesize }
        } else {
            Fetch::Inline(Vec::new())
        };
        out.push((
            Entry {
                name,
                kind,
                size: if kind == EntryKind::File { filesize } else { 0 },
                compressed_size: filesize,
                method: "Stored".into(),
                mtime_epoch: (mtime > 0).then_some(mtime as u64),
                encrypted: false,
                link_target: if kind == EntryKind::Symlink {
                    Some(String::from_utf8_lossy(&data[doff..doff + filesize as usize]).into_owned())
                } else {
                    None
                },
            },
            fetch,
        ));
        pos = align4(doff + filesize as usize);
    }
    Ok(out)
}

fn align4(x: usize) -> usize {
    (x + 3) & !3
}

/// 二进制 cpio(bin 0707 / hpbin 070707):26 字节头,16 位字段;
/// 32 位字段以两个 16 位字(高字在前,各字按 endianness)存放。
fn parse_cpio_bin(data: &[u8], be: bool, hp: bool) -> anyhow::Result<Vec<(Entry, Fetch)>> {
    let want = if hp { 0o70707u16 } else { 0o0707u16 };
    let u16_at = |p: usize| -> u16 {
        if be {
            u16::from_be_bytes([data[p], data[p + 1]])
        } else {
            u16::from_le_bytes([data[p], data[p + 1]])
        }
    };
    let mut pos = 0usize;
    let mut out = Vec::new();
    while pos + 26 <= data.len() {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        if u16_at(pos) != want {
            break; // 头损坏:停在最后一个合法条目
        }
        let mode = u16_at(pos + 6);
        let nlink = u16_at(pos + 12);
        let mtime = (u32::from(u16_at(pos + 16)) << 16) | u32::from(u16_at(pos + 18));
        let namesize = u16_at(pos + 20) as usize;
        let filesize = (u32::from(u16_at(pos + 22)) << 16) | u32::from(u16_at(pos + 24));
        if namesize == 0 || pos + 26 + namesize > data.len() {
            anyhow::bail!("cpio(bin): 名字长度无效");
        }
        let mut name = String::from_utf8_lossy(&data[pos + 26..pos + 26 + namesize - 1])
            .trim_end_matches('\0')
            .to_string();
        let doff = (pos + 26 + (namesize + 1)) & !1;
        if doff + filesize as usize > data.len() {
            anyhow::bail!("cpio(bin): 数据越界");
        }
        if name.starts_with("TRAILER") {
            break;
        }
        name = name.trim_start_matches("./").to_string();
        let kind = match mode & 0xF000 {
            0x4000 => EntryKind::Directory,
            0xA000 => EntryKind::Symlink,
            0x8000 => EntryKind::File,
            _ => EntryKind::Special,
        };
        let fetch = match kind {
            EntryKind::Symlink => Fetch::Inline(data[doff..doff + filesize as usize].to_vec()),
            EntryKind::File => Fetch::Raw { off: doff as u64, len: u64::from(filesize) },
            _ => Fetch::Inline(Vec::new()),
        };
        out.push((
            Entry {
                name,
                kind,
                size: if kind == EntryKind::File { u64::from(filesize) } else { 0 },
                compressed_size: u64::from(filesize),
                method: "Stored".into(),
                mtime_epoch: (mtime > 0).then_some(u64::from(mtime)),
                encrypted: false,
                link_target: if kind == EntryKind::Symlink {
                    Some(String::from_utf8_lossy(&data[doff..doff + filesize as usize]).into_owned())
                } else {
                    None
                },
            },
            fetch,
        ));
        let _ = nlink;
        pos = doff + filesize as usize + (filesize as usize % 2);
        if hp {
            pos = (pos + 1) & !1;
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ iso

/// 极简 ISO9660:主 VD + 可选 Joliet SVD,递归目录记录,不含 Rock Ridge。
fn parse_iso(data: &[u8]) -> anyhow::Result<Vec<(Entry, Fetch)>> {
    if data.len() < 32768 + 2048 || &data[32769..32774] != b"CD001" {
        anyhow::bail!("iso: 未找到 CD001 主卷描述符");
    }
    // 逻辑块大小:both-endian 对(LE u16 + BE u16 相等),常见位于 PVD+126 或 +128
    let mut sector = 0usize;
    for p in [126usize, 128] {
        let le = u16::from_le_bytes([data[32768 + p], data[32768 + p + 1]]) as usize;
        let be = u16::from_be_bytes([data[32768 + p + 2], data[32768 + p + 3]]) as usize;
        if le == be && le >= 2048 && le.is_multiple_of(2048) {
            sector = le;
            break;
        }
    }
    if sector == 0 {
        anyhow::bail!("iso: 未找到合法逻辑块大小");
    }
    // 找 Joliet SVD(escape %/u),否则用主 VD
    let mut use_joliet: Option<(usize, bool)> = None; // (vd offset, le)
    let mut mi = 17usize;
    while mi * sector + 90 <= data.len() {
        let at = mi * sector;
        if data[at] != 2 || &data[at + 1..at + 6] != b"CD001" {
            break;
        }
        let esc = &data[at + 88..at + 90];
        if esc == b"%/" || esc == b"0/" {
            use_joliet = Some((at, esc == b"%/"));
            break;
        }
        mi += 1;
    }
    let (vd, joliet) = match use_joliet {
        Some(v) => (v.0, v.1),
        None => (16 * sector, false),
    };
    if vd + 190 > data.len() {
        anyhow::bail!("iso: 卷描述符不完整");
    }
    // 目录记录固定 both-endian:extent @+2(LE)/+6(BE),size @+10(LE)/+14(BE)。
    // 始终读 LE 半边;Joliet 只影响文件名字符集,不改字节序。
    let root_at = vd + 156;
    let root_lba = u32::from_le_bytes(data[root_at + 2..root_at + 6].try_into().unwrap()) as usize;
    let root_len = u32::from_le_bytes(data[root_at + 10..root_at + 14].try_into().unwrap()) as usize;
    let mut out = Vec::new();
    let mut stack: Vec<(usize, usize, String)> = vec![(root_lba, root_len, String::new())]; // lba, len, prefix
    let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();
    while let Some((lba, len, prefix)) = stack.pop() {
        // 防御:同一 extent 只走一次(损坏/伪造档可能自环),条目总量封顶
        if !visited.insert(lba) || out.len() >= MAX_ENTRIES {
            continue;
        }
        let mut at = lba.saturating_mul(sector);
        let end = at.saturating_add(len);
        while at + 34 <= data.len().min(end) {
            let rlen = data[at] as usize;
            if rlen == 0 {
                at = (at / sector + 1) * sector; // 记录跨扇区/填充
                continue;
            }
            if rlen < 34 {
                break; // 记录长度非法:停在最后一个可信条目
            }
            let ext = u32::from_le_bytes(data[at + 2..at + 6].try_into().unwrap()) as usize;
            let sz = u32::from_le_bytes(data[at + 10..at + 14].try_into().unwrap()) as usize;
            let flags = data[at + 25];
            let nl = data[at + 32] as usize;
            if at + 33 + nl > data.len() {
                break; // 名字越界:档已损坏
            }
            let raw = &data[at + 33..at + 33 + nl];
            // ECMA-119 9.1:dot 条目名是单字节 0x00(自)/0x01(父),Joliet 下亦然
            if nl == 1 && raw[0] <= 1 {
                at += rlen;
                continue;
            }
            let name = decode_iso_name(raw, joliet);
            at += rlen;
            if name == "." || name == ".." {
                continue;
            }
            let full = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            if flags & 2 != 0 {
                out.push((
                    Entry { name: full.clone(), kind: EntryKind::Directory, size: 0, compressed_size: 0, method: "ISO".into(), mtime_epoch: None, encrypted: false, link_target: None },
                    Fetch::Inline(Vec::new()),
                ));
                stack.push((ext, sz, full));
            } else {
                out.push((
                    Entry {
                        name: full,
                        kind: EntryKind::File,
                        size: sz as u64,
                        compressed_size: sz as u64,
                        method: "ISO".into(),
                        mtime_epoch: None,
                        encrypted: false,
                        link_target: None,
                    },
                    Fetch::Raw { off: (ext.saturating_mul(sector)) as u64, len: sz as u64 },
                ));
            }
        }
    }
    Ok(out)
}

fn decode_iso_name(raw: &[u8], joliet: bool) -> String {
    let mut s = if joliet {
        let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(raw).into_owned()
    };
    if let Some(i) = s.find(';') {
        s.truncate(i);
    }
    s
}

// ------------------------------------------------------------------ 辅助

fn ascii_num(s: &[u8]) -> Option<u64> {
    let t = std::str::from_utf8(s).ok()?.trim();
    if t.is_empty() {
        return Some(0);
    }
    t.parse().ok()
}

fn ascii_num_oct(s: &[u8]) -> Option<u32> {
    let t = std::str::from_utf8(s).ok()?.trim().trim_start_matches('0');
    if t.is_empty() {
        return Some(0);
    }
    u32::from_str_radix(t, 8).ok()
}

// ------------------------------------------------------------------ 单流压缩(lzma-alone / lzip / lz4)

/// 写入侧限额器(Write 无标准 take 适配器):超限置 overflow 并中断解码。
struct Capped<'a> {
    out: &'a mut Vec<u8>,
    cap: u64,
    overflow: bool,
}
impl std::io::Write for Capped<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.out.len() as u64 + buf.len() as u64 > self.cap {
            self.overflow = true;
            return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "size limit"));
        }
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn comp_label(c: Comp) -> &'static str {
    match c {
        Comp::Gzip => "GZip",
        Comp::Bzip2 => "BZip2",
        Comp::Xz => "Xz",
        Comp::Zstd => "Zstd",
        Comp::LzmaAlone => "LzmaAlone",
        Comp::Lzip => "Lzip",
        Comp::Lz4 => "Lz4",
    }
}

/// 字节流是否以合法 tar 头开始(ustar 魔数 + 头和校验)。
/// 字节流是否以合法 tar 头开始(ustar 魔数 + 头和校验)。
fn looks_tar(b: &[u8]) -> bool {
    if b.len() < 512 || &b[257..262] != b"ustar" {
        return false;
    }
    let given = tar_octal(&b[148..156]);
    let sum: u32 = b[..512]
        .iter()
        .enumerate()
        .map(|(i, &c)| if (148..156).contains(&i) { 32 } else { c as u32 })
        .sum();
    given == Some(sum)
}

fn tar_octal(f: &[u8]) -> Option<u32> {
    let s: String = f
        .iter()
        .map(|&b| b as char)
        .take_while(|c| *c != '\0' && *c != ' ')
        .collect();
    u32::from_str_radix(s.trim(), 8).ok()
}

/// 把 tar 字节流经 `tar` crate(unarc 同款)展开为条目计划。
fn plan_tar(bytes: &[u8]) -> anyhow::Result<Vec<(Entry, Fetch)>> {
    let mut ar = tar::Archive::new(std::io::Cursor::new(bytes));
    let mut out = Vec::new();
    for e in ar.entries()? {
        let mut e = e?;
        if out.len() >= MAX_ENTRIES {
            break;
        }
        let (et, size, mtime) = {
            let h = e.header();
            (h.entry_type(), h.size().unwrap_or(0), h.mtime().ok())
        };
        let name = e.path()?.to_string_lossy().into_owned();
        if name.is_empty() {
            continue;
        }
        let kind = if et == tar::EntryType::Directory {
            EntryKind::Directory
        } else if et == tar::EntryType::Symlink {
            EntryKind::Symlink
        } else if et == tar::EntryType::Link {
            EntryKind::HardLink
        } else if et == tar::EntryType::Char || et == tar::EntryType::Block || et == tar::EntryType::Fifo {
            EntryKind::Special
        } else {
            EntryKind::File
        };
        let link_target = if et == tar::EntryType::Symlink || et == tar::EntryType::Link {
            e.link_name().ok().flatten().map(|p| p.to_string_lossy().into_owned())
        } else {
            None
        };
        let data = if kind == EntryKind::File {
            let mut v = Vec::new();
            e.read_to_end(&mut v)?;
            v
        } else {
            Vec::new()
        };
        out.push((
            Entry {
                name,
                kind,
                size: if kind == EntryKind::File { size } else { 0 },
                compressed_size: size,
                method: "Tar".into(),
                mtime_epoch: mtime,
                encrypted: false,
                link_target,
            },
            Fetch::Inline(data),
        ));
    }
    Ok(out)
}

/// 从路径推单流解压目标名(去 .foo 伪装,再去压缩扩展)。
fn single_stem(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let lower = name.to_lowercase();
    let has_foo = lower.ends_with(".foo");
    // 压缩后缀是 ASCII:按字节长切原始名安全(伪装的 .foo 同为 ASCII 尾)
    let core = if has_foo { &lower[..lower.len() - 4] } else { &lower };
    for suf in [".lzma", ".lz4", ".lz"] {
        if core.ends_with(suf) {
            let cut = name.len() - suf.len() - if has_foo { 4 } else { 0 };
            return Some(name[..cut].to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_stem_strips_foo_and_comp_ext() {
        let p = |s: &str| single_stem(Path::new(s));
        assert_eq!(p("t.txt.lzma").as_deref(), Some("t.txt"));
        assert_eq!(p("t.txt.lzma.foo").as_deref(), Some("t.txt"));
        assert_eq!(p("T.TAR.LZMA").as_deref(), Some("T.TAR"));
        assert_eq!(p("x.lz").as_deref(), Some("x"));
        assert_eq!(p("x.LZ4.foo").as_deref(), Some("x"));
        assert_eq!(p("noext").as_deref(), None);
        assert_eq!(p("t.txt.gz").as_deref(), None); // gz 走 unarc 路径,不经此函数
    }

    #[test]
    fn looks_tar_rejects_garbage() {
        assert!(!looks_tar(&[0u8; 512]));
        let mut h = vec![0u8; 512];
        h[257..262].copy_from_slice(b"ustar");
        // 头和字段清零 → 校验和必然不符
        assert!(!looks_tar(&h));
    }
}
