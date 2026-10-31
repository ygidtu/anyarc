//! M2 自写容器后端:ar / deb / rpm / cpio / iso9660。
//!
//! 与 7z 对拍语义(见 PLAN §5 注记):
//! - ar:全部成员原样;
//! - deb:只暴露 `data.tar`(取 `data.tar.*` 成员,按内层压缩解压并剥扩展名);
//! - rpm:payload 解压为单条 `{NAME-VER-REL.ARCH}.cpio`;
//! - cpio:目录树(newc 070701/070702);
//! - iso:ISO9660 目录树(含 Joliet 名字),条目按存储名(去 `;1`)。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
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
    fn is_single_file(&self) -> bool {
        // 单流压缩(gz/lzma/lzip/lz4)非 tar 复合、音频解码 → 计划里只有一个文件条目
        self.plan.len() == 1 && self.plan[0].0.kind == EntryKind::File
    }
    /// 真流式:压缩成员边解压边分块写 w(限额用 take(cap+1) 检出超限)。
    fn read_current_to(&mut self, w: &mut dyn Write) -> anyhow::Result<u64> {
        let i = self.current.take().ok_or_else(|| anyhow::anyhow!("无当前条目"))?;
        let flen = std::fs::metadata(&self.path)?.len();
        let cap = self.max_entry_size.unwrap_or(u64::MAX);
        match &self.plan[i].1 {
            Fetch::Inline(b) => write_chunks(b, w),
            Fetch::Raw { off, len } => {
                if *off > flen || *len > flen - *off {
                    anyhow::bail!("条目数据超出文件范围");
                }
                self.check_size(*len)?;
                let mut f = File::open(&self.path)?;
                f.seek(SeekFrom::Start(*off))?;
                copy_chunks(f.take(*len), w)
            }
            Fetch::Comp { off, len, comp } => {
                if *off > flen || *len > flen - *off {
                    anyhow::bail!("条目数据超出文件范围");
                }
                let mut f = File::open(&self.path)?;
                f.seek(SeekFrom::Start(*off))?;
                let raw = f.take(*len);
                let n = match comp {
                    Comp::Gzip => copy_chunks(flate2::read::MultiGzDecoder::new(raw).take(cap.saturating_add(1)), w)?,
                    Comp::Bzip2 => copy_chunks(bzip2::read::BzDecoder::new(raw).take(cap.saturating_add(1)), w)?,
                    Comp::Zstd => {
                        let d = zstd::stream::read::Decoder::new(raw)?;
                        copy_chunks(d.take(cap.saturating_add(1)), w)?
                    }
                    Comp::Lz4 => copy_chunks(lz4_flex::frame::FrameDecoder::new(raw).take(cap.saturating_add(1)), w)?,
                    Comp::Lzip => copy_chunks(lzma_rust2::LzipReader::new(raw).take(cap.saturating_add(1)), w)?,
                    Comp::Xz => {
                        // 借用 unarc 的纯 Rust XZ 解码器(无 Read 包装,整读后分块写)
                        let cur = std::io::BufReader::new(raw);
                        let mut a = unarc_rs::xz::XzArchive::new(cur).map_err(|e| anyhow::anyhow!("{e}"))?;
                        let v = a.read_with_limit(Some(cap)).map_err(|e| anyhow::anyhow!("{e}"))?;
                        write_chunks(&v, w)?
                    }
                    Comp::LzmaAlone => {
                        let mut r = std::io::BufReader::new(raw);
                        let mut sink = CappedWrite { w, written: 0, cap, overflow: false };
                        if let Err(e) = lzma_rs::lzma_decompress(&mut r, &mut sink) {
                            if sink.overflow {
                                anyhow::bail!("解压尺寸超过上限 {cap}(--max-entry-size)");
                            }
                            anyhow::bail!("lzma: {e}");
                        }
                        sink.written
                    }
                };
                if n > cap {
                    anyhow::bail!("解压尺寸超过上限 {cap}(--max-entry-size)");
                }
                Ok(n)
            }
        }
    }
}

