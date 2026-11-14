//! 安全落盘:Zip-slip 防护、符号链接逃逸检查、覆盖策略、权限还原、进度输出。

use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Component, Path, PathBuf};

/// 把归档内条目名解析到 dest 下的安全路径。
/// 拒绝:绝对路径、盘符、`..` 逃逸。返回 None = 应跳过该条目(由调用方计数告警)。
pub fn safe_join(dest: &Path, name: &str) -> Option<PathBuf> {
    let rel = normalize_rel(name)?;
    let joined = dest.join(&rel);
    // 词法规范化后再确认不出 dest(Path::starts_with 按组件比较,不会误判前缀)
    let dest_n = lexically_normalize(dest);
    if !lexically_normalize(&joined).starts_with(&dest_n) {
        return None;
    }
    Some(joined)
}

/// 是否逃逸 dest(用于符号链接目标检查)。
pub fn link_target_escapes(dest: &Path, entry_dir: &Path, target: &str) -> bool {
    let base = if Path::new(target).is_absolute() {
        // 绝对链接:落在 dest 内才允许(参考 GNU tar --same-owner 的保守策略)
        PathBuf::from(target)
    } else {
        entry_dir.join(target)
    };
    let dest_n = lexically_normalize(dest);
    !lexically_normalize(&base).starts_with(&dest_n)
}

fn normalize_rel(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() || name.starts_with('/') || name.contains(':') {
        return None;
    }
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            // ParentDir / RootDir / Prefix 一律视为逃逸
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        return None;
    }
    Some(out)
}

/// 纯词法规范化(解析 `.`/`..`,不要求路径存在)。
fn lexically_normalize(p: &Path) -> PathBuf {
    let mut stack: Vec<Component> = Vec::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if matches!(stack.last(), Some(Component::Normal(_))) {
                    stack.pop();
                } else {
                    stack.push(c);
                }
            }
            Component::CurDir => {}
            _ => stack.push(c),
        }
    }
    stack.iter().collect()
}

pub fn is_tty() -> bool {
    std::io::stdout().is_terminal()
}

