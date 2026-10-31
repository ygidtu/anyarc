//! `anyarc create` 写路径。**白名单**(PLAN §6):tar 家族、zip、7z、
//! 单流压缩(gz/bz2/xz/zst/lzma/lzip/lz4)。白名单外一律清晰报错;RAR create 永久禁区。
//!
//! 单流压缩语义:输入**恰好一个文件**;多输入用 `.tar.*` 输出(自动容器化)。
//! 先写临时文件再 rename,失败不留半截输出。

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

/// 展开输入(目录递归),得到 (磁盘路径, 归档内名字)。
/// 归档名 = 相对路径风格;绝对路径去掉根前缀(与 tar/7z 习惯一致)。
fn plan_entries(inputs: &[PathBuf]) -> anyhow::Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    for p in inputs {
        let meta = std::fs::symlink_metadata(p).map_err(|_| anyhow::anyhow!("输入不存在:{}", p.display()))?;
        if meta.is_dir() {
            // 归档根 = 输入目录的 basename(tar/7z 惯例),条目名带该前缀
            let root = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "archive".into());
            let base = p.clone();
            let mut stack = vec![base.clone()];
            while let Some(d) = stack.pop() {
                for e in std::fs::read_dir(&d)? {
                    let epath = e?.path();
                    let em = std::fs::symlink_metadata(&epath)?;
                    if em.is_dir() {
                        stack.push(epath.clone());
                        out.push((epath.clone(), format!("{root}/{}", arc_name(&base, &epath)) + "/"));
                    } else if em.is_file() || em.is_symlink() {
                        out.push((epath.clone(), format!("{root}/{}", arc_name(&base, &epath))));
                    }
                }
            }
            out.push((base.clone(), format!("{root}/")));
        } else if meta.is_file() || meta.is_symlink() {
            let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            out.push((p.clone(), name));
        } else {
            bail!("输入不是普通文件/目录/符号链接:{}", p.display());
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out.dedup_by(|a, b| a.1 == b.1);
    Ok(out)
}

fn arc_name(base: &Path, f: &Path) -> String {
    let rel = f.strip_prefix(base).unwrap_or(f);
    let s = rel.to_string_lossy().replace('\\', "/");
    s.trim_start_matches('/').to_string()
}

pub struct CreateOptions<'a> {
    pub out: &'a Path,
    pub inputs: &'a [PathBuf],
    pub password: Option<&'a str>,
    pub overwrite: bool,
}

pub fn create(opts: &CreateOptions) -> anyhow::Result<u64> {
    let fmt = CreateFormat::from_out(opts.out)?;
    if opts.out.exists() && !opts.overwrite {
        bail!("输出已存在(用 --overwrite 覆盖):{}", opts.out.display());
    }
    if opts.password.is_some() {
        match fmt {
            CreateFormat::Zip => {}
            CreateFormat::SevenZ => bail!("7z 创建密码(7zAES)未实现:请改用 zip 密码或无密码 7z"),
            other => bail!("创建密码仅支持 zip(WinZip AES);{} 不支持", other.name()),
        }
    }
    let entries = plan_entries(opts.inputs)?;
    if entries.is_empty() {
        bail!("没有可写入的输入条目");
    }
    if fmt.is_single() {
        let files: Vec<_> = entries.iter().filter(|(_, n)| !n.ends_with('/')).collect();
        if files.len() != 1 {
            bail!(
                "单流压缩 {} 只接受 1 个输入文件(得到 {} 个);多文件请用 .tar.{} 输出",
                fmt.name(),
                files.len(),
                fmt.name()
            );
        }
    }
    let tmp = opts.out.with_extension("anyarc-tmp");
    let mut file = std::fs::File::create(&tmp)?;
    let r = match fmt {
        CreateFormat::Tar => {
            write_tar(&mut file, &entries)?;
            file.metadata()?.len()
        }
        CreateFormat::Zip => write_zip(&mut file, &entries, opts.password)?,
        CreateFormat::SevenZ => write_7z(&mut file, &entries, opts.password)?,
        CreateFormat::Gz | CreateFormat::Bz2 | CreateFormat::Xz | CreateFormat::Zst | CreateFormat::Lzma
        | CreateFormat::Lzip | CreateFormat::Lz4 => write_single(&mut file, fmt, &entries[0].0)?,
        // tar + 压缩层
        c => {
            let tar_bytes = build_tar(&entries)?;
            write_tar_comp(&mut file, c, &tar_bytes)?
        }
    };
    drop(file);
    std::fs::rename(&tmp, opts.out)?;
    Ok(r)
}

// ---------------------------------------------------------------- tar 家族

fn build_tar(entries: &[(PathBuf, String)]) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    write_tar(&mut buf, entries)?;
    Ok(buf)
}

fn write_tar(w: &mut impl Write, entries: &[(PathBuf, String)]) -> anyhow::Result<()> {
    let mut builder = tar::Builder::new(w);
    for (path, name) in entries {
        let meta = std::fs::symlink_metadata(path)?;
        if name.ends_with('/') {
            // tar crate 签名:append_dir(归档内名字, 磁盘路径)
            builder.append_dir(name, path)?;
        } else if meta.is_symlink() {
            let tgt = std::fs::read_link(path)?;
            let mut h = tar::Header::new_ustar();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_mode(0o777);
            h.set_size(0);
            h.set_uid(0);
            h.set_gid(0);
            set_mtime(&mut h, path);
            // append_link 自动处理归档路径/链接目标(GNU 长名)+ set_cksum
            builder.append_link(&mut h, name, tgt)?;
        } else {
            let mut f = std::fs::File::open(path)?;
            builder.append_file(name, &mut f)?;
        }
    }
    builder.into_inner()?;
    Ok(())
}

