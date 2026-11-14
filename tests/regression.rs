//! 需求验收回归套件(CLI 二进制级)。
//!
//! 约定:任何修改必须 `cargo test --release`(单元 + 本套件)全绿才允许 push
//! (CI 与 pre-push hook 双重门禁)。
//!
//! ┌─ 需求 ────────────────────┬─ 用例 ───────────────────────────────┐
//! │ 流式解压 + pv 进度        │ r1_gz_stream_roundtrip               │
//! │ 递归解压(-r 显式开启)   │ r2_default_single_level / r3         │
//! │ 多参数卷集去重            │ r4_duplicate_args                    │
//! │ --smart 同名目录合并      │ r5_smart_collapse_dup_dir            │
//! │ --smart 单条目同名提升    │ r6_smart_lift_single_samename        │
//! │ 权限还原(tar)             │ r7_tar_mode_restore                  │
//! │ 权限还原(zip 边信道)      │ r8_zip_mode_restore                  │
//! │ 权限还原(tar.bz2/zst)     │ r9_compound_tar_modes                │
//! │ 权限还原(rar5)            │ r10_rar_mode_restore                 │
//! │ --overwrite 0444 回环     │ r11_readonly_overwrite_roundtrip     │
//! │ macOS 垃圾剔除            │ r12_macos_junk_rules(非 mac 基线)   │
//! │ 分卷 zip(.zNN+末卷)     │ r13_spanned_zip_extract              │
//! │ RAR 分卷智能串联          │ 单元:volume_chain / part_number      │
//! │ zip-slip 防护             │ r14_zip_slip_rejected                │
//! │ 自引用条目防护            │ r15_self_reference_skipped           │
//! │ 坏档不 panic + 不留半成品 │ r16_corrupt_inputs_no_panic          │
//! │ 限额防线(entry/total)     │ r17_size_caps                        │
//! └───────────────────────────┴──────────────────────────────────────┘

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

const ANYARC: &str = env!("CARGO_BIN_EXE_anyarc");

/// 每用例独立临时目录(测试并行安全)
fn sandbox(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("anyarc_it_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn anyarc(args: &[&str], cwd: Option<&Path>) -> Out {
    let mut cmd = Command::new(ANYARC);
    cmd.args(args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    cmd.env_remove("ANYARC_PASSWORD");
    let o = cmd.output().expect("run anyarc");
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).to_string(),
        stderr: String::from_utf8_lossy(&o.stderr).to_string(),
    }
}

// ---------------------------------------------------------------- fixtures

fn gz_bytes(payload: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(payload).unwrap();
    enc.finish().unwrap()
}

/// tar 条目 (name, data, mode);top_dir 可选顶层目录;返回 **裸 tar** 字节
fn tar_bytes(entries: &[(&str, &[u8], u32)], top_dir: Option<&str>) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut tb = tar::Builder::new(&mut buf);
        for (name, data, mode) in entries {
            let full = match top_dir {
                Some(t) => format!("{t}/{name}"),
                None => name.to_string(),
            };
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(*mode);
            h.set_cksum();
            tb.append_data(&mut h, &full, *data).unwrap();
        }
        tb.finish().unwrap();
    }
    buf
}

/// zip 条目 (name, data, unix_mode?);add_dirs 目录项
fn zip_bytes(entries: &[(&str, &[u8], Option<u32>)], add_dirs: &[&str]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        for d in add_dirs {
            let opts: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
            zw.add_directory(*d, opts).unwrap();
        }
        for (name, data, mode) in entries {
            let mut opts: zip::write::FileOptions<'_, '_, ()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            if let Some(m) = mode {
                opts = opts.unix_permissions(*m);
            }
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap();
    }
    buf
}

fn mode_of(p: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        0
    }
}

// ------------------------------------------------------------ 流式/递归/smart