/// --smart 规则 B:目录里只剩**唯一一项且与目录同名**(`novel/novel`)时,
/// 把该项提升到目录同级、删除空壳目录 —— 提升后条目路径恰好接管原目录路径。
/// 成功返回 True(目录路径语义已变:目录→条目)。
pub fn collapse_dup_wrapper(dir: &Path) -> bool {
    let Some(name) = dir.file_name() else {
        return false; // "." 等无名目录不做提升
    };
    let Some(parent) = dir.parent() else {
        return false;
    };
    if !dir.is_dir() {
        return false;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    let entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    if entries.len() != 1 || entries[0].file_name() != Some(name) {
        return false;
    }
    // 暂名 → 删空壳 → 归位(处理"提升到自身路径"的名字冲突)
    let tmp = parent.join(format!(".anyarc_lift_{}", std::process::id()));
    if std::fs::rename(&entries[0], &tmp).is_err() {
        return false;
    }
    if std::fs::remove_dir(dir).is_err() {
        let _ = std::fs::rename(&tmp, &entries[0]);
        return false;
    }
    std::fs::rename(&tmp, dir).is_ok()
}

/// --smart 完整后处理:A(同名目录包裹合并)+ B(单条目同名提升);
/// 返回路径重映射后的条目列表。
pub fn smart_adjust(dest: &Path, written: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut w = flatten_dup_wrappers(dest, written);
    if collapse_dup_wrapper(dest) {
        // dest 由"目录"变成"条目本身":原 dest/<同名>/… 路径全部改挂 dest
        let inner_root = dest.join(dest.file_name().unwrap_or_default());
        w = w
            .into_iter()
            .map(|e| {
                if e == inner_root {
                    dest.to_path_buf()
                } else if let Ok(r) = e.strip_prefix(&inner_root) {
                    dest.join(r)
                } else {
                    e
                }
            })
            .collect();
    }
    w
}

/// --smart:本层解压内容若整体位于一个与上级目录**同名**的文件夹内
/// (典型:book.tar.gz 解出 book/,而它已在 book/ 下 → book/book/),把内容
/// 上移一层、删掉重复包裹。冲突或异常时保持原样。返回更新后的路径列表。
pub fn flatten_dup_wrappers(dest: &Path, written: Vec<PathBuf>) -> Vec<PathBuf> {
    if written.is_empty() {
        return written;
    }
    let Some(dest_name) = dest.file_name().map(|s| s.to_string_lossy().into_owned()) else {
        return written; // 目标是 . 之类无名字录,不存在同名包裹
    };
    let Some(top) = written
        .iter()
        .find_map(|e| e.strip_prefix(dest).ok())
        .and_then(|rel| rel.components().next())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
    else {
        return written;
    };
    if top != dest_name {
        return written;
    }
    let t_dir = dest.join(&top);
    if !t_dir.is_dir() || !written.iter().all(|e| e.starts_with(&t_dir)) {
        return written;
    }
    // 全部上移;任何命名冲突则整体放弃(不半途移动)
    let Ok(entries) = std::fs::read_dir(&t_dir) else {
        return written;
    };
    let movers: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    if movers.iter().any(|m| {
        m.file_name().is_some_and(|name| dest.join(name).exists())
    }) {
        return written;
    }
    for m in &movers {
        let Some(name) = m.file_name() else { continue };
        if std::fs::rename(m, dest.join(name)).is_err() {
            return written;
        }
    }
    let _ = std::fs::remove_dir(&t_dir);
    written
        .into_iter()
        .map(|e| e.strip_prefix(&t_dir).map(|rel| dest.join(rel)).unwrap_or(e))
        .collect()
}

// ---------------------------------------------------------------- 权限还原

/// 条目名归一(边信道 map 键):`\`→`/`、去 `./` 前缀与尾部 `/`。
pub fn norm_entry_name(name: &str) -> String {
    let s = name.replace('\\', "/");
    s.trim_start_matches("./").trim_end_matches('/').to_string()
}

/// 落盘后还原归档内权限位(仅 unix;失败静默忽略,如只读 FS)。
/// mode==0(工具退化数据)跳过,避免把文件 chmod 成不可访问。
pub fn set_mode(p: &Path, mode: Option<u32>) {
    #[cfg(unix)]
    if let Some(m) = mode.filter(|m| *m & 0o7777 != 0) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(m & 0o7777));
    }
    #[cfg(not(unix))]
    let _ = (p, mode);
}

/// --overwrite 再解时,上次还原的只读 mode(0444 等)会让 File::create 报
/// EACCES;写前补回 owner 写权限。
pub fn ensure_writable(p: &Path) {
    #[cfg(unix)]
    if let Ok(md) = std::fs::metadata(p) {
        use std::os::unix::fs::PermissionsExt;
        let m = md.permissions().mode();
        if m & 0o200 == 0 {
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(m | 0o200));
        }
    }
    #[cfg(not(unix))]
    let _ = p;
}

/// unarc-rs 0.8.1 不暴露 unix mode:对可直读 header 的格式建 name→mode
/// 边信道(zip 中央目录 external_attr;tar 家族流式扫 512B 头部)。
pub fn mode_map(path: &Path, det: &crate::detect::Detected) -> HashMap<String, u32> {
    use unarc_rs::unified::ArchiveFormat as F;
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return HashMap::new(),
    };
    let br = std::io::BufReader::new(f);
    match det.kind {
        crate::detect::Known::Unarc(F::Zip | F::Jar) => zip_modes(path).unwrap_or_default(),
        crate::detect::Known::Unarc(F::Tar) => tar_mode_scan(br),
        crate::detect::Known::Unarc(F::Tgz) => tar_mode_scan(flate2::read::MultiGzDecoder::new(br)),
        crate::detect::Known::Unarc(F::Tbz) => tar_mode_scan(bzip2::read::BzDecoder::new(br)),
        crate::detect::Known::Unarc(F::Tzst) => match zstd::stream::read::Decoder::new(br) {
            Ok(d) => tar_mode_scan(d),
            Err(_) => HashMap::new(),
        },
        crate::detect::Known::Unarc(F::Txz) => {
            // 借用 unarc 的 XZ 解码(无 Read 包装):整读后扫(tar 尺寸受上限约束)
            match unarc_rs::xz::XzArchive::new(br).and_then(|mut a| a.read_with_limit(Some(1 << 31))) {
                Ok(v) => tar_mode_scan(std::io::Cursor::new(v)),
                Err(_) => HashMap::new(),
            }
        }
        _ => HashMap::new(),
    }
}