/// 分块转发 r → w(每块触发调用方的进度回调)。
fn copy_chunks(mut r: impl Read, w: &mut dyn Write) -> anyhow::Result<u64> {
    let mut buf = vec![0u8; crate::backend::WRITE_CHUNK];
    let mut total = 0u64;
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n])?;
        total += n as u64;
    }
    Ok(total)
}

fn write_chunks(b: &[u8], w: &mut dyn Write) -> anyhow::Result<u64> {
    for chunk in b.chunks(crate::backend::WRITE_CHUNK) {
        w.write_all(chunk)?;
    }
    Ok(b.len() as u64)
}

/// 限额写入器(写穿到内层 w;超 cap 报错,供限额检出)。
struct CappedWrite<'a> {
    w: &'a mut dyn Write,
    written: u64,
    cap: u64,
    overflow: bool,
}

impl Write for CappedWrite<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written + buf.len() as u64 > self.cap {
            self.overflow = true;
            return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "size limit"));
        }
        self.w.write_all(buf)?;
        self.written += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.w.flush()
    }
}

pub(crate) fn decompress(comp: Comp, raw: &[u8], cap: u64) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    match comp {
        Comp::Gzip => {
            // MultiGzDecoder:多成员 gzip 全量拼接(与 gzip/7z/patool 语义一致;
            // flate2 的 GzDecoder 只解第一个成员)
            let mut d = flate2::read::MultiGzDecoder::new(raw).take(cap + 1);
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

/// 只解压前 512B 判定 tar 复合;流损坏返回残缺/空(真报错留给全量解压)。
fn decompress_head(comp: Comp, data: &[u8]) -> Vec<u8> {
    let cur = std::io::Cursor::new(data);
    let mut out = Vec::new();
    match comp {
        Comp::Gzip => {
            let _ = flate2::read::MultiGzDecoder::new(cur).take(512).read_to_end(&mut out);
        }
        Comp::Bzip2 => {
            let _ = bzip2::read::BzDecoder::new(cur).take(512).read_to_end(&mut out);
        }
        Comp::Zstd => {
            if let Ok(d) = zstd::stream::read::Decoder::new(cur) {
                let _ = d.take(512).read_to_end(&mut out);
            }
        }
        Comp::Lz4 => {
            let _ = lz4_flex::frame::FrameDecoder::new(cur).take(512).read_to_end(&mut out);
        }
        Comp::Lzip => {
            let _ = lzma_rust2::LzipReader::new(cur).take(512).read_to_end(&mut out);
        }
        Comp::LzmaAlone => {
            let mut r = std::io::BufReader::new(cur);
            let mut cw = Capped { out: &mut out, cap: 512, overflow: false };
            let _ = lzma_rs::lzma_decompress(&mut r, &mut cw);
        }
        Comp::Xz => {
            if let Ok(mut a) = unarc_rs::xz::XzArchive::new(std::io::BufReader::new(cur)) {
                out = a.read_with_limit(Some(512)).unwrap_or_default();
            }
        }
    }
    out
}

/// 流头部/尾部元数据估算解压总长(供进度条/ETA;None = 未知)。
fn stream_total(comp: Comp, data: &[u8]) -> Option<u64> {
    match comp {
        // gzip 尾 ISIZE(mod 2^32):单成员 <4 GiB 精确;多成员只见末成员,
        // 超出估算时 BytesBar 自动降级为无总长模式。
        Comp::Gzip if data.len() >= 10 => {
            let isize_b: [u8; 4] = data[data.len() - 4..].try_into().ok()?;
            Some(u32::from_le_bytes(isize_b) as u64)
        }
        // LZMA-alone 头:props(1) + 字典大小 u32 LE(4) + 解压尺寸 u64 LE(8,全 1 = 未知)。
        Comp::LzmaAlone if data.len() >= 13 => {
            let sz_b: [u8; 8] = data[5..13].try_into().ok()?;
            let sz = u64::from_le_bytes(sz_b);
            (sz != u64::MAX).then_some(sz)
        }
        // lz4 frame(与 lz4_flex 解析器同布局):magic(4) FLG(1) BD(1) [CSIZE(8)]。
        Comp::Lz4 if data.len() >= 14 && &data[..4] == b"\x04\x22\x4d\x18" && data[4] & 0b0000_1000 != 0 => {
            let cs: [u8; 8] = data[6..14].try_into().ok()?;
            Some(u64::from_le_bytes(cs))
        }
        _ => None,
    }
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
        // 单流压缩:只 peek 解压头部 512B 判定 tar 复合;单条目改为读时流式解压
        // (read_current_to 分块推进度条,大文件不再开档即全量驻留内存)。
        Container::LzmaAlone | Container::Lzip | Container::Lz4 | Container::Gz => {
            let comp = match container {
                Container::LzmaAlone => Comp::LzmaAlone,
                Container::Lzip => Comp::Lzip,
                Container::Lz4 => Comp::Lz4,
                _ => Comp::Gzip,
            };
            let cap = opts.max_entry_size.unwrap_or(u64::MAX);
            if looks_tar(&decompress_head(comp, &data)) {
                let bytes = decompress(comp, &data, cap)?;
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
                        // 头部/尾部估算(gz ISIZE 等);未知为 0(进度条退化为无总长模式)
                        size: stream_total(comp, &data).unwrap_or(0),
                        compressed_size: data.len() as u64,
                        method: comp_label(comp).into(),
                        mtime_epoch: None,
                        encrypted: false,
                        link_target: None,
                    },
                    Fetch::Comp { off: 0, len: data.len() as u64, comp },
                )]
            }
        }
        // L5 音频:解码为 PCM 并包 44B WAV 头,单条目 {stem}.wav
        Container::Flac | Container::Ape | Container::Shn => {
            let cap = opts.max_entry_size.unwrap_or(u64::MAX);
            let (fmt_label, bytes) = decode_audio(container, &data, cap)?;
            let name = match &opts.single_file_name {
                Some(n) => n.clone(),
                None => audio_out_name(path),
            };
            vec![(
                Entry {
                    name,
                    kind: EntryKind::File,
                    size: bytes.len() as u64,
                    compressed_size: data.len() as u64,
                    method: fmt_label.into(),
                    mtime_epoch: None,
                    encrypted: false,
                    link_target: None,
                },
                Fetch::Inline(bytes),
            )]
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
        Container::Gz => Format::Gz,
        Container::Flac => Format::Flac,
        Container::Ape => Format::Ape,
        Container::Shn => Format::Shn,
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

/// newc/070702 ASCII 头。字段各 8 hex,标准序(GNU cpio 产物实测校准):
/// ino@6 mode@14 uid@22 gid@30 nlink@38 mtime@46 filesize@54
/// devmaj@62 devmin@70 rdevmaj@78 rdevmin@86 namesize@94 chksum@102,name@110
fn parse_cpio_newc(data: &[u8]) -> anyhow::Result<Vec<(Entry, Fetch)>> {
    if data.len() < 110 {
        anyhow::bail!("cpio: 文件过短");
    }
    let hex = |s: &[u8]| u32::from_str_radix(std::str::from_utf8(s).unwrap_or("0"), 16).unwrap_or(0);
    let mut pos = 0usize;
    let mut out: Vec<(Entry, Fetch)> = Vec::new();
    // (devmaj, ino) → 组内条目下标;解析完做二段归并(见下)
    let mut groups: std::collections::HashMap<(u32, u32), Vec<usize>> = std::collections::HashMap::new();
    while pos + 110 <= data.len() && data[pos..].starts_with(b"0707") {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        let ino = hex(&data[pos + 6..pos + 14]);
        let mode = hex(&data[pos + 14..pos + 22]);
        let nlink = hex(&data[pos + 38..pos + 46]);
        let mtime = hex(&data[pos + 46..pos + 54]);
        let mut filesize = hex(&data[pos + 54..pos + 62]) as u64;
        let devmaj = hex(&data[pos + 62..pos + 70]);
        let _devmin = hex(&data[pos + 70..pos + 78]);
        let namesize = hex(&data[pos + 94..pos + 102]) as usize;
        if namesize == 0 || pos + 110 + namesize > data.len() {
            anyhow::bail!("cpio: 名字长度无效");
        }
        let chksum = hex(&data[pos + 102..pos + 110]);
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
        if kind == EntryKind::File && nlink > 1 {
            groups.entry((devmaj, ino)).or_default().push(out.len());
        }
        let fetch = if kind == EntryKind::Symlink {
            Fetch::Inline(data[doff..doff + filesize as usize].to_vec())
        } else if kind == EntryKind::File {
            Fetch::Raw { off: doff as u64, len: filesize }
        } else {
            Fetch::Inline(Vec::new())
        };
        let link_target = if kind == EntryKind::Symlink {
            Some(String::from_utf8_lossy(&data[doff..doff + filesize as usize]).into_owned())
        } else {
            None
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
                link_target,
            },
            fetch,
        ));
        pos = align4(doff + filesize as usize);
    }
    // 二段归并:同 (devmaj, ino) 组内以“带数据的条目”为捐赠者(保持 File+Raw);
    // 其余改 HardLink 回填 link_target。GNU cpio 既可能 dup 在前(0 字节)、
    // 也可能把数据挂在后出现的 dup 名下,故按 size>0 选捐赠者而非出现顺序。
    for idxs in groups.values() {
        if idxs.len() < 2 {
            continue;
        }
        let donor = idxs.iter().copied().find(|&i| out[i].0.size > 0).unwrap_or(idxs[0]);
        for &i in idxs {
            if i != donor {
                out[i].0.kind = EntryKind::HardLink;
                out[i].0.size = 0;
                out[i].0.link_target = Some(out[donor].0.name.clone());
                out[i].1 = Fetch::Inline(Vec::new());
            }
        }
    }
    Ok(out)
}

