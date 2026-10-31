//! unarc-rs 封装:单卷 / 分卷统一打开,类型擦除。

use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::{Path, PathBuf};

use unarc_rs::unified::{ArchiveEntry, ArchiveFormat, ArchiveOptions, UnifiedArchive, VolumeProvider};

use crate::backend::{ArchiveBackend, OpenOptions};
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
}

/// 条目名归一化:ArchiveEntry.name 不可变,统一在出口处做 `\` → `/`。
pub fn entry_name(entry: &ArchiveEntry) -> String {
    entry.name().replace('\\', "/")
}

impl ArchiveBackend for UnarcBackend {
    fn format(&self) -> ArchiveFormat {
        self.inner.format()
    }
    fn next_entry(&mut self) -> anyhow::Result<Option<ArchiveEntry>> {
        self.inner.next().map_err(|e| anyhow::anyhow!("{e}"))
    }
    fn read(&mut self, entry: &ArchiveEntry) -> anyhow::Result<Vec<u8>> {
        self.inner.read(entry).map_err(|e| anyhow::anyhow!("{e}"))
    }
    fn skip(&mut self, entry: &ArchiveEntry) -> anyhow::Result<()> {
        self.inner.skip(entry).map_err(|e| anyhow::anyhow!("{e}"))
    }
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
    };

    // 分卷:7z .001/.002… 与 zip .zip/.z01…
    if kind == ArchiveFormat::SevenZ {
        if let Some(vols) = collect_7z_volumes(path) {
            let a = ArchiveFormat::open_multi_volume_7z(&vols, to_opts(opts))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            return Ok(Box::new(UnarcBackend { inner: boxed(a) }));
        }
    }
    if kind == ArchiveFormat::Zip {
        if let Some(vols) = collect_zip_volumes(path) {
            let a = ArchiveFormat::open_multi_volume_zip(&vols, to_opts(opts))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            return Ok(Box::new(UnarcBackend { inner: boxed(a) }));
        }
    }

    let file = BufReader::new(File::open(path)?);
    let mut o = to_opts(opts);
    // RAR/ARJ:挂命名模式 provider(单卷不受影响,找不到后继即正常结束)
    if matches!(kind, ArchiveFormat::Rar | ArchiveFormat::Arj) {
        if let Some((dir, stem)) = split_dir_stem(path) {
            o = o.with_volume_provider(PatternVolumes { dir, stem, kind });
        }
    }
    let mut a = UnifiedArchive::open_with_format_and_options(file, kind, o)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(n) = &opts.single_file_name {
        a.set_single_file_name(n.clone());
    }
    Ok(Box::new(UnarcBackend { inner: boxed(a) }))
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
}
