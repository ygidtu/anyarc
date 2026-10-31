//! unarc-rs 封装:单卷 / 分卷统一打开,类型擦除,输出解耦的 Entry 模型。

use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::{Path, PathBuf};

use unarc_rs::EncryptionMethod;
use unarc_rs::date_time::DosDateTime;
use unarc_rs::unified::{ArchiveEntry, ArchiveEntryKind, ArchiveFormat, ArchiveOptions, UnifiedArchive, VolumeProvider};

use crate::backend::{ArchiveBackend, Entry, EntryKind, Format, OpenOptions};
use crate::detect::{Backlog, Detected, Known};

/// 把不同 Reader 单态化的 UnifiedArchive 擦成统一 trait object。
trait UnifiedOps {
    fn format(&self) -> ArchiveFormat;
    fn next(&mut self) -> unarc_rs::error::Result<Option<ArchiveEntry>>;
    fn read(&mut self, entry: &ArchiveEntry) -> unarc_rs::error::Result<Vec<u8>>;
    fn skip(&mut self, entry: &ArchiveEntry) -> unarc_rs::error::Result<()>;
}

impl<T: Read + Seek> UnifiedOps for UnifiedArchive<T> {
    fn format(&self) -> ArchiveFormat {
        UnifiedArchive::format(self)
    }
    fn next(&mut self) -> unarc_rs::error::Result<Option<ArchiveEntry>> {
        self.next_entry()
    }
    fn read(&mut self, entry: &ArchiveEntry) -> unarc_rs::error::Result<Vec<u8>> {
        self.read(entry)
    }
    fn skip(&mut self, entry: &ArchiveEntry) -> unarc_rs::error::Result<()> {
        self.skip(entry)
    }
}

pub struct UnarcBackend {
    inner: Box<dyn UnifiedOps>,
    current: Option<ArchiveEntry>,
}

impl ArchiveBackend for UnarcBackend {
    fn format(&self) -> Format {
        Format::Unarc(self.inner.format())
    }
    fn next_entry(&mut self) -> anyhow::Result<Option<Entry>> {
        let e = self.inner.next().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(e.map(|e| {
            let out = to_entry(&e);
            self.current = Some(e);
            out
        }))
    }
    fn read_current(&mut self) -> anyhow::Result<Vec<u8>> {
        let e = self.current.take().ok_or_else(|| anyhow::anyhow!("无当前条目(先 next_entry)"))?;
        self.inner.read(&e).map_err(|e| anyhow::anyhow!("{e}"))
    }
    fn skip_current(&mut self) -> anyhow::Result<()> {
        let e = self.current.take().ok_or_else(|| anyhow::anyhow!("无当前条目(先 next_entry)"))?;
        self.inner.skip(&e).map_err(|e| anyhow::anyhow!("{e}"))
    }
}

/// ArchiveEntry → 解耦 Entry(名字归一化 `\`→`/`,加密标志合并)。
fn to_entry(e: &ArchiveEntry) -> Entry {
    Entry {
        name: e.name().replace('\\', "/"),
        kind: match e.kind() {
            ArchiveEntryKind::File => EntryKind::File,
            ArchiveEntryKind::Directory => EntryKind::Directory,
            ArchiveEntryKind::SymbolicLink => EntryKind::Symlink,
            ArchiveEntryKind::HardLink => EntryKind::HardLink,
            ArchiveEntryKind::Special | ArchiveEntryKind::Unknown => EntryKind::Special,
        },
        size: e.original_size(),
        compressed_size: e.compressed_size(),
        method: e.compression_method().to_string(),
        mtime_epoch: e.modified_time().map(dos_epoch),
        encrypted: e.encryption() != EncryptionMethod::None,
        link_target: e.link_target().map(|t| t.replace('\\', "/")),
    }
}

/// DOS 时间戳 → Unix epoch 秒(公历换算,无外部依赖)。
fn dos_epoch(d: DosDateTime) -> u64 {
    let (mut y, m, dd) = (i64::from(d.year()), i64::from(d.month()), i64::from(d.day()));
    // days_from_civil Howard Hinnant
    y -= i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = 153 * (m + if m > 2 { -3 } else { 9 }) + 2;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy / 5 + dd - 1;
    let days = era * 146097 + doe - 719468;
    (days as u64) * 86400 + u64::from(d.hour()) * 3600 + u64::from(d.minute()) * 60 + u64::from(d.second())
}