fn align4(x: usize) -> usize {
    (x + 3) & !3
}

/// 二进制 cpio(bin 0707 / hpbin 070707):26 字节头。字段偏移已用
/// 真实 fixture(t.cpio = hpbin)校准(与 POSIX 名义布局相比整体前移一字段)。
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
    // ino(+10,16 位)→ 首条目名:同 inode 的后续常规文件按 hardlink 暴露
    let mut by_ino: std::collections::HashMap<u16, String> = std::collections::HashMap::new();
    while pos + 26 <= data.len() {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        if u16_at(pos) != want {
            break; // 头损坏:停在最后一个合法条目
        }
        let mode = u16_at(pos + 6);
        let ino = u16_at(pos + 10);
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
        let mut kind = match mode & 0xF000 {
            0x4000 => EntryKind::Directory,
            0xA000 => EntryKind::Symlink,
            0x8000 => EntryKind::File,
            _ => EntryKind::Special,
        };
        let is_dup = if kind == EntryKind::File && nlink > 1 {
            if by_ino.contains_key(&ino) {
                true
            } else {
                by_ino.insert(ino, name.clone());
                false
            }
        } else {
            false
        };
        if is_dup {
            kind = EntryKind::HardLink;
        }
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
                link_target: match kind {
                    EntryKind::Symlink => Some(String::from_utf8_lossy(&data[doff..doff + filesize as usize]).into_owned()),
                    EntryKind::HardLink => by_ino.get(&ino).cloned(),
                    _ => None,
                },
            },
            fetch,
        ));
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
    for suf in [".lzma", ".lz4", ".lz", ".gz"] {
        if core.ends_with(suf) {
            let cut = name.len() - suf.len() - if has_foo { 4 } else { 0 };
            return Some(name[..cut].to_string());
        }
    }
    None
}

