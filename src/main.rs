//! PoC 兼容性验证 harness:
//! 对 patool 全部 fixture 依次跑 unarc-rs 与 sevenz-rust2,
//! 结果与系统参照工具生成的 sha256 manifest 逐条比对。
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use unarc_rs::unified::{ArchiveFormat, ArchiveOptions, UnifiedArchive};

const DATA: &str = "/home/zym/code/patool/tests/data";
const REF: &str = "/home/zym/code/anyarc/ref";
const PASS: &str = "thereisnotry";

type Entries = BTreeMap<String, String>; // name -> sha256

fn hexsha(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn is_pw(key: &str) -> bool {
    key.starts_with("p.") || key.starts_with("p ")
}

/// 单文件流格式解压后的目标名(去掉最后一个扩展名)
fn single_name(key: &str) -> String {
    match key.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => key.to_string(),
    }
}

fn load_ref(key: &str) -> Option<Entries> {
    let text = fs::read_to_string(format!("{REF}/{key}.manifest")).ok()?;
    let mut m = Entries::new();
    for line in text.lines() {
        let (sha, name) = line.split_once('\t')?;
        m.insert(name.to_string(), sha.to_string());
    }
    Some(m)
}

/// 遍历并读取一个已打开的 archive,返回 (条目哈希表, 目录数, 文本嗅探是否全为"42")
fn collect<T: std::io::Read + std::io::Seek>(
    mut archive: UnifiedArchive<T>,
    key: &str,
) -> Result<(String, Entries, usize, bool), String> {
    let fmt = archive.format().name().to_string();
    match archive.format() {
        ArchiveFormat::Gz | ArchiveFormat::Bz2 | ArchiveFormat::Xz | ArchiveFormat::Zst
        | ArchiveFormat::Z => archive.set_single_file_name(single_name(key)),
        _ => {}
    }
    let mut entries = Entries::new();
    let mut dirs = 0;
    let mut sniff_ok = true;
    while let Some(en) = archive.next_entry().map_err(|e| format!("{e}"))? {
        if en.is_directory() {
            dirs += 1;
            archive.skip(&en).map_err(|e| format!("skip: {e}"))?;
            continue;
        }
        let data = archive
            .read(&en)
            .map_err(|err| format!("read {}: {err}", en.name()))?;
        if en.name().ends_with(".txt") && !data.starts_with(b"42") {
            sniff_ok = false;
        }
        // 路径分隔符归一化(ACE/CAB 内部为反斜杠)
        entries.insert(en.name().replace('\\', "/"), hexsha(&data));
    }
    Ok((fmt, entries, dirs, sniff_ok))
}

/// 通过 unarc-rs 统一 API 读取一个 fixture(含分卷 7z)
fn run_unarc(path: &Path, key: &str) -> Result<(String, Entries, usize, bool), String> {
    let mut opts = ArchiveOptions::new().with_verify_crc(true);
    if is_pw(key) {
        opts = opts.with_password(PASS);
    }
    if let Some(stem) = key.strip_suffix(".001") {
        let mut vols: Vec<PathBuf> = fs::read_dir(DATA)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| {
                        (n.starts_with(stem) && n.len() == stem.len() + 3
                            && n[stem.len() + 1..].chars().all(|c| c.is_ascii_digit()))
                            || n == path.file_name().unwrap().to_str().unwrap()
                    })
                    .unwrap_or(false)
            })
            .collect();
        vols.sort();
        let a = ArchiveFormat::open_multi_volume_7z(&vols, opts).map_err(|e| format!("{e}"))?;
        collect(a, key)
    } else {
        // 用内容(magic bytes)探测格式,再显式打开 —— 兼容 .foo 改名 fixture
        let mut probe = std::io::BufReader::new(fs::File::open(path).map_err(|e| format!("{e}"))?);
        let fmt = ArchiveFormat::detect(&mut probe, Some(path))
            .map_err(|e| format!("detect: {e}"))?
            .ok_or_else(|| "detect: 未知格式".to_string())?;
        let file = std::io::BufReader::new(fs::File::open(path).map_err(|e| format!("{e}"))?);
        let a = UnifiedArchive::open_with_format_and_options(file, fmt, opts).map_err(|e| format!("{e}"))?;
        collect(a, key)
    }
}

/// sevenz-rust2 直接解压到临时目录,再哈希
fn run_sevenz(path: &Path, key: &str) -> Result<Entries, String> {
    let tmp = PathBuf::from(format!("/tmp/opencode/poc-sevenz/{}", key.replace('/', "_")));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    let r = if is_pw(key) {
        sevenz_rust2::decompress_file_with_password(path, &tmp, PASS.into())
    } else {
        sevenz_rust2::decompress_file(path, &tmp)
    };
    r.map_err(|e| format!("{e}"))?;
    let mut entries = Entries::new();
    let mut stack = vec![tmp.clone()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(&tmp).unwrap().to_string_lossy().replace('\\', "/");
                entries.insert(rel, hexsha(&fs::read(&p).unwrap()));
            }
        }
    }
    Ok(entries)
}

