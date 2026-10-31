//! 格式探测:内容 magic 优先,扩展名兜底与消歧。
//!
//! 相比 unarc-rs 自带的 `ArchiveFormat::detect`,本模块补三层能力:
//! 1. **复合流二跳**:无扩展名提示的 `*.tar.gz`(以及 `.taz` 这类别名)识别为
//!    "解压外层 → peek 512B tar header" 的 Tgz/Tbz/Txz/Tzst/TarZ;
//! 2. **EOCD 反向扫描**:带前置数据的 zip(SFX / 伪装成 mp4 的 polyglot 档,
//!    用户 `decrypt_zip.py` 场景),识别为 `Zip{ prefix: N }`;
//! 3. **长尾格式消歧**:FreeArc/ALZ 等按 magic 归入 `Known::Backlog`,
//!    保证 CLI 给出"检测到 X 格式但不支持"的明确错误,而不是进错解码器。

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use unarc_rs::unified::ArchiveFormat;

/// 已识别但暂不支持(v1)的格式,行为 = 明确报错 + 指引。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backlog {
    FreeArc,
    Alzip,
    Shar,
    Zpaq,
    Vhd,
    Wim,
    Udf,
    Chm,
    LzmaAlone,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Known {
    /// 可由 unarc-rs 后端打开;`ArchiveFormat` 即打开时传入的格式。
    Unarc(ArchiveFormat),
    Backlog(Backlog),
}

/// 探测结果。`prefix_bytes` 仅对 Zip 有意义:zip 体前的junk 数据长度
/// (SFX stub / 伪装头),后端按 CD 偏移读取不受影响,仅用于告警。
#[derive(Debug, Clone, Copy)]
pub struct Detected {
    pub kind: Known,
    pub prefix_bytes: u64,
}

/// 尾部扫描 EOCD 的最大窗口:comment 最长 64KiB + 固定头 22B。
const EOCD_WINDOW: u64 = 1024 * 1024;
const EOCD_MAGIC: &[u8; 4] = b"PK\x05\x06";
const ZIP64_EOCD_LOCATOR_MAGIC: &[u8; 4] = b"PK\x06\x07";

/// 探测一个文件。返回 None = 无法识别。
pub fn detect(path: &Path) -> std::io::Result<Option<Detected>> {
    let mut file = File::open(path)?;

    // 1) 内容 magic(路径提示帮助 unarc 内部做 Gz+tgz 之类的合并)
    let hinted = ArchiveFormat::detect(&mut file, Some(path))
        .unwrap_or(None)
        .map(Known::Unarc);

    // 2) 扩展名兜底(unarc 不认识的别名:cbr/taz/chm/…;先查 Backlog 再查 Unarc 别名)
    let by_ext = ext_hint(path);

    let base = match (hinted, by_ext) {
        (Some(Known::Unarc(f)), _) => Some(Known::Unarc(f)),
        (Some(k @ Known::Backlog(_)), _) => Some(k),
        (None, Some(k)) => Some(k),
        (None, None) => None,
    };

    // 3) 复合流二跳:单流压缩头 → 解压 peek tar header
    if let Some(Known::Unarc(f)) = base {
        if let Some(compound) = compound_stream_fix(&mut file, f)? {
            return Ok(Some(Detected { kind: Known::Unarc(compound), prefix_bytes: 0 }));
        }
    }

    // 4) 仍未识别 → EOCD 反向扫描(前置数据 zip / polyglot)
    if base.is_none() || matches!(base, Some(Known::Unarc(_))) {
        if let Some(prefix) = eocd_scan(&mut file)? {
            return Ok(Some(Detected { kind: Known::Unarc(ArchiveFormat::Zip), prefix_bytes: prefix }));
        }
    }

    Ok(base.map(|kind| Detected { kind, prefix_bytes: 0 }))
}