// ------------------------------------------------------------------ L5 音频

/// 音频解码输出名:去 .foo 伪装、去音频扩展,换 `.wav`(patool 语义:
/// flac/ape/shn 解压产物为 {stem}.wav)。
fn audio_out_name(path: &Path) -> String {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("audio");
    let lower = name.to_lowercase();
    let has_foo = lower.ends_with(".foo");
    let core = if has_foo { &lower[..lower.len() - 4] } else { &lower };
    let stem = ["flac", "ape", "shn"]
        .iter()
        .find_map(|e| core.strip_suffix(&format!(".{e}")))
        .unwrap_or(core);
    format!("{stem}.wav")
}

/// 44 字节 PCM WAV 头(无扩展 chunk,patool 生态惯例)。
fn wav_header(channels: u16, rate: u32, bits: u16, data_len: u32) -> Vec<u8> {
    let block_align = channels * bits / 8;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + data_len).to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes()); // PCM
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * block_align as u32).to_le_bytes());
    h.extend_from_slice(&block_align.to_le_bytes());
    h.extend_from_slice(&bits.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_len.to_le_bytes());
    h
}

/// 解码音频容器为单条目 WAV 字节(限额 cap 作用于 WAV 总长)。
/// 返回 (方法标签, WAV 字节)。
fn decode_audio(container: Container, data: &[u8], cap: u64) -> anyhow::Result<(String, Vec<u8>)> {
    let (label, wav) = match container {
        Container::Flac => ("FLAC", decode_flac(data, cap)?),
        Container::Ape => ("APE", decode_ape(data, cap)?),
        Container::Shn => ("Shorten", decode_shn(data, cap)?),
        _ => unreachable!("decode_audio 只处理音频容器"),
    };
    if wav.len() as u64 > cap {
        anyhow::bail!("解压尺寸超过上限 {cap}(--max-entry-size)");
    }
    Ok((label.to_string(), wav))
}