#[test]
fn r1_gz_stream_roundtrip() {
    let s = sandbox("r1");
    let payload: Vec<u8> = (0..400_000u32).map(|i| (i * 31 % 251) as u8).collect();
    fs::write(s.join("data.gz"), gz_bytes(&payload)).unwrap();
    let out = anyarc(&["extract", "data.gz", "-o", "o"], Some(&s));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(fs::read(s.join("o/data")).unwrap(), payload);
    // 非 TTY:stdout 必须干净(进度/摘要只走 TTY stderr)
    assert!(out.stdout.is_empty(), "stdout polluted: {:?}", out.stdout);
}

/// 手工构造含非法路径条目(`../x`)的裸 tar(tar crate 拒写此类档)
fn raw_tar_escape(name: &str, data: &[u8]) -> Vec<u8> {
    let mut blk = [0u8; 512];
    let n = name.as_bytes();
    blk[..n.len().min(100)].copy_from_slice(&n[..n.len().min(100)]);
    blk[100..107].copy_from_slice(b"0000644");
    blk[108] = 0;
    blk[116..123].copy_from_slice(b"0000000");
    blk[123] = 0;
    blk[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
    blk[135] = 0;
    blk[136..147].copy_from_slice(b"00000000000");
    blk[147] = 0;
    blk[148..156].copy_from_slice(b"        "); // 校验和占位=8 空格
    blk[156] = b'0';
    blk[257..262].copy_from_slice(b"ustar");
    blk[263..265].copy_from_slice(b"00");
    let sum: u32 = blk.iter().map(|&b| b as u32).sum();
    blk[148..154].copy_from_slice(format!("{sum:06o}").as_bytes());
    blk[154] = 0;
    let mut out = blk.to_vec();
    out.extend_from_slice(data);
    out.resize(512 * data.len().div_ceil(512).max(1) + 512, 0);
    out.extend_from_slice(&[0u8; 512]);
    out
}

#[test]
fn r2_default_single_level() {
    let s = sandbox("r2");
    let inner = gz_bytes(&tar_bytes(&[("leaf.txt", b"deep", 0o644)], None));
    fs::write(s.join("a.zip"), zip_bytes(&[("inner.tar.gz", &inner, None)], &[])).unwrap();
    let out = anyarc(&["extract", "a.zip", "-o", "o"], Some(&s));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(s.join("o/inner.tar.gz").is_file(), "默认应只解一层");
    assert!(!s.join("o/leaf.txt").exists());
}

#[test]
fn r3_recursive_reaches_leaf() {
    let s = sandbox("r3");
    let inner = gz_bytes(&tar_bytes(&[("leaf.txt", b"deep", 0o644)], None));
    fs::write(s.join("a.zip"), zip_bytes(&[("inner.tar.gz", &inner, None)], &[])).unwrap();
    let out = anyarc(&["extract", "--recursive", "a.zip", "-o", "o"], Some(&s));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(fs::read(s.join("o/leaf.txt")).unwrap(), b"deep");
    assert!(!s.join("o/inner.tar.gz").exists(), "中间压缩包应删除");
    assert!(out.stderr.contains("继续解压"), "应有递归提示:{}", out.stderr);
}

#[test]
fn r4_duplicate_args() {
    let s = sandbox("r4");
    fs::write(s.join("a.zip"), zip_bytes(&[("f.txt", b"x", None)], &[])).unwrap();
    let out = anyarc(&["extract", "a.zip", "a.zip", "-o", "o"], Some(&s));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stderr.contains("跳过"), "重复参数应提示跳过:{}", out.stderr);
}

#[test]
fn r5_smart_collapse_dup_dir() {
    let s = sandbox("r5");
    // a.zip: x/inner.tar.gz;tar 顶层同名 x/ → smart 合并成 x/leaf.txt
    let inner = gz_bytes(&tar_bytes(&[("leaf.txt", b"L", 0o644)], Some("x")));
    fs::write(s.join("a.zip"), zip_bytes(&[("x/inner.tar.gz", &inner, None)], &["x/"])).unwrap();
    let out = anyarc(&["extract", "--recursive", "--smart", "a.zip", "-o", "o"], Some(&s));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(fs::read(s.join("o/x/leaf.txt")).unwrap(), b"L");
    assert!(!s.join("o/x/x").exists(), "重复包裹目录应合并");
}