fn cmp_entries(got: &Entries, want: &Entries) -> Vec<String> {
    let mut diffs = vec![];
    for (n, h) in want {
        match got.get(n) {
            None => diffs.push(format!("缺:{n}")),
            Some(g) if g != h => diffs.push(format!("哈希不符:{n}")),
            _ => {}
        }
    }
    for n in got.keys() {
        if !want.contains_key(n) {
            diffs.push(format!("多:{n}"));
        }
    }
    diffs
}

struct Row {
    fixture: String,
    fmt: String,
    unarc: String,
    sevenz: String,
    detail: String,
}

fn main() {
    fs::create_dir_all("/tmp/opencode/poc-sevenz").unwrap();
    let mut files: Vec<PathBuf> = fs::read_dir(DATA)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .filter(|p| {
            let n = p.file_name().unwrap().to_str().unwrap();
            !["t.txt", "t2.txt", "t.flac.wav"].contains(&n)
        })
        .collect();
    files.sort();
    let filter: Option<String> = std::env::args().nth(1);
    if let Some(f) = &filter {
        files.retain(|p| p.file_name().unwrap().to_str().unwrap().contains(f));
    }

    let mut rows: Vec<Row> = vec![];
    for path in &files {
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        let key = name.strip_suffix(".foo").unwrap_or(&name).to_string();
        eprintln!("TRY {name}");

        // --- unarc-rs ---
        let (fmt, unarc_status, detail) = match run_unarc(path, &key) {
            Ok((f, e, d, sniff)) => {
                let refm = load_ref(&key);
                let st = match &refm {
                    Some(w) => {
                        let diffs = cmp_entries(&e, w);
                        if diffs.is_empty() {
                            "MATCH".to_string()
                        } else {
                            format!("MISMATCH({})", diffs.len())
                        }
                    }
                    None => {
                        if sniff {
                            "NOREF-OK".to_string()
                        } else {
                            "NOREF-SNIFF-FAIL".to_string()
                        }
                    }
                };
                let mut det = format!("{}项/{}目录", e.len(), d);
                if !sniff {
                    det += " ⚠文本非42";
                }
                if let Some(w) = &refm {
                    let diffs = cmp_entries(&e, w);
                    if !diffs.is_empty() {
                        det += &format!(" [{}]", diffs.join(";"));
                    }
                }
                (f, st, det)
            }
            Err(e) => ("-".to_string(), "FAIL".to_string(), e),
        };

        // --- sevenz-rust2(仅 7z 家族 fixture)---
        let mut sevenz_status = "-".to_string();
        if key.ends_with(".7z") || key.ends_with(".7z.001") {
            match run_sevenz(path, &key) {
                Ok(e) => {
                    if let Some(w) = load_ref(&key) {
                        let diffs = cmp_entries(&e, &w);
                        sevenz_status = if diffs.is_empty() {
                            "MATCH".to_string()
                        } else {
                            format!("MISMATCH({}) {}", diffs.len(), diffs.join(";"))
                        };
                    } else {
                        sevenz_status = format!("EXTRACTED {}项", e.len());
                    }
                }
                Err(e) => sevenz_status = format!("FAIL {e}"),
            }
        }

        rows.push(Row { fixture: name, fmt, unarc: unarc_status, sevenz: sevenz_status, detail });
    }

    println!("| fixture | 检测格式 | unarc-rs | sevenz-rust2 | 备注 |");
    println!("|---|---|---|---|---|");
    for r in &rows {
        println!("| {} | {} | {} | {} | {} |", r.fixture, r.fmt, r.unarc, r.sevenz, r.detail);
    }
    let ok = rows
        .iter()
        .filter(|r| r.unarc.starts_with("MATCH") || r.unarc == "NOREF-OK")
        .count();
    let mismatch = rows.iter().filter(|r| r.unarc.starts_with("MISMATCH")).count();
    let fail = rows.iter().filter(|r| r.unarc == "FAIL").count();
    let sz_match = rows.iter().filter(|r| r.sevenz == "MATCH").count();
    let sz_fail = rows.iter().filter(|r| r.sevenz.starts_with("FAIL")).count();
    println!(
        "\n总计 {} 个 fixture: unarc-rs MATCH/NOREF-OK={ok}, MISMATCH={mismatch}, FAIL={fail}; sevenz-rust2: MATCH={sz_match}, FAIL={sz_fail}",
        rows.len()
    );
}