/// FLAC → WAV(claxon)。位深:≤8 → 8u,≤16 → 16,其余 → 24(截断到 24 位,
/// FLAC 实际罕见 >24;20/24 均按 3 字节 LE 存)。
fn decode_flac(data: &[u8], cap: u64) -> anyhow::Result<Vec<u8>> {
    let mut rd = claxon::FlacReader::new(std::io::Cursor::new(data))
        .map_err(|e| anyhow::anyhow!("FLAC 打开失败: {e}"))?;
    let si = rd.streaminfo();
    let ch = si.channels as u16;
    let bits: u16 = if si.bits_per_sample <= 8 {
        8
    } else if si.bits_per_sample <= 16 {
        16
    } else if si.bits_per_sample <= 24 {
        24
    } else {
        anyhow::bail!("FLAC 位深 {} 不支持(>24)", si.bits_per_sample);
    };
    let per = usize::from(bits / 8);
    // 预检:total_samples 已知时先估大小(未知则靠流式限额)
    if let Some(tot) = si.samples {
        let est = tot * u64::from(ch) * per as u64;
        if est > 4 << 30 {
            anyhow::bail!("FLAC 解码结果过大(≈{est} 字节),拒绝");
        }
    }
    let mut pcm: Vec<u8> = Vec::new();
    for s in rd.samples() {
        let s = s.map_err(|e| anyhow::anyhow!("FLAC 解码失败: {e}"))?;
        match bits {
            8 => pcm.push((s + 128) as u8), // WAV 8-bit 为无符号
            16 => pcm.extend_from_slice(&(s.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16).to_le_bytes()),
            _ => pcm.extend_from_slice(&s.to_le_bytes()[..3]),
        }
        if pcm.len() > 4 << 30 || pcm.len() as u64 > cap {
            anyhow::bail!("FLAC 解码结果过大(>{}),中止", if cap < (4 << 30) { format!("{cap} 字节(--max-entry-size)") } else { "4 GiB".into() });
        }
    }
    let mut wav = wav_header(ch, si.sample_rate, bits, pcm.len() as u32);
    wav.extend_from_slice(&pcm);
    Ok(wav)
}

