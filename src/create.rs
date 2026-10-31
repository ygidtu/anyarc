//! `anyarc create` / `anyarc repack` 写路径。**白名单**(PLAN §6):tar 家族、zip、7z、
//! 单流压缩(gz/bz2/xz/zst/lzma/lzip/lz4)。白名单外一律清晰报错;RAR create 永久禁区。
//!
//! 单流压缩语义:输入**恰好一个文件**;多输入用 `.tar.*` 输出(自动容器化)。
//! 先写临时文件再 rename,失败不留半截输出。
//!
//! 条目统一为内存模型 [`WriteEntry`](create 从磁盘读入;repack 从归档读出),
//! 所有写后端消费同一结构。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::bail;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateFormat {
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    TarZst,
    TarLzma,
    TarLzip,
    TarLz4,
    Zip,
    SevenZ,
    Gz,
    Bz2,
    Xz,
    Zst,
    Lzma,
    Lzip,
    Lz4,
}

impl CreateFormat {
    pub fn name(&self) -> &'static str {
        match self {
            CreateFormat::Tar => "tar",
            CreateFormat::TarGz => "tar.gz",
            CreateFormat::TarBz2 => "tar.bz2",
            CreateFormat::TarXz => "tar.xz",
            CreateFormat::TarZst => "tar.zst",
            CreateFormat::TarLzma => "tar.lzma",
            CreateFormat::TarLzip => "tar.lzip",
            CreateFormat::TarLz4 => "tar.lz4",
            CreateFormat::Zip => "zip",
            CreateFormat::SevenZ => "7z",
            CreateFormat::Gz => "gz",
            CreateFormat::Bz2 => "bz2",
            CreateFormat::Xz => "xz",
            CreateFormat::Zst => "zst",
            CreateFormat::Lzma => "lzma",
            CreateFormat::Lzip => "lzip",
            CreateFormat::Lz4 => "lz4",
        }
    }

    /// 单流压缩(输入必须恰好 1 个文件)
    pub fn is_single(self) -> bool {
        matches!(
            self,
            CreateFormat::Gz
                | CreateFormat::Bz2
                | CreateFormat::Xz
                | CreateFormat::Zst
                | CreateFormat::Lzma
                | CreateFormat::Lzip
                | CreateFormat::Lz4
        )
    }

    /// 按输出文件扩展名判定;未知扩展名/禁区给清晰错误。表按后缀长度降序匹配。
    pub fn from_out(path: &Path) -> anyhow::Result<CreateFormat> {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        let table: &[(&str, CreateFormat)] = &[
            (".tar.gz", CreateFormat::TarGz),
            (".tar.bz2", CreateFormat::TarBz2),
            (".tar.xz", CreateFormat::TarXz),
            (".tar.zst", CreateFormat::TarZst),
            (".tar.lzma", CreateFormat::TarLzma),
            (".tar.lz", CreateFormat::TarLzip),
            (".tar.lz4", CreateFormat::TarLz4),
            (".tgz", CreateFormat::TarGz),
            (".tbz2", CreateFormat::TarBz2),
            (".tbz", CreateFormat::TarBz2),
            (".txz", CreateFormat::TarXz),
            (".tzst", CreateFormat::TarZst),
            (".tar", CreateFormat::Tar),
            (".zip", CreateFormat::Zip),
            (".7z", CreateFormat::SevenZ),
            (".gz", CreateFormat::Gz),
            (".bz2", CreateFormat::Bz2),
            (".xz", CreateFormat::Xz),
            (".zst", CreateFormat::Zst),
            (".lzma", CreateFormat::Lzma),
            (".lz", CreateFormat::Lzip),
            (".lz4", CreateFormat::Lz4),
        ];
        for (suf, f) in table {
            if name.ends_with(suf) {
                return Ok(*f);
            }
        }
        if name.ends_with(".rar") || name.ends_with(".cbr") {
            bail!("RAR 创建是永久禁区(算法闭源,无合法实现):请改用 zip / 7z / tar");
        }
        bail!(
            "无法由扩展名判定创建格式:{name:?}。白名单:tar(.gz/.bz2/.xz/.zst/.lzma/.lz/.lz4)、zip、7z、单流 gz/bz2/xz/zst/lzma/lz/lz4"
        );
    }
}