#[test]
fn r6_smart_lift_single_samename() {
    let s = sandbox("r6");
    // novel.zip: novel/novel.tar.gz;tar 根为单同名文件 novel → 提升同级、删空壳
    let inner = gz_bytes(&tar_bytes(&[("novel", b"PAY", 0o644)], None));
    fs::write(s.join("novel.zip"), zip_bytes(&[("novel/novel.tar.gz", &inner, None)], &["novel/"])).unwrap();
    // 对照:无 smart → 保留 o1/novel/novel
    let out1 = anyarc(&["extract", "-r", "novel.zip", "-o", "o1"], Some(&s));
    assert_eq!(out1.code, 0, "{}", out1.stderr);
    assert_eq!(fs::read(s.join("o1/novel/novel")).unwrap(), b"PAY");
    // 有 smart → o2/novel 直接是文件
    let out2 = anyarc(&["extract", "-r", "--smart", "novel.zip", "-o", "o2"], Some(&s));
    assert_eq!(out2.code, 0, "{}", out2.stderr);
    assert!(s.join("o2/novel").is_file(), "同名单条目应提升");
    assert_eq!(fs::read(s.join("o2/novel")).unwrap(), b"PAY");
    assert!(!s.join("o2/novel/novel").exists());
}

// ---------------------------------------------------------------- 权限还原

#[test]
fn r7_tar_mode_restore() {
    let s = sandbox("r7");
    fs::write(s.join("plain.tar"), tar_bytes(&[("run.sh", b"#!/bin/sh\n", 0o755), ("cfg", b"c", 0o600)], None)).unwrap();
    let out = anyarc(&["extract", "plain.tar", "-o", "o"], Some(&s));
    assert_eq!(out.code, 0, "{}", out.stderr);
    #[cfg(unix)]
    {
        assert_eq!(mode_of(&s.join("o/run.sh")), 0o755);
        assert_eq!(mode_of(&s.join("o/cfg")), 0o600);
    }
}

#[test]
fn r8_zip_mode_restore() {
    let s = sandbox("r8");
    fs::write(s.join("z.zip"), zip_bytes(&[("tool", b"#!/bin/sh\n", Some(0o755)), ("txt", b"t", None)], &[])).unwrap();
    let out = anyarc(&["extract", "z.zip", "-o", "o"], Some(&s));
    assert_eq!(out.code, 0, "{}", out.stderr);
    #[cfg(unix)]
    {
        assert_eq!(mode_of(&s.join("o/tool")), 0o755, "zip unix attr 应还原");
        let m = mode_of(&s.join("o/txt"));
        assert!(m & 0o700 != 0, "无 mode 条目保持可用默认: {m:o}");
    }
}

#[test]
fn r9_compound_tar_modes() {
    // tar.bz2 / tar.zst:unarc Tbz/Tzst 后端不带 mode → 边信道必须补全
    let s = sandbox("r9");
    let t = tar_bytes(&[("x.sh", b"x", 0o750)], None);
    {
        let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        enc.write_all(&t).unwrap();
        fs::write(s.join("a.tar.bz2"), enc.finish().unwrap()).unwrap();
    }
    {
        let mut enc = zstd::Encoder::new(Vec::new(), 3).unwrap();
        enc.write_all(&t).unwrap();
        fs::write(s.join("b.tar.zst"), enc.finish().unwrap()).unwrap();
    }
    for (f, od) in [("a.tar.bz2", "o1"), ("b.tar.zst", "o2")] {
        let out = anyarc(&["extract", f, "-o", od], Some(&s));
        assert_eq!(out.code, 0, "{f}: {}", out.stderr);
        #[cfg(unix)]
        assert_eq!(mode_of(&s.join(od).join("x.sh")), 0o750, "{f} 边信道权限");
    }
}