/// Monkey's Audio → WAV(ape-decoder;仅 ≥3.95,<3.95 报清晰错误)。
/// decode_all 已按位深输出交织 LE PCM,直接配 WAV 头。
fn decode_ape(data: &[u8], cap: u64) -> anyhow::Result<Vec<u8>> {
    let mut dec = ape_decoder::ApeDecoder::new(std::io::Cursor::new(data))
        .map_err(|e| anyhow::anyhow!("APE 打开失败(仅支持 APE 3.95+): {e}"))?;
    let (ch, rate, bits) = {
        let info = dec.info();
        (info.channels, info.sample_rate, info.bits_per_sample)
    };
    if info_size_exceeds(&dec, cap) {
        anyhow::bail!("APE 解码结果超过上限 {cap}(--max-entry-size)");
    }
    let pcm = dec
        .decode_all()
        .map_err(|e| anyhow::anyhow!("APE 解码失败: {e}"))?;
    let mut wav = wav_header(ch, rate, bits, pcm.len() as u32);
    wav.extend_from_slice(&pcm);
    Ok(wav)
}

/// APE 输出大小预估(total_samples×block_align)。
fn info_size_exceeds(dec: &ape_decoder::ApeDecoder<std::io::Cursor<&[u8]>>, cap: u64) -> bool {
    let i = dec.info();
    i.total_samples.saturating_mul(i.block_align as u64) > cap.saturating_sub(44)
}