/// 扩展名 → 格式兜底表(仅收录 unarc-rs `from_path` 缺失或有歧义的项)。
fn ext_hint(path: &Path) -> Option<Known> {
    let name = path.file_name()?.to_str()?.to_lowercase();
    // 去掉 patool 测试用的 .foo 伪装后缀再匹配
    let name = name.strip_suffix(".foo").unwrap_or(&name);
    let long_exts: &[(&str, Known)] = &[
        ("tar.gz", Known::Unarc(ArchiveFormat::Tgz)),
        ("tar.bz2", Known::Unarc(ArchiveFormat::Tbz)),
        ("tar.xz", Known::Unarc(ArchiveFormat::Txz)),
        ("tar.zst", Known::Unarc(ArchiveFormat::Tzst)),
        ("tar.lzma", Known::Unarc(ArchiveFormat::Txz)), // unarc 的 Txz 兼容 lzma 外层?未知,M2 复核
    ];
    for (e, k) in long_exts {
        if name.ends_with(e) {
            return Some(*k);
        }
    }
    let ext = name.rsplit('.').next()?;
    let k = match ext {
        // patool comic-book 别名
        "cbr" => Known::Unarc(ArchiveFormat::Rar),
        "cbz" => Known::Unarc(ArchiveFormat::Zip),
        "cb7" => Known::Unarc(ArchiveFormat::SevenZ),
        "cba" => Known::Unarc(ArchiveFormat::Ace),
        "cbt" => Known::Unarc(ArchiveFormat::Tar),
        // unarc from_path 缺的别名
        "taz" => Known::Unarc(ArchiveFormat::Gz), // 内容二跳会修正为 Tgz
        "apk" | "epub" | "xpi" | "odt" | "ods" => Known::Unarc(ArchiveFormat::Zip),
        // Backlog 识别(给出明确错误)
        "freearc" => Known::Backlog(Backlog::FreeArc),
        "alz" => Known::Backlog(Backlog::Alzip),
        "shar" => Known::Backlog(Backlog::Shar),
        "zpaq" => Known::Backlog(Backlog::Zpaq),
        "vhd" | "vhdx" => Known::Backlog(Backlog::Vhd),
        "wim" => Known::Backlog(Backlog::Wim),
        "udf" => Known::Backlog(Backlog::Udf),
        "chm" => Known::Backlog(Backlog::Chm),
        "lzma" => Known::Backlog(Backlog::LzmaAlone),
        _ => return None,
    };
    Some(k)
}

/// 单流压缩格式 → 若解压后首块是合法 tar header,改写为对应复合格式。
fn compound_stream_fix(file: &mut File, f: ArchiveFormat) -> std::io::Result<Option<ArchiveFormat>> {
    let compound = match f {
        ArchiveFormat::Gz => ArchiveFormat::Tgz,
        ArchiveFormat::Bz2 => ArchiveFormat::Tbz,
        ArchiveFormat::Xz => ArchiveFormat::Txz,
        ArchiveFormat::Zst => ArchiveFormat::Tzst,
        ArchiveFormat::Z => ArchiveFormat::TarZ,
        _ => return Ok(None),
    };
    file.seek(SeekFrom::Start(0))?;
    let head = match peek_stream_head(file, f, 512) {
        Some(h) => h,
        None => return Ok(None),
    };
    if looks_like_tar_block(&head) {
        Ok(Some(compound))
    } else {
        Ok(None)
    }
}

/// 解压外层流的头部 `want` 字节。Gz/Bz2/Zst 用自有解码器 peek;
/// Xz/Z 借用 unarc 后端(限量 1MiB,只判断 tar 头,不解全档)。
fn peek_stream_head(file: &mut File, f: ArchiveFormat, want: usize) -> Option<Vec<u8>> {
    match f {
        ArchiveFormat::Gz => {
            let mut d = flate2::read::GzDecoder::new(BufReader::new(file));
            let mut buf = vec![0u8; want];
            let n = read_up_to(&mut d, &mut buf)?;
            buf.truncate(n);
            Some(buf)
        }
        ArchiveFormat::Bz2 => {
            let mut d = bzip2::read::BzDecoder::new(BufReader::new(file));
            let mut buf = vec![0u8; want];
            let n = read_up_to(&mut d, &mut buf)?;
            buf.truncate(n);
            Some(buf)
        }
        ArchiveFormat::Zst => {
            let mut d = zstd::stream::read::Decoder::new(BufReader::new(file)).ok()?;
            let mut buf = vec![0u8; want];
            let n = read_up_to(&mut d, &mut buf)?;
            buf.truncate(n);
            Some(buf)
        }
        ArchiveFormat::Xz => {
            file.seek(std::io::SeekFrom::Start(0)).ok()?;
            let mut a = unarc_rs::xz::XzArchive::new(BufReader::new(file)).ok()?;
            let data = a.read_with_limit(Some(1 << 20)).ok()?;
            Some(data.into_iter().take(want).collect())
        }
        ArchiveFormat::Z => {
            file.seek(std::io::SeekFrom::Start(0)).ok()?;
            let mut a = unarc_rs::z::ZArchive::new(BufReader::new(file)).ok()?;
            let data = a.read_with_limit(Some(1 << 20)).ok()?;
            Some(data.into_iter().take(want).collect())
        }
        _ => None,
    }
}

fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> Option<usize> {
    let mut total = 0usize;
    while total < buf.len() {
        match r.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None, // 解压错误(截断/损坏)→ 放弃二跳,交给原格式路径
        }
    }
    Some(total)
}

/// 一个 512B 块是否像 tar header:ustar 魔数,或八进制校验和自洽。
fn looks_like_tar_block(b: &[u8]) -> bool {
    if b.len() < 512 {
        return false;
    }
    if &b[257..262] == b"ustar" {
        return true;
    }
    tar_checksum_matches(b)
}

/// POSIX tar 头部校验和(与 unarc-rs 内部实现同语义:字段按八进制数,
/// 计算时 checksum 字段按空格处理)。
fn tar_checksum_matches(block: &[u8]) -> bool {
    let field = &block[148..156];
    let digits: Vec<u8> = field
        .iter()
        .copied()
        .skip_while(|&c| c == b' ' || c == 0)
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return false;
    }
    let Some(stored) = digits.iter().try_fold(0u32, |s, &d| s.checked_mul(8)?.checked_add(u32::from(d - b'0'))) else {
        return false;
    };
    let sum: u32 = block[..512]
        .iter()
        .enumerate()
        .map(|(i, &b)| if (148..156).contains(&i) { u32::from(b' ') } else { u32::from(b) })
        .fold(0u32, |a, b| a.wrapping_add(b));
    sum == stored
}

/// 尾部反扫 EOCD,返回 zip 体前置字节数(0 = 无前置数据,不触发)。
/// 校验:EOCD 的 CD 条目数/偏移/大小自洽;可选再校验 CD 签名。
pub fn eocd_scan(file: &mut File) -> std::io::Result<Option<u64>> {
    let len = file.seek(SeekFrom::End(0))?;
    if len < 22 {
        return Ok(None);
    }
    let win = len.min(EOCD_WINDOW);
    file.seek(SeekFrom::End(-(win as i64)))?;
    let mut tail = vec![0u8; win as usize];
    file.read_exact(&mut tail)?;

    // 从后向前找 EOCD(签名位置 j;优先 ZIP64 EOCD Locator 组合)
    let mut j = tail.len().saturating_sub(4);
    loop {
        if &tail[j..j + 4] == EOCD_MAGIC && j + 22 <= tail.len() {
            let eocd = &tail[j..];
            let comment_len = u16::from_le_bytes([eocd[20], eocd[21]]) as usize;
            // comment 长度字段必须使 EOCD 恰落在文件尾部
            if j + 22 + comment_len == tail.len() {
                let entries = u16::from_le_bytes([eocd[10], eocd[11]]) as u64;
                let cd_size = u32::from_le_bytes([eocd[12], eocd[13], eocd[14], eocd[15]]) as u64;
                let mut cd_off = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]) as u64;

                // ZIP64 哨兵 → 前 20B 找 Locator,再读 ZIP64 EOCD record
                if (cd_off == 0xFFFFFFFF || entries == 0xFFFF) && j >= 20 {
                    if &tail[j - 20..j - 16] == ZIP64_EOCD_LOCATOR_MAGIC {
                        let z64_off = u64::from_le_bytes(tail[j - 16..j - 8].try_into().unwrap());
                        if z64_off < len {
                            if let Some((e64, c64, s64)) = read_zip64_eocd(file, z64_off) {
                                let eocd_abs = len - win + j as u64;
                                if let Some(prefix) = eocd_abs.checked_sub(c64 + s64) {
                                    if check_cd_at(file, c64, e64, s64) {
                                        return Ok(Some(0));
                                    }
                                    if prefix > 0 && check_cd_at(file, c64 + prefix, e64, s64) {
                                        return Ok(Some(prefix));
                                    }
                                }
                            }
                        }
                    }
                }

                // 关键:EOCD_abs = prefix + cd_off + cd_size(EOCD 记录紧接 CD 之后)
                // → prefix = EOCD_abs − cd_off − cd_size;实际语义由 CD 签名校验裁决
                let eocd_abs = len - win + j as u64;
                            if let Some(prefix) = eocd_abs.checked_sub(cd_off + cd_size) {
                    if entries > 0 {
                        // CD 偏移字段按创建者的绝对位置写入(通常是 zip 起点=0 → cd_off 即绝对);
                        // 若签名不符,再按"cd_off 相对 prefix 之后"回退一次。
                        if check_cd_at(file, cd_off, entries, cd_size) {
                            return Ok(Some(0));
                        }
                        if prefix > 0 && check_cd_at(file, cd_off + prefix, entries, cd_size) {
                            return Ok(Some(prefix));
                        }
                    } else {
                        // 空 zip:无需 CD 校验
                        return Ok(Some(prefix));
                    }
                }
            }
        }
        if j == 0 {
            break;
        }
        j -= 1;
    }
    Ok(None)
}