#[derive(Debug, Clone)]
pub enum WriteKind {
    File(Vec<u8>),
    Dir,
    Symlink(String),
}

#[derive(Debug, Clone)]
pub struct WriteEntry {
    pub name: String, // 目录以 '/' 结尾
    pub kind: WriteKind,
    pub mode: u32,
    pub mtime: Option<u64>, // unix epoch 秒
}

/// 展开磁盘输入为内存条目(目录递归)。归档根 = 输入目录 basename(tar/7z 惯例)。
fn plan_entries(inputs: &[PathBuf]) -> anyhow::Result<Vec<WriteEntry>> {
    let mut out = Vec::new();
    for p in inputs {
        let meta = std::fs::symlink_metadata(p)
            .map_err(|_| anyhow::anyhow!("输入不存在:{}", p.display()))?;
        if meta.is_dir() {
            let root = p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "archive".into());
            let base = p.clone();
            let mut stack = vec![base.clone()];
            while let Some(d) = stack.pop() {
                for e in std::fs::read_dir(&d)? {
                    let epath = e?.path();
                    let em = std::fs::symlink_metadata(&epath)?;
                    let rel = format!("{root}/{}", arc_name(&base, &epath));
                    if em.is_dir() {
                        stack.push(epath.clone());
                        out.push(make_entry(&epath, &em, rel + "/"));
                    } else if em.is_file() || em.is_symlink() {
                        out.push(make_entry(&epath, &em, rel));
                    }
                }
            }
            out.push(make_entry(p, &meta, format!("{root}/")));
        } else if meta.is_file() || meta.is_symlink() {
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push(make_entry(p, &meta, name));
        } else {
            bail!("输入不是普通文件/目录/符号链接:{}", p.display());
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    Ok(out)
}

fn make_entry(path: &Path, meta: &std::fs::Metadata, name: String) -> WriteEntry {
    let mode = perm_mode(&meta.permissions());
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    let kind = if name.ends_with('/') {
        WriteKind::Dir
    } else if meta.is_symlink() {
        WriteKind::Symlink(
            std::fs::read_link(path)
                .map(|t| t.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
    } else {
        WriteKind::File(std::fs::read(path).unwrap_or_default())
    };
    WriteEntry { name, kind, mode, mtime }
}

fn arc_name(base: &Path, f: &Path) -> String {
    let rel = f.strip_prefix(base).unwrap_or(f);
    let s = rel.to_string_lossy().replace('\\', "/");
    s.trim_start_matches('/').to_string()
}

#[cfg(unix)]
fn perm_mode(p: &std::fs::Permissions) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    p.mode() & 0o7777
}
#[cfg(not(unix))]
fn perm_mode(_p: &std::fs::Permissions) -> u32 {
    0o644
}

// ---------------------------------------------------------------- 入口

pub struct CreateOptions<'a> {
    pub out: &'a Path,
    pub inputs: &'a [PathBuf],
    pub password: Option<&'a str>,
    pub overwrite: bool,
}

pub fn create(opts: &CreateOptions) -> anyhow::Result<u64> {
    let fmt = CreateFormat::from_out(opts.out)?;
    let entries = plan_entries(opts.inputs)?;
    write_archive(fmt, opts.out, &entries, opts.password, opts.overwrite)
}

/// 统一落盘管线(格式判定 → 校验 → 临时文件 → rename)。
pub(crate) fn write_archive(
    fmt: CreateFormat,
    out: &Path,
    entries: &[WriteEntry],
    password: Option<&str>,
    overwrite: bool,
) -> anyhow::Result<u64> {
    if out.exists() && !overwrite {
        bail!("输出已存在(用 --overwrite 覆盖):{}", out.display());
    }
    if password.is_some() {
        match fmt {
            CreateFormat::Zip => {}
            CreateFormat::SevenZ => bail!("7z 创建密码(7zAES)未实现:请改用 zip 密码或无密码 7z"),
            other => bail!("创建密码仅支持 zip(WinZip AES);{} 不支持", other.name()),
        }
    }
    if entries.is_empty() {
        bail!("没有可写入的输入条目");
    }
    if fmt.is_single() {
        let files: Vec<_> = entries
            .iter()
            .filter(|e| matches!(e.kind, WriteKind::File(_)))
            .collect();
        if entries.len() != 1 || files.len() != 1 {
            bail!(
                "单流压缩 {} 只接受 1 个文件条目(得到 {} 个);多文件请用 .tar.{} 输出",
                fmt.name(),
                files.len(),
                fmt.name()
            );
        }
    }
    if fmt == CreateFormat::SevenZ
        && entries.iter().any(|e| matches!(e.kind, WriteKind::Symlink(_)))
    {
        bail!("7z 写路径不支持符号链接条目(sevenz-rust2 无 external attributes 支持):请用 tar/zip 输出");
    }
    let tmp = out.with_extension("anyarc-tmp");
    let r = (|| -> anyhow::Result<u64> {
        let mut file = std::fs::File::create(&tmp)?;
        let n = match fmt {
            CreateFormat::Tar => {
                write_tar(&mut file, entries)?;
                file.metadata()?.len()
            }
            CreateFormat::Zip => write_zip(&mut file, entries, password)?,
            CreateFormat::SevenZ => write_7z(&mut file, entries)?,
            c @ (CreateFormat::Gz | CreateFormat::Bz2 | CreateFormat::Xz | CreateFormat::Zst
            | CreateFormat::Lzma | CreateFormat::Lzip | CreateFormat::Lz4) => {
                let WriteKind::File(data) = &entries[0].kind else {
                    unreachable!("单流校验已保证唯一 File 条目");
                };
                write_single(&mut file, c, data)?
            }
            c => {
                let mut tar_bytes = Vec::new();
                write_tar(&mut tar_bytes, entries)?;
                write_tar_comp(&mut file, c, &tar_bytes)?
            }
        };
        Ok(n)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    let n = r?;
    std::fs::rename(&tmp, out)?;
    Ok(n)
}

// ---------------------------------------------------------------- tar 家族

fn write_tar(w: &mut impl Write, entries: &[WriteEntry]) -> anyhow::Result<()> {
    let mut builder = tar::Builder::new(w);
    for e in entries {
        let mut h = tar::Header::new_ustar();
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(e.mtime.unwrap_or(0));
        match &e.kind {
            WriteKind::Dir => {
                h.set_entry_type(tar::EntryType::Directory);
                h.set_mode(e.mode | 0o755);
                h.set_size(0);
                // append_data 自动 set_path + set_cksum
                builder.append_data(&mut h, &e.name, std::io::empty())?;
            }
            WriteKind::Symlink(tgt) => {
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_mode(0o777);
                h.set_size(0);
                builder.append_link(&mut h, &e.name, tgt)?;
            }
            WriteKind::File(data) => {
                h.set_entry_type(tar::EntryType::Regular);
                h.set_mode(e.mode & 0o7777);
                h.set_size(data.len() as u64);
                builder.append_data(&mut h, &e.name, std::io::Cursor::new(data))?;
            }
        }
    }
    builder.into_inner()?;
    Ok(())
}

fn write_tar_comp(file: &mut std::fs::File, fmt: CreateFormat, tar_bytes: &[u8]) -> anyhow::Result<u64> {
    match fmt {
        CreateFormat::TarGz => {
            let mut w = flate2::write::GzEncoder::new(&mut *file, flate2::Compression::default());
            w.write_all(tar_bytes)?;
            w.finish()?;
        }
        CreateFormat::TarBz2 => {
            let mut w = bzip2::write::BzEncoder::new(&mut *file, bzip2::Compression::default());
            w.write_all(tar_bytes)?;
            w.finish()?;
        }
        CreateFormat::TarZst => {
            let mut w = zstd::stream::write::Encoder::new(&mut *file, 3)?;
            w.write_all(tar_bytes)?;
            w.finish()?;
        }
        CreateFormat::TarXz => {
            let mut w = lzma_rust2::XzWriter::new(&mut *file, lzma_rust2::XzOptions::default())
                .map_err(|e| anyhow::anyhow!("xz: {e}"))?;
            w.write_all(tar_bytes).map_err(|e| anyhow::anyhow!("xz: {e}"))?;
            w.finish().map_err(|e| anyhow::anyhow!("xz: {e}"))?;
        }
        CreateFormat::TarLzma => {
            let opts = lzma_rust2::LzmaOptions::default();
            let mut w = lzma_rust2::LzmaWriter::new_use_header(&mut *file, &opts, Some(tar_bytes.len() as u64))
                .map_err(|e| anyhow::anyhow!("lzma: {e}"))?;
            w.write_all(tar_bytes).map_err(|e| anyhow::anyhow!("lzma: {e}"))?;
            w.finish().map_err(|e| anyhow::anyhow!("lzma: {e}"))?;
        }
        CreateFormat::TarLzip => {
            let mut w = lzma_rust2::LzipWriter::new(&mut *file, lzma_rust2::LzipOptions::default());
            w.write_all(tar_bytes).map_err(|e| anyhow::anyhow!("lzip: {e}"))?;
            w.finish().map_err(|e| anyhow::anyhow!("lzip: {e}"))?;
        }
        CreateFormat::TarLz4 => {
            let mut w = lz4_flex::frame::FrameEncoder::new(&mut *file);
            w.write_all(tar_bytes)?;
            w.finish()?;
        }
        _ => unreachable!("write_tar_comp 只接 tar.* 格式"),
    }
    Ok(file.metadata()?.len())
}

// ---------------------------------------------------------------- 单流压缩

fn write_single(file: &mut std::fs::File, fmt: CreateFormat, data: &[u8]) -> anyhow::Result<u64> {
    match fmt {
        CreateFormat::Gz => {
            let mut w = flate2::write::GzEncoder::new(&mut *file, flate2::Compression::default());
            w.write_all(data)?;
            w.finish()?;
        }
        CreateFormat::Bz2 => {
            let mut w = bzip2::write::BzEncoder::new(&mut *file, bzip2::Compression::default());
            w.write_all(data)?;
            w.finish()?;
        }
        CreateFormat::Zst => {
            let mut w = zstd::stream::write::Encoder::new(&mut *file, 3)?;
            w.write_all(data)?;
            w.finish()?;
        }
        CreateFormat::Xz => {
            let mut w = lzma_rust2::XzWriter::new(&mut *file, lzma_rust2::XzOptions::default())
                .map_err(|e| anyhow::anyhow!("xz: {e}"))?;
            w.write_all(data).map_err(|e| anyhow::anyhow!("xz: {e}"))?;
            w.finish().map_err(|e| anyhow::anyhow!("xz: {e}"))?;
        }
        CreateFormat::Lzma => {
            let opts = lzma_rust2::LzmaOptions::default();
            let mut w = lzma_rust2::LzmaWriter::new_use_header(&mut *file, &opts, Some(data.len() as u64))
                .map_err(|e| anyhow::anyhow!("lzma: {e}"))?;
            w.write_all(data).map_err(|e| anyhow::anyhow!("lzma: {e}"))?;
            w.finish().map_err(|e| anyhow::anyhow!("lzma: {e}"))?;
        }
        CreateFormat::Lzip => {
            let mut w = lzma_rust2::LzipWriter::new(&mut *file, lzma_rust2::LzipOptions::default());
            w.write_all(data).map_err(|e| anyhow::anyhow!("lzip: {e}"))?;
            w.finish().map_err(|e| anyhow::anyhow!("lzip: {e}"))?;
        }
        CreateFormat::Lz4 => {
            let mut w = lz4_flex::frame::FrameEncoder::new(&mut *file);
            w.write_all(data)?;
            w.finish()?;
        }
        _ => unreachable!(),
    }
    Ok(file.metadata()?.len())
}

// ---------------------------------------------------------------- zip / 7z

/// unix epoch 秒 → 年月日时分秒(days-from-civil 逆算法,Hinnant)。
fn civil_from_unix(secs: i64) -> (u16, u16, u16, u16, u16, u16) {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400) as u32;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u16;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u16;
    let y = if m <= 2 { y + 1 } else { y };
    (
        y.clamp(1980, 2107) as u16,
        m,
        d,
        (tod / 3600) as u16,
        (tod % 3600 / 60) as u16,
        (tod % 60) as u16,
    )
}

fn zip_time(mtime: Option<u64>) -> zip::DateTime {
    let secs = mtime.unwrap_or(0) as i64;
    let (y, mo, d, hh, mm, ss) = civil_from_unix(secs);
    let y = if y < 1980 { 1980 } else { y };
    zip::DateTime::try_from((u16::from(y) << 9 | mo << 5 | d, hh << 11 | mm << 5 | ss / 2))
        .unwrap_or(zip::DateTime::DEFAULT)
}

fn write_zip(file: &mut std::fs::File, entries: &[WriteEntry], password: Option<&str>) -> anyhow::Result<u64> {
    use zip::write::FileOptions;
    let mut zw = zip::ZipWriter::new(&mut *file);
    for e in entries {
        let mut opts: FileOptions<'_, '_, ()> =
            FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .last_modified_time(zip_time(e.mtime));
        if let Some(pw) = password {
            opts = opts.with_aes_encryption(zip::AesMode::Aes256, pw);
        }
        match &e.kind {
            WriteKind::Dir => zw.add_directory(&e.name, opts)?,
            WriteKind::Symlink(tgt) => {
                let link_opts: FileOptions<'_, '_, ()> =
                    FileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored)
                        .last_modified_time(zip_time(e.mtime));
                zw.add_symlink(&e.name, tgt, link_opts)?;
            }
            WriteKind::File(data) => {
                zw.start_file(&e.name, opts)?;
                zw.write_all(data)?;
            }
        }
    }
    zw.finish()?;
    Ok(file.metadata()?.len())
}

/// unix epoch 秒 → NTFS 100ns ticks(1601 纪元偏移 11644473600s)。
fn nt_time(secs: u64) -> sevenz_rust2::NtTime {
    sevenz_rust2::NtTime::from((secs + 11_644_473_600) * 10_000_000)
}

fn write_7z(file: &mut std::fs::File, entries: &[WriteEntry]) -> anyhow::Result<u64> {
    let mut aw = sevenz_rust2::ArchiveWriter::new(std::io::Cursor::new(Vec::<u8>::new()))?;
    for e in entries {
        let mut entry = match &e.kind {
            WriteKind::Dir => sevenz_rust2::ArchiveEntry::new_directory(&e.name),
            WriteKind::File(data) => {
                let mut en = sevenz_rust2::ArchiveEntry::new_file(&e.name);
                en.size = data.len() as u64;
                en
            }
            WriteKind::Symlink(_) => unreachable!("调用方已拒绝 7z symlink"),
        };
        if let Some(t) = e.mtime {
            entry.has_last_modified_date = true;
            entry.last_modified_date = nt_time(t);
        }
        match &e.kind {
            WriteKind::File(data) => {
                aw.push_archive_entry(entry, Some(std::io::Cursor::new(data)))?;
            }
            _ => {
                aw.push_archive_entry::<std::io::Empty>(entry, None)?;
            }
        }
    }
    let bytes = aw.finish()?.into_inner();
    file.write_all(&bytes)?;
    Ok(bytes.len() as u64)
}

// ---------------------------------------------------------------- repack

pub struct RepackOptions<'a> {
    pub src: &'a Path,
    pub dst: &'a Path,
    /// 源解密密码
    pub password: Option<&'a str>,
    /// 目标加密密码(仅 zip AES;与源密码分离,避免意外加密产物)
    pub write_password: Option<&'a str>,
    pub overwrite: bool,
}