#[test]
fn r10_rar_mode_restore() {
    let s = sandbox("r10");
    {
        use rars::{builder::Builder, ArchiveVersion};
        let mut b = Builder::new(ArchiveVersion::Rar50);
        b.add_bytes(b"bin.sh".to_vec(), b"#!/bin/sh\n".to_vec(), None, Some(0o755)).unwrap();
        b.write_to_path(&s.join("r.rar"), None).unwrap();
    }
    let out = anyarc(&["extract", "r.rar", "-o", "o"], Some(&s));
    assert_eq!(out.code, 0, "{}", out.stderr);
    #[cfg(unix)]
    assert_eq!(mode_of(&s.join("o/bin.sh")), 0o755, "RAR5 unix mode 应还原");
    fs::remove_dir_all(&s).ok();
}

#[test]
fn r11_readonly_overwrite_roundtrip() {
    let s = sandbox("r11");
    fs::write(s.join("ro.tar"), tar_bytes(&[("ro.txt", b"1", 0o444)], None)).unwrap();
    let o1 = anyarc(&["extract", "ro.tar", "-o", "o"], Some(&s));
    assert_eq!(o1.code, 0, "{}", o1.stderr);
    #[cfg(unix)]
    assert_eq!(mode_of(&s.join("o/ro.txt")), 0o444);
    // --overwrite 再解:上次 0444 不应挡住重写
    let o2 = anyarc(&["extract", "--overwrite", "ro.tar", "-o", "o"], Some(&s));
    assert_eq!(o2.code, 0, "0444 回环: {}", o2.stderr);
    #[cfg(unix)]
    assert_eq!(mode_of(&s.join("o/ro.txt")), 0o444);
}

// ------------------------------------------------------- 打包/分卷/安全/健壮

#[test]
fn r12_macos_junk_rules() {
    let s = sandbox("r12");
    fs::create_dir_all(s.join("pkg")).unwrap();
    fs::write(s.join("pkg/keep.txt"), b"k").unwrap();
    let out = anyarc(&["create", "o.zip", "pkg"], Some(&s));
    assert_eq!(out.code, 0, "{}", out.stderr);
    let listing = anyarc(&["list", "o.zip"], Some(&s));
    assert!(listing.stdout.contains("keep.txt"));
    // 剔除规则单测在 create::tests::junk_skipping_only_on_macos(cfg 门控两侧断言)
}