fn to_opts(opts: &OpenOptions) -> ArchiveOptions {
    let mut o = ArchiveOptions::new()
        .with_verify_crc(opts.verify_crc)
        .with_max_entry_size(opts.max_entry_size)
        .with_max_total_size(opts.max_total_size);
    if let Some(p) = &opts.password {
        o = o.with_password(p.clone());
    }
    o
}

/// 路径命名模式分卷提供器:后端按编号 1,2,… 请求后续卷,
/// 找不到(返回 None)时若末卷未声明后继则按单卷正常结束。
struct PatternVolumes {
    dir: PathBuf,
    stem: String,
    kind: ArchiveFormat,
}

impl VolumeProvider for PatternVolumes {
    fn open_volume(&self, number: u32) -> Option<Box<dyn std::io::Read + Send>> {
        for cand in volume_candidates(&self.dir, &self.stem, self.kind, number) {
            if let Ok(f) = File::open(&cand) {
                return Some(Box::new(BufReader::new(f)));
            }
        }
        None
    }
}

/// 各格式的后续卷命名候选(number 从 1 起,1 = 第二个文件)。
fn volume_candidates(dir: &Path, stem: &str, kind: ArchiveFormat, number: u32) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match kind {
        ArchiveFormat::Rar => {
            // RAR5: name.part002.rar …(number 从 1 起,物理 part 号 = number+1)
            out.push(dir.join(format!("{stem}.part{:03}.rar", number + 1)));
            // RAR4: name.r00, name.r01 …(第二卷 = r00)
            if number >= 1 {
                out.push(dir.join(format!("{stem}.r{:02}", number - 1)));
            }
        }
        ArchiveFormat::Arj => {
            // ARJ: name.a01, name.a02 …
            out.push(dir.join(format!("{stem}.a{number:02}")));
        }
        _ => {}
    }
    out
}

/// 去掉最后一个扩展名得到分卷 stem(.rar/.cbr/.arj → 公共名)。
fn split_dir_stem(path: &Path) -> Option<(PathBuf, String)> {
    let name = path.file_name()?.to_str()?;
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    Some((path.parent()?.to_path_buf(), stem.to_string()))
}

/// 打开入口:分卷自动聚合,Backlog 格式给出明确错误。
pub fn open(path: &Path, det: &Detected, opts: &OpenOptions) -> anyhow::Result<Box<dyn ArchiveBackend>> {
    let kind = match det.kind {
        Known::Unarc(f) => f,
        Known::Backlog(b) => {
            let name = match b {
                Backlog::FreeArc => "FreeArc (.freearc/.arc)",
                Backlog::Alzip => "ALZ (ALTools)",
                Backlog::Shar => "shar (shell archive)",
                Backlog::Zpaq => "zpaq",
                Backlog::Vhd => "VHD/VHDX 磁盘镜像",
                Backlog::Wim => "WIM 镜像",
                Backlog::Udf => "UDF 文件系统镜像",
                Backlog::Chm => "CHM 帮助文档",
                Backlog::LzmaAlone => "LZMA 单流(含 .tar.lzma 外层)",
                Backlog::Planned(what) => {
                    anyhow::bail!(
                        "检测到 {what} 格式,当前版本不支持解压(M2 计划自写容器解析)。建议:系统工具(如 ar/rpm/cpio)可处理此档案"
                    );
                }
                Backlog::Other => "未知长尾格式",
            };
            anyhow::bail!(
                "检测到 {name} 格式,当前版本不支持解压(已列入 Backlog)。建议:7-Zip / 官方工具可处理此档案"
            );
        }
        Known::Container(_) => unreachable!("容器格式由 container 模块打开"),
    };

    // 分卷:7z .001/.002… 与 zip .zip/.z01…
    if kind == ArchiveFormat::SevenZ
        && let Some(vols) = collect_7z_volumes(path) {
            let a = ArchiveFormat::open_multi_volume_7z(&vols, to_opts(opts))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            return Ok(Box::new(UnarcBackend { inner: boxed(a), current: None }));
        }
    if kind == ArchiveFormat::Zip
        && let Some(vols) = collect_zip_volumes(path) {
            let a = ArchiveFormat::open_multi_volume_zip(&vols, to_opts(opts))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            return Ok(Box::new(UnarcBackend { inner: boxed(a), current: None }));
        }

    let file = BufReader::new(File::open(path)?);
    let mut o = to_opts(opts);
    // RAR/ARJ:挂命名模式 provider(单卷不受影响,找不到后继即正常结束)
    if matches!(kind, ArchiveFormat::Rar | ArchiveFormat::Arj)
        && let Some((dir, stem)) = split_dir_stem(path) {
            o = o.with_volume_provider(PatternVolumes { dir, stem, kind });
        }
    let mut a = UnifiedArchive::open_with_format_and_options(file, kind, o)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(n) = &opts.single_file_name {
        a.set_single_file_name(n.clone());
    }
    Ok(Box::new(UnarcBackend { inner: boxed(a), current: None }))
}