/// 读 `src`(L1–L3 全格式)→ 按 `dst` 扩展名走 create 白名单。
pub fn repack(opts: &RepackOptions) -> anyhow::Result<u64> {
    let fmt = CreateFormat::from_out(opts.dst)?;
    let det = crate::detect::detect(opts.src)?
        .ok_or_else(|| anyhow::anyhow!("无法识别归档格式:{}", opts.src.display()))?;
    let open_opts = crate::backend::OpenOptions {
        password: opts
            .password
            .map(str::to_string)
            .or_else(|| std::env::var("ANYARC_PASSWORD").ok().filter(|s| !s.is_empty())),
        verify_crc: true,
        max_entry_size: Some(4 << 30),
        ..Default::default()
    };
    let mut ar = crate::backend::open(opts.src, &det, &open_opts)?;
    let mut entries = Vec::new();
    while let Some(e) = ar.next_entry()? {
        // File 分支的 read_current 会消费条目;其余必须显式 skip 才能前进
        let consumed = matches!(e.kind, crate::backend::EntryKind::File);
        let kind = match e.kind {
            crate::backend::EntryKind::Directory => WriteKind::Dir,
            crate::backend::EntryKind::Symlink => {
                WriteKind::Symlink(e.link_target.clone().unwrap_or_default())
            }
            crate::backend::EntryKind::HardLink | crate::backend::EntryKind::Special => {
                ar.skip_current()?;
                bail!(
                    "repack v1 不支持 {} 条目:{:?}(hardlink/special 元数据无法跨格式迁移)",
                    if matches!(e.kind, crate::backend::EntryKind::HardLink) { "hardlink" } else { "special" },
                    e.name
                );
            }
            crate::backend::EntryKind::File => {
                if e.encrypted
                    && opts.password.is_none()
                    && std::env::var("ANYARC_PASSWORD").map(|s| s.is_empty()).unwrap_or(true)
                {
                    bail!("源归档含加密条目,repack 需要密码(--password / ANYARC_PASSWORD)");
                }
                WriteKind::File(ar.read_current()?)
            }
        };
        entries.push(WriteEntry { name: e.name, kind, mode: 0o644, mtime: e.mtime_epoch });
        if !consumed {
            ar.skip_current()?;
        }
    }
    // 单流源解压出的条目若为 tar(复合已被展开)则条目名保留树形;
    // 单条目 tar→单流 反向打包时对齐命名(去掉解压产生的扩展名)
    if fmt.is_single() && entries.len() == 1 && matches!(entries[0].kind, WriteKind::File(_)) {
        // 保持条目名无关(单流不存名字),无需处理
    }
    write_archive(fmt, opts.dst, &entries, opts.write_password, opts.overwrite)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt_of(name: &str) -> CreateFormat {
        CreateFormat::from_out(Path::new(name)).unwrap()
    }

    #[test]
    fn ext_routing_prefers_longer_suffixes() {
        assert_eq!(fmt_of("a.tar.lz"), CreateFormat::TarLzip); // 不是 .lz 单流
        assert_eq!(fmt_of("a.lz"), CreateFormat::Lzip);
        assert_eq!(fmt_of("a.tar.lzma"), CreateFormat::TarLzma);
        assert_eq!(fmt_of("a.lzma"), CreateFormat::Lzma);
        assert_eq!(fmt_of("a.TGZ"), CreateFormat::TarGz); // 大小写不敏感
        assert_eq!(fmt_of("x.tar.zst"), CreateFormat::TarZst);
        assert_eq!(fmt_of("x.7z"), CreateFormat::SevenZ);
        assert!(CreateFormat::Gz.is_single() && !CreateFormat::TarGz.is_single());
    }

    #[test]
    fn forbidden_and_unknown_extensions() {
        assert!(CreateFormat::from_out(Path::new("a.rar")).is_err());
        assert!(CreateFormat::from_out(Path::new("a.cbr")).is_err());
        assert!(CreateFormat::from_out(Path::new("a.foo")).is_err());
        // RAR 错误信息须含"永久禁区"指引
        let e = CreateFormat::from_out(Path::new("a.rar")).unwrap_err().to_string();
        assert!(e.contains("永久禁区"), "{e}");
    }

    #[test]
    fn plan_entries_dir_uses_basename_root() {
        let uniq = format!("anyarc_create_test_{}_{}", std::process::id(), line!());
        let root = std::env::temp_dir().join(uniq);
        let _ = std::fs::remove_dir_all(&root);
        let d = root.join("pkg");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("f.txt"), b"x").unwrap();
        std::fs::write(d.join("sub/g.txt"), b"y").unwrap();
        let ents = plan_entries(&[d.clone()]).unwrap();
        let names: Vec<&str> = ents.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"pkg/f.txt"));
        assert!(names.contains(&"pkg/sub/g.txt"));
        assert!(names.contains(&"pkg/sub/"));
        assert!(names.contains(&"pkg/"));
        // 单文件输入 → basename 条目
        let one = plan_entries(&[d.join("f.txt")]).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].name, "f.txt");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn civil_from_unix_roundtrip() {
        // 2000-01-01 00:00:00 UTC = 946684800
        assert_eq!(civil_from_unix(946_684_800), (2000, 1, 1, 0, 0, 0));
        // 2026-10-31 12:34:56 UTC = 1793450096
        let t = 1_793_450_096;
        let (y, mo, d, hh, mm, ss) = civil_from_unix(t);
        assert_eq!((y, mo, d), (2026, 10, 31), "{y}-{mo}-{d}");
        assert_eq!((hh, mm, ss), (12, 34, 56));
        // zip epoch 前钳制
        let (y, ..) = civil_from_unix(0);
        assert!(y <= 1980);
    }
}