#[test]
fn r13_spanned_zip_extract() {
    // Info-ZIP 语义:CD/local 虚拟绝对偏移 + 末卷 EOCD this-disk≠0,手工切两卷
    let s = sandbox("r13");
    let orig = zip_bytes(&[("d/run.sh", b"#!/bin/sh\n", Some(0o755)), ("d/big.bin", &vec![7u8; 3000], None)], &[]);
    let eocd = (0..orig.len().saturating_sub(21)).rev().find(|i| &orig[*i..*i + 4] == b"PK\x05\x06").unwrap();
    let cd_size = u32::from_le_bytes(orig[eocd + 12..eocd + 16].try_into().unwrap()) as usize;
    let cd_off = u32::from_le_bytes(orig[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    let cut = cd_off + cd_size / 2;
    let mut last = orig[cut..].to_vec();
    // EOCD this-disk 字段置 1(模拟分卷元数据)
    let eo_in_last = last.len() - 22;
    last[eo_in_last + 4..eo_in_last + 6].copy_from_slice(&1u16.to_le_bytes());
    fs::write(s.join("sp.z01"), &orig[..cut]).unwrap();
    fs::write(s.join("sp.zip"), &last).unwrap();
    for entry in ["sp.zip", "sp.z01"] {
        let od = if entry == "sp.zip" { "oa" } else { "ob" };
        let out = anyarc(&["extract", entry, "-o", od], Some(&s));
        assert_eq!(out.code, 0, "{entry}: {}", out.stderr);
        assert_eq!(fs::read(s.join(od).join("d/run.sh")).unwrap(), b"#!/bin/sh\n", "{entry}");
        assert_eq!(fs::read(s.join(od).join("d/big.bin")).unwrap().len(), 3000, "{entry}");
        #[cfg(unix)]
        assert_eq!(mode_of(&s.join(od).join("d/run.sh")), 0o755, "{entry} 分卷权限");
    }
}

#[test]
fn r14_zip_slip_rejected() {
    let s = sandbox("r14");
    fs::write(s.join("evil.tar"), raw_tar_escape("../escaped.txt", b"X!")).unwrap();
    fs::create_dir_all(s.join("o")).unwrap();
    let out = anyarc(&["extract", "evil.tar", "-o", "o"], Some(&s));
    assert!(!s.join("escaped.txt").exists(), "路径逃逸必须拒绝");
    assert!(out.stderr.contains("安全保护") || out.stderr.contains("跳过"), "{}", out.stderr);
}

#[test]
fn r15_self_reference_skipped() {
    let s = sandbox("r15");
    // 归档内含与自身同名条目 + --overwrite 解到同目录(流式会先截断数据源)
    fs::write(s.join("self.zip"), zip_bytes(&[("self.zip", b"trap", None)], &[])).unwrap();
    let out = anyarc(&["extract", "--overwrite", "self.zip", "-o", "."], Some(&s));
    assert!(out.stderr.contains("自引用") || out.stderr.contains("跳过"), "应拒绝自引用:{}", out.stderr);
    // 原归档未被截断损坏:仍可 list
    let l = anyarc(&["list", "self.zip"], Some(&s));
    assert_eq!(l.code, 0, "自引用防护后归档应完好: {}", l.stderr);
}

#[test]
fn r16_corrupt_inputs_no_panic() {
    let s = sandbox("r16");
    let good = gz_bytes(&[0x90u8; 200_000]);
    fs::write(s.join("trunc.gz"), &good[..good.len() / 2]).unwrap();
    fs::write(s.join("empty.cpio"), b"").unwrap();
    fs::write(s.join("junk.zip"), b"PK\x03\x04not-a-zip-at-all").unwrap();
    fs::write(s.join("garbage"), b"\x00\x01\x02random junk").unwrap();
    for f in ["trunc.gz", "empty.cpio", "junk.zip", "garbage"] {
        let out = anyarc(&["extract", f, "-o", "o"], Some(&s));
        assert_ne!(out.code, 0, "{f} 坏档应非零退出");
        assert!(!out.stderr.contains("panicked"), "{f} panic 了:{}", out.stderr);
        assert!(!out.stdout.contains("panicked"));
    }
    assert!(!s.join("o/trunc").exists(), "解压失败不留半成品");
}

#[test]
fn r17_size_caps() {
    let s = sandbox("r17");
    fs::write(
        s.join("c.zip"),
        zip_bytes(&[("a.bin", &vec![3u8; 5000], None), ("b.bin", &vec![4u8; 5000], None)], &[]),
    )
    .unwrap();
    // entry 上限:整读路径应明确拒绝(自有中文防线或 zip crate 内部限额均可)
    let o1 = anyarc(&["extract", "--max-entry-size", "10", "c.zip", "-o", "o1"], Some(&s));
    assert_ne!(o1.code, 0);
    assert!(
        o1.stderr.contains("单个文件太大") || o1.stderr.contains("超过上限") || o1.stderr.contains("size limit"),
        "{}",
        o1.stderr
    );
    // total 预算:跨条目累计超限中止
    let o2 = anyarc(&["extract", "--max-total-size", "6000", "c.zip", "-o", "o2"], Some(&s));
    assert_ne!(o2.code, 0);
    assert!(
        o2.stderr.contains("超过上限") || o2.stderr.contains("总大小") || o2.stderr.contains("size limit"),
        "{}",
        o2.stderr
    );
}