fn set_mtime(h: &mut tar::Header, path: &Path) {
    let t = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    h.set_mtime(t);
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

fn write_single(file: &mut std::fs::File, fmt: CreateFormat, input: &Path) -> anyhow::Result<u64> {
    let mut data = Vec::new();
    std::fs::File::open(input)?.read_to_end(&mut data)?;
    match fmt {
        CreateFormat::Gz => {
            let mut w = flate2::write::GzEncoder::new(&mut *file, flate2::Compression::default());
            w.write_all(&data)?;
            w.finish()?;
        }
        CreateFormat::Bz2 => {
            let mut w = bzip2::write::BzEncoder::new(&mut *file, bzip2::Compression::default());
            w.write_all(&data)?;
            w.finish()?;
        }
        CreateFormat::Zst => {
            let mut w = zstd::stream::write::Encoder::new(&mut *file, 3)?;
            w.write_all(&data)?;
            w.finish()?;
        }
        CreateFormat::Xz => {
            let mut w = lzma_rust2::XzWriter::new(&mut *file, lzma_rust2::XzOptions::default())
                .map_err(|e| anyhow::anyhow!("xz: {e}"))?;
            w.write_all(&data).map_err(|e| anyhow::anyhow!("xz: {e}"))?;
            w.finish().map_err(|e| anyhow::anyhow!("xz: {e}"))?;
        }
        CreateFormat::Lzma => {
            let opts = lzma_rust2::LzmaOptions::default();
            let mut w = lzma_rust2::LzmaWriter::new_use_header(&mut *file, &opts, Some(data.len() as u64))
                .map_err(|e| anyhow::anyhow!("lzma: {e}"))?;
            w.write_all(&data).map_err(|e| anyhow::anyhow!("lzma: {e}"))?;
            w.finish().map_err(|e| anyhow::anyhow!("lzma: {e}"))?;
        }
        CreateFormat::Lzip => {
            let mut w = lzma_rust2::LzipWriter::new(&mut *file, lzma_rust2::LzipOptions::default());
            w.write_all(&data).map_err(|e| anyhow::anyhow!("lzip: {e}"))?;
            w.finish().map_err(|e| anyhow::anyhow!("lzip: {e}"))?;
        }
        CreateFormat::Lz4 => {
            let mut w = lz4_flex::frame::FrameEncoder::new(&mut *file);
            w.write_all(&data)?;
            w.finish()?;
        }
        _ => unreachable!(),
    }
    Ok(file.metadata()?.len())
}

// ---------------------------------------------------------------- zip / 7z

fn write_zip(file: &mut std::fs::File, entries: &[(PathBuf, String)], password: Option<&str>) -> anyhow::Result<u64> {
    use zip::write::FileOptions;
    let mut zw = zip::ZipWriter::new(&mut *file);
    for (path, name) in entries {
        let meta = std::fs::symlink_metadata(path)?;
        let mut opts: zip::write::FileOptions<'_, '_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        if let Some(pw) = password {
            opts = opts.with_aes_encryption(zip::AesMode::Aes256, pw);
        }
        if name.ends_with('/') {
            zw.add_directory(name, opts)?;
        } else if meta.is_symlink() {
            let tgt = std::fs::read_link(path)?;
            let link_opts: zip::write::FileOptions<'_, '_, ()> =
                FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zw.add_symlink(name, tgt.to_string_lossy().as_ref(), link_opts)?;
        } else {
            let data = std::fs::read(path)?;
            zw.start_file(name, opts)?;
            zw.write_all(&data)?;
        }
    }
    zw.finish()?;
    Ok(file.metadata()?.len())
}

fn write_7z(file: &mut std::fs::File, entries: &[(PathBuf, String)], _password: Option<&str>) -> anyhow::Result<u64> {
    let mut aw = sevenz_rust2::ArchiveWriter::new(std::io::Cursor::new(Vec::<u8>::new()))?;
    for (path, name) in entries {
        if name.ends_with('/') {
            let entry = sevenz_rust2::ArchiveEntry::new_directory(name);
            aw.push_archive_entry::<std::io::Empty>(entry, None)?;
        } else {
            let entry = sevenz_rust2::ArchiveEntry::from_path(path, name.clone());
            let f = std::fs::File::open(path)?;
            aw.push_archive_entry(entry, Some(std::io::BufReader::new(f)))?;
        }
    }
    let bytes = aw.finish()?.into_inner();
    file.write_all(&bytes)?;
    Ok(bytes.len() as u64)
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
        let names: Vec<&str> = ents.iter().map(|(_, n)| n.as_str()).collect();
        assert!(names.contains(&"pkg/f.txt"));
        assert!(names.contains(&"pkg/sub/g.txt"));
        assert!(names.contains(&"pkg/sub/"));
        assert!(names.contains(&"pkg/"));
        // 单文件输入 → basename 条目
        let one = plan_entries(&[d.join("f.txt")]).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].1, "f.txt");
        let _ = std::fs::remove_dir_all(&root);
    }
}