fn boxed<T: Read + Seek + 'static>(a: UnifiedArchive<T>) -> Box<dyn UnifiedOps> {
    Box::new(a)
}

/// `.7z.001` → 同 stem 的 `.002/.003/…`(按数字升序);非分卷返回 None。
fn collect_7z_volumes(path: &Path) -> Option<Vec<PathBuf>> {
    let name = path.file_name()?.to_str()?;
    let (stem, num) = name.rsplit_once('.')?;
    if num.len() != 3 || !num.chars().all(|c| c.is_ascii_digit()) || num != "001" {
        return None;
    }
    let dir = path.parent()?;
    let mut vols = vec![path.to_path_buf()];
    for n in 2u32.. {
        let cand = dir.join(format!("{stem}.{n:03}"));
        if cand.is_file() {
            vols.push(cand);
        } else {
            break;
        }
    }
    if vols.len() > 1 { Some(vols) } else { None }
}

/// 多卷 zip:起点必须是 `.zip`(末卷),或首卷 `.z01`?unarc 需要全部卷按序。
/// 从任一 `.zip/.zNN` 收集同 stem 的 `.z01..` + `.zip`。
fn collect_zip_volumes(path: &Path) -> Option<Vec<PathBuf>> {
    let name = path.file_name()?.to_str()?.to_lowercase();
    let (stem, ext) = name.rsplit_once('.')?;
    if ext != "zip" && !(ext.len() == 3 && ext.starts_with('z') && ext[1..].chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let dir = path.parent()?;
    let mut zparts: Vec<PathBuf> = Vec::new();
    for n in 1u32.. {
        let cand = dir.join(format!("{stem}.z{n:02}"));
        if cand.is_file() {
            zparts.push(cand);
        } else {
            break;
        }
    }
    if zparts.is_empty() {
        return None;
    }
    let zip_final = dir.join(format!("{stem}.zip"));
    if !zip_final.is_file() {
        return None;
    }
    zparts.push(zip_final);
    Some(zparts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn rar_volume_names() {
        let d = Path::new("/x");
        let c1 = volume_candidates(d, "t", ArchiveFormat::Rar, 1);
        assert_eq!(c1[0], Path::new("/x/t.part002.rar"));
        assert_eq!(c1[1], Path::new("/x/t.r00"));
        let c2 = volume_candidates(d, "t", ArchiveFormat::Rar, 2);
        assert_eq!(c2[1], Path::new("/x/t.r01"));
    }

    #[test]
    fn arj_volume_names() {
        let c = volume_candidates(Path::new("/x"), "t", ArchiveFormat::Arj, 1);
        assert_eq!(c[0], Path::new("/x/t.a01"));
    }

    #[test]
    fn stem_strip() {
        let (_, s) = split_dir_stem(Path::new("/x/t.rar")).unwrap();
        assert_eq!(s, "t");
        let (_, s) = split_dir_stem(Path::new("/x/t.cb7.foo")).unwrap();
        assert_eq!(s, "t.cb7");
    }

    #[test]
    fn dos_epoch_conversion() {
        // 2010-02-19 06:49:xx DOS → epoch 1266562140+
        let d = DosDateTime::new(((2010 - 1980) << 25) | (2 << 21) | (19 << 16) | (6 << 11) | (49 << 5) | 0);
        assert_eq!(dos_epoch(d), 1266562140);
    }
}