/// Shorten v2/v3 → WAV(自研解码器 src/shn.rs,经 t.shn→t.wav 字节级校准)。
/// 采样率不在 SHN 流内:若 verbatim 信封是标准 RIFF/WAVE 头则从中取;
/// 否则按 patool 惯例默认 44100。位深按 filetype:1/2→8 位,其余 PCM→16 位。
fn decode_shn(data: &[u8], cap: u64) -> anyhow::Result<Vec<u8>> {
    // 每样本至少 1 字节:样本数预算取 cap/2(留 WAV 头/多字节余量)
    let ds = crate::shn::decode(data, cap / 2)?;
    let nch = ds.channels as usize;
    let bits: u16 = if matches!(ds.filetype, 1 | 2) { 8 } else { 16 };
    let mut rate = 44100u32;
    let envelope_is_wav = ds.verbatim.len() >= 36
        && ds.verbatim.starts_with(b"RIFF")
        && ds.verbatim.get(8..12) == Some(b"WAVE")
        && ds.verbatim.get(12..16) == Some(b"fmt ");
    let mut header: Option<Vec<u8>> = None;
    if envelope_is_wav {
        let fmt_ch = u16::from_le_bytes([ds.verbatim[22], ds.verbatim[23]]);
        rate = u32::from_le_bytes(ds.verbatim[24..28].try_into().unwrap());
        let fmt_bits = u16::from_le_bytes([ds.verbatim[34], ds.verbatim[35]]);
        if fmt_ch as usize != nch {
            anyhow::bail!("Shorten 信封声道({fmt_ch})与流内声道({nch})不一致");
        }
        if fmt_bits != bits {
            anyhow::bail!("Shorten 信封位深({fmt_bits})与 filetype({}) 不符", ds.filetype);
        }
        header = Some(ds.verbatim.clone());
    }
    let len = ds.channel_samples.iter().map(|c| c.len()).max().unwrap_or(0);
    let per = usize::from(bits / 8);
    let pcm_len = len * nch * per;
    let mut wav = match header {
        // 原 WAV 信封:保留全部 chunk 结构,仅按实际解码长度修正 RIFF/data size
        Some(mut h) => {
            h[4..8].copy_from_slice(&((36 + pcm_len) as u32).to_le_bytes());
            if let Some(i) = h.windows(4).position(|w| w == b"data") {
                if i + 8 <= h.len() {
                    h[i + 4..i + 8].copy_from_slice(&(pcm_len as u32).to_le_bytes());
                    h.truncate(i + 8); // data 是最后一个 chunk(信封只到头部为止)
                }
            }
            h
        }
        None => wav_header(nch as u16, rate, bits, pcm_len as u32),
    };
    // GNU 内部样本与容器字节同位型(无符号类型内部即 0..N),直接按位截断写出
    for i in 0..len {
        for c in &ds.channel_samples {
            let s = c.get(i).copied().unwrap_or(0);
            if bits == 8 {
                wav.push(s as u8);
            } else {
                wav.extend_from_slice(&(s as u16).to_le_bytes());
            }
        }
    }
    Ok(wav)
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
        assert_eq!(p("t.txt.gz").as_deref(), Some("t.txt")); // gz 也走自写 MultiGz 路径
    }

    #[test]
    fn looks_tar_rejects_garbage() {
        assert!(!looks_tar(&[0u8; 512]));
        let mut h = vec![0u8; 512];
        h[257..262].copy_from_slice(b"ustar");
        // 头和字段清零 → 校验和必然不符
        assert!(!looks_tar(&h));
    }

    #[test]
    fn stream_total_gzip_isize() {
        // 构造 gzip:头部 10B + 尾部 ISIZE(u32 LE)。ISIZE = 0x3c00000
        let mut data = vec![0x1f, 0x8b, 0, 0, 0, 0, 0, 0, 0, 0];
        data.extend_from_slice(&[0, 0, 0, 0]); // CRC32
        data.extend_from_slice(&0x03c0_0000u32.to_le_bytes()); // ISIZE
        assert_eq!(stream_total(Comp::Gzip, &data), Some(0x03c0_0000));
    }

    #[test]
    fn stream_total_lzma_alone() {
        // props(1) + dict(4) + size(8)
        let mut data = vec![0x5d, 0x00, 0x00, 0x80, 0x00];
        data.extend_from_slice(&62914560u64.to_le_bytes()); // known size
        data.extend_from_slice(&[0, 0, 0, 0]); // rest
        assert_eq!(stream_total(Comp::LzmaAlone, &data), Some(62914560));
        // unknown size(全 1)→ None
        let mut unk = vec![0x5d, 0x00, 0x00, 0x80, 0x00];
        unk.extend_from_slice(&u64::MAX.to_le_bytes());
        unk.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(stream_total(Comp::LzmaAlone, &unk), None);
    }

    #[test]
    fn stream_total_lz4_frame() {
        // magic(4) FLG(1,含 content-size bit3)BD(1) CSIZE(8)
        let mut data = vec![0x04, 0x22, 0x4d, 0x18, 0b0000_1000, 0x40];
        data.extend_from_slice(&123456u64.to_le_bytes());
        data.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(stream_total(Comp::Lz4, &data), Some(123456));
    }

    /// 懒式流单文件路径:is_single_file/估算总长/read_current_to 与整读字节一致。
    #[test]
    fn lazy_gz_stream_matches_read_current() {
        use crate::detect::{Container, Detected, Known};
        let payload: Vec<u8> = (0..200_000u32).flat_map(|i| (i * 7919).to_le_bytes()).collect();
        let mut gz = Vec::new();
        {
            let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
            std::io::Write::write_all(&mut enc, &payload).unwrap();
            enc.finish().unwrap();
        }
        let path = std::env::temp_dir().join(format!("anyarc_lazy_{}.gz", std::process::id()));
        std::fs::write(&path, &gz).unwrap();

        let det = Detected { kind: Known::Container(Container::Gz), prefix_bytes: 0 };
        let mut ar = open(&path, &det, &crate::backend::OpenOptions::default()).unwrap();
        assert!(ar.is_single_file(), "单流 gz 应判定为单文件作业");
        let entry = ar.next_entry().unwrap().unwrap();
        assert_eq!(entry.size, payload.len() as u64, "总长估算 = ISIZE(单成员精确)");
        let mut sink = Vec::new();
        let n = ar.read_current_to(&mut sink).unwrap();
        assert_eq!(n as usize, payload.len());
        assert_eq!(sink, payload);
        let _ = std::fs::remove_file(&path);
    }

    /// 空 gzip(ISIZE=0):无总长模式,read_current_to 返回 0 且不 panic。
    #[test]
    fn lazy_gz_empty_stream() {
        use crate::detect::{Container, Detected, Known};
        let path = std::env::temp_dir().join(format!("anyarc_empty_{}.gz", std::process::id()));
        let mut gz = Vec::new();
        {
            let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
            std::io::Write::write_all(&mut enc, b"").unwrap();
            enc.finish().unwrap();
        }
        std::fs::write(&path, &gz).unwrap();
        let det = Detected { kind: Known::Container(Container::Gz), prefix_bytes: 0 };
        let mut ar = open(&path, &det, &crate::backend::OpenOptions::default()).unwrap();
        assert!(ar.is_single_file());
        let entry = ar.next_entry().unwrap().unwrap();
        assert_eq!(entry.size, 0); // 空流 ISIZE=0 → 无总长模式
        let mut sink = Vec::new();
        let n = ar.read_current_to(&mut sink).unwrap();
        assert_eq!(n, 0);
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod cpio_tests {
    use super::*;

    fn newc_entry(ino: u32, mode: u32, nlink: u32, mtime: u32, name: &str, data: &[u8]) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(b"070701");
        // magic(6) + 11 个 8-hex 字段(ino..chksum)+ name = 110 头
        for v in [ino, mode, 0, 0, nlink, mtime, data.len() as u32, 0, 0, 0, 0] {
            h.extend_from_slice(format!("{v:08X}").as_bytes());
        }
        h.extend_from_slice(format!("{:08X}", name.len() + 1).as_bytes());
        h.extend_from_slice(b"00000000"); // chksum
        h.extend_from_slice(name.as_bytes());
        h.push(0);
        while h.len() % 4 != 0 {
            h.push(0);
        }
        h.extend_from_slice(data);
        while h.len() % 4 != 0 {
            h.push(0);
        }
        h
    }

    #[test]
    fn newc_parses_sizes_and_hardlinks() {
        let mut d = Vec::new();
        // GNU 风格:0 字节的 dup 在前,数据挂在后出现的名字下
        d.extend(newc_entry(1, 0o100644, 2, 100, "a.txt", b""));
        d.extend(newc_entry(1, 0o100644, 2, 100, "c.lnk", b"hello"));
        d.extend(newc_entry(2, 0o100644, 1, 100, "b.txt", b"1234567890"));
        d.extend(newc_entry(0, 0, 1, 0, "TRAILER!!!", b""));
        while d.len() % 512 != 0 {
            d.push(0);
        }
        let ents = parse_cpio(&d).unwrap();
        let got: Vec<(&str, &str, u64)> = ents
            .iter()
            .map(|(e, _)| (e.name.as_str(), match e.kind {
                EntryKind::File => "file",
                EntryKind::HardLink => "hardlink",
                _ => "?",
            }, e.size))
            .collect();
        assert_eq!(got, vec![("a.txt", "hardlink", 0), ("c.lnk", "file", 5), ("b.txt", "file", 10)]);
        let lt = &ents[0].0.link_target;
        assert_eq!(lt.as_deref(), Some("c.lnk"));
    }
}