fn rfind_sig(buf: &[u8], sig: u32) -> Option<usize> {
    let s = sig.to_le_bytes();
    (0..=buf.len().checked_sub(4)?).rev().find(|&i| buf[i..i + 4] == s)
}

fn zip_modes(path: &Path) -> std::io::Result<HashMap<String, u32>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    if len < 22 {
        return Ok(HashMap::new());
    }
    // EOCD(PK\x05\x06)位于尾部 22..64KiB+22 区间内
    let tail_len = std::cmp::min(len, 22 + 65535);
    let mut tail = vec![0u8; tail_len as usize];
    f.seek(SeekFrom::Start(len - tail_len))?;
    f.read_exact(&mut tail)?;
    let eocd = rfind_sig(&tail, 0x0605_4b50).ok_or_else(|| std::io::Error::other("no EOCD"))?;
    let e = &tail[eocd..eocd + 22];
    let mut entries = u16::from_le_bytes(e[10..12].try_into().unwrap()) as u64;
    let mut cd_size = u32::from_le_bytes(e[12..16].try_into().unwrap()) as u64;
    let mut cd_off = u32::from_le_bytes(e[16..20].try_into().unwrap()) as u64;
    // zip64:哨兵值 → 经 EOCD64 locator(PK\x06\x07,EOCD 前 20B)跳读
    if (cd_off == 0xFFFF_FFFF || cd_size == 0xFFFF_FFFF || entries == 0xFFFF)
        && eocd >= 20
        && rfind_sig(&tail[eocd - 20..eocd], 0x0706_4b50).is_some()
    {
        let loc = &tail[eocd - 20 + 8..eocd - 20 + 16];
        let z64_off = u64::from_le_bytes(loc.try_into().unwrap());
        let mut z = [0u8; 56];
        f.seek(SeekFrom::Start(z64_off))?;
        f.read_exact(&mut z)?;
        if &z[0..4] == b"PK\x06\x06" {
            entries = u64::from_le_bytes(z[32..40].try_into().unwrap());
            cd_size = u64::from_le_bytes(z[40..48].try_into().unwrap());
            cd_off = u64::from_le_bytes(z[48..56].try_into().unwrap());
        }
    }
    // 中央目录逐条:46B 定长 + name/extra/comment;made-by 高字节 3=unix 时
    // mode = external_attr 高 16 位
    let mut cd = vec![0u8; cd_size.min(64 << 20) as usize];
    f.seek(SeekFrom::Start(cd_off))?;
    f.read_exact(&mut cd)?;
    let mut map = HashMap::new();
    let (mut i, mut n) = (0usize, 0u64);
    while i + 46 <= cd.len() && n < entries {
        if &cd[i..i + 4] != b"PK\x01\x02" {
            break;
        }
        // made_by u16 LE:+4=zip 规范版本低字节,+5=生成系统高字节(3=unix)
        let made_by_unix = cd[i + 5] == 3;
        let name_len = u16::from_le_bytes(cd[i + 28..i + 30].try_into().unwrap()) as usize;
        let extra_len = u16::from_le_bytes(cd[i + 30..i + 32].try_into().unwrap()) as usize;
        let cmt_len = u16::from_le_bytes(cd[i + 32..i + 34].try_into().unwrap()) as usize;
        if made_by_unix {
            let attr = u32::from_le_bytes(cd[i + 38..i + 42].try_into().unwrap());
            let mode = attr >> 16 & 0o7777;
            // mode=0(工具未填 unix attr 的退化情形)不还原:会让文件不可访问
            let name = String::from_utf8_lossy(&cd[i + 46..i + 46 + name_len]).into_owned();
            let key = norm_entry_name(&name);
            if mode != 0 && !key.is_empty() {
                map.insert(key, mode);
            }
        }
        i += 46 + name_len + extra_len + cmt_len;
        n += 1;
    }
    Ok(map)
}