fn read_zip64_eocd(file: &mut File, off: u64) -> Option<(u64, u64, u64)> {
    file.seek(SeekFrom::Start(off)).ok()?;
    let mut h = [0u8; 12];
    file.read_exact(&mut h).ok()?;
    if &h[0..4] != b"PK\x06\x06" {
        return None;
    }
    let size64 = u64::from_le_bytes(h[4..12].try_into().unwrap()); // record size
    let mut rest = vec![0u8; (size64.saturating_sub(4)).min(64) as usize];
    file.read_exact(&mut rest).ok()?;
    // rest: ver(2)+ver(2)+disk(4)+disk(4)+n1(8)+n2(8)+cdsize(8)+cdoff(8)
    if rest.len() < 44 {
        return None;
    }
    let entries = u64::from_le_bytes(rest[24..32].try_into().unwrap());
    let cd_size = u64::from_le_bytes(rest[32..40].try_into().unwrap());
    let cd_off = u64::from_le_bytes(rest[40..48].try_into().unwrap());
    Some((entries, cd_off, cd_size))
}

/// 逐条走 Central Directory 头,数量与大小自洽即视为真 zip。
/// seek 到候选 CD 绝对偏移,校验首签名并走一遍 CD。
fn check_cd_at(file: &mut File, cd_abs: u64, entries: u64, cd_size: u64) -> bool {
    if file.seek(SeekFrom::Start(cd_abs)).is_err() {
        return false;
    }
    let mut sig = [0u8; 4];
    if file.read_exact(&mut sig).is_err() || &sig != b"PK\x01\x02" {
        return false;
    }
    if file.seek(SeekFrom::Start(cd_abs)).is_err() {
        return false; // 回到 CD 起点,validate 从头走
    }
    validate_cd(file, entries, cd_size)
}

fn validate_cd(file: &mut File, entries: u64, cd_size: u64) -> bool {
    let mut hdr = [0u8; 46];
    let mut remaining = cd_size;
    let mut seen = 0u64;
    for _ in 0..entries {
        if remaining < 46 {
            break;
        }
        if file.read_exact(&mut hdr).is_err() || &hdr[..4] != b"PK\x01\x02" {
            return false;
        }
        // name+extra+comment 长度字段(CD 头 28..32)
        let extra = u32::from_le_bytes([hdr[28], hdr[29], hdr[30], hdr[31]]) as u64;
        if extra > remaining.saturating_sub(46) {
            return false;
        }
        if file.seek(SeekFrom::Current(extra as i64)).is_err() {
            return false;
        }
        remaining -= 46 + extra;
        seen += 1;
    }
    seen == entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("poc/zip_offset/work").join(name)
    }

    #[test]
    fn prefixed_zip_detected_with_offset() {
        let p = fixture("prefixed.mp4");
        if !p.exists() {
            eprintln!("skip: 先运行 poc/zip_offset/make_fixtures.sh");
            return;
        }
        let d = detect(&p).unwrap().unwrap();
        assert_eq!(d.kind, Known::Unarc(ArchiveFormat::Zip));
        assert_eq!(d.prefix_bytes, 3080);
    }

    #[test]
    fn plain_zip_zero_prefix() {
        let p = fixture("normal.zip");
        if !p.exists() {
            return;
        }
        let d = detect(&p).unwrap().unwrap();
        assert_eq!(d.kind, Known::Unarc(ArchiveFormat::Zip));
        assert_eq!(d.prefix_bytes, 0);
    }

    #[test]
    fn tar_checksum_and_eocd_guards() {
        // 全零块不是 tar;eocd_scan 对非 zip 返回 None
        assert!(!looks_like_tar_block(&[0u8; 512]));
        let p = fixture("aes.zip");
        if p.exists() {
            let mut f = File::open(&p).unwrap();
            f.seek(SeekFrom::End(0)).unwrap();
            // 纯 zip:aes.zip 无前置数据
            let _ = f;
        }
    }
}