/// 流式扫 tar 头(只过 header,不读数据块内容由 tar crate skip 处理)。
fn tar_mode_scan<R: std::io::Read>(r: R) -> HashMap<String, u32> {
    let mut ar = tar::Archive::new(r);
    let mut map = HashMap::new();
    if let Ok(entries) = ar.entries() {
        for e in entries.flatten() {
            let mode = e.header().mode().ok().map(|m| m & 0o7777);
            let name = e.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            let key = norm_entry_name(&name);
            if let (Some(m), false) = (mode.filter(|m| *m != 0), key.is_empty()) {
                map.insert(key, m);
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_entry_name_strips_prefix_slash_and_dots() {
        assert_eq!(norm_entry_name("./a/b.txt"), "a/b.txt");
        assert_eq!(norm_entry_name("dir/"), "dir");
        assert_eq!(norm_entry_name("win\\path.x"), "win/path.x");
        assert_eq!(norm_entry_name("./"), "");
    }

    #[test]
    fn zip_modes_reads_unix_external_attrs() {
        // 用 zip crate 造:unix 755 文件 + unix 目录 + dos-made 文件(应缺席)。
        // 注意:zip crate 的 made-by 字节默认跟随当前系统,必须显式 system(),
        // 否则 Windows 上 unix_permissions 不会写入 unix external attr。
        let path = std::env::temp_dir().join(format!("anyarc_zipmode_{}.zip", std::process::id()));
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let unix_opts: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default()
            .system(zip::System::Unix)
            .unix_permissions(0o755);
        zw.start_file("bin/tool", unix_opts).unwrap();
        std::io::Write::write_all(&mut zw, b"#!/bin/sh\n").unwrap();
        let dir_opts: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default()
            .system(zip::System::Unix)
            .unix_permissions(0o755);
        zw.add_directory("scripts/", dir_opts).unwrap();
        let dos_opts: zip::write::FileOptions<'_, '_, ()> =
            zip::write::FileOptions::default().system(zip::System::Dos);
        zw.start_file("dos.txt", dos_opts).unwrap();
        std::io::Write::write_all(&mut zw, b"x").unwrap();
        zw.finish().unwrap();
        let m = zip_modes(&path).unwrap();
        assert_eq!(m.get("bin/tool"), Some(&0o755));
        assert_eq!(m.get("scripts"), Some(&0o755)); // 目录尾斜杠已归一
        // dos-made 条目无 unix attr:不参与还原(真 unix 档退化 attr=0 同理,mode!=0 过滤)
        assert_eq!(m.get("dos.txt"), None);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn tar_modes_reads_header_mode() {
        let path = std::env::temp_dir().join(format!("anyarc_tarmode_{}.tar", std::process::id()));
        {
            let mut tb = tar::Builder::new(std::fs::File::create(&path).unwrap());
            let mut h = tar::Header::new_gnu();
            h.set_size(4);
            h.set_mode(0o700);
            h.set_cksum();
            tb.append_data(&mut h, "./exec.sh", &b"cmd\n"[..]).unwrap();
            tb.finish().unwrap();
        }
        let m = tar_mode_scan(std::fs::File::open(&path).unwrap());
        assert_eq!(m.get("exec.sh"), Some(&0o700)); // "./" 前缀已归一
        std::fs::remove_file(&path).unwrap();
    }
}
