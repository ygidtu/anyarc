//! 验收矩阵:对 patool 全部 fixture 用 anyarc 探测+解码管线跑一遍,
//! 与 poc/ref 生成的 sha256 manifest 比对(原 PoC harness 的 M1 版)。
use anyhow::Context;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyarc::backend::{self, EntryKind, OpenOptions};
use anyarc::detect::{self, Known};

fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("ANYARC_FIXTURES").unwrap_or_else(|_| "/home/zym/code/patool/tests/data".into()))
}
fn ref_dir() -> PathBuf {
    PathBuf::from(std::env::var("ANYARC_REF").unwrap_or_else(|_| {
        format!("{}/poc/ref", env!("CARGO_MANIFEST_DIR"))
    }))
}
const PASS: &str = "thereisnotry";

type Entries = BTreeMap<String, String>;

fn hexsha(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn is_pw(key: &str) -> bool {
    key.starts_with("p.") || key.starts_with("p ")
}

/// 单流压缩解压后的目标名(去掉最后一层扩展名)
fn single_name(key: &str) -> String {
    for suf in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tgz", ".tbz2", ".txz", ".tzst", ".taz"] {
        if let Some(stem) = key.strip_suffix(suf) {
            return stem.to_string();
        }
    }
    match key.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => key.to_string(),
    }
}

fn load_ref(key: &str) -> Option<Entries> {
    let text = fs::read_to_string(ref_dir().join(format!("{key}.manifest"))).ok()?;
    let mut m = Entries::new();
    for line in text.lines() {
        let (sha, name) = line.split_once('\t')?;
        m.insert(name.to_string(), sha.to_string());
    }
    Some(m)
}

/// 用 anyarc 管线完整读取一个 fixture
fn run(path: &Path, key: &str) -> anyhow::Result<(String, Entries, usize, bool)> {
    let det = detect::detect(path)?.context("detect: 未知格式")?;
    if matches!(det.kind, Known::Backlog(_)) {
        anyhow::bail!("Backlog 格式(按预期不支持)");
    }
    let mut opts = OpenOptions {
        password: if is_pw(key) { Some(PASS.to_string()) } else { None },
        single_file_name: None,
        ..Default::default()
    };
    // 单流格式:探测结果若为 Gz/Bz2/…(未触发二跳)则设目标名
    if let Known::Unarc(f) = det.kind {
        use unarc_rs::unified::ArchiveFormat as Af;
        if matches!(f, Af::Gz | Af::Bz2 | Af::Xz | Af::Zst | Af::Z) {
            opts.single_file_name = Some(single_name(key));
        }
    }
    let mut ar = backend::open(path, &det, &opts)?;
    let fmt = ar.format().name().to_string();
    let mut entries = Entries::new();
    let mut dirs = 0usize;
    let mut sniff_ok = true;
    while let Some(e) = ar.next_entry()? {
        if e.kind == EntryKind::Directory {
            dirs += 1;
            ar.skip_current()?;
            continue;
        }
        let data = ar.read_current().with_context(|| format!("read {}", e.name))?;
        if e.name.ends_with(".txt") && !data.starts_with(b"42") {
            sniff_ok = false;
        }
        entries.insert(e.name.clone(), hexsha(&data));
    }
    Ok((fmt, entries, dirs, sniff_ok))
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

fn main() {
    let mut files: Vec<PathBuf> = fs::read_dir(data_dir())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .filter(|p| {
            let n = p.file_name().unwrap().to_str().unwrap();
            !["t.txt", "t2.txt", "t.flac.wav"].contains(&n)
        })
        .collect();
    files.sort();
    if let Some(f) = std::env::args().nth(1) {
        files.retain(|p| p.file_name().unwrap().to_str().unwrap().contains(&f));
    }

    println!("| fixture | 检测格式 | 结果 | 备注 |");
    println!("|---|---|---|---|");
    let (mut n_match, mut n_noref, mut n_mm, mut n_fail) = (0, 0, 0, 0);
    for path in &files {
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        let key = name.strip_suffix(".foo").unwrap_or(&name).to_string();
        let (fmt, status, detail) = match run(path, &key) {
            Ok((f, e, d, sniff)) => {
                let refm = load_ref(&key);
                let st = match &refm {
                    Some(w) => {
                        let diffs = cmp_entries(&e, w);
                        if diffs.is_empty() {
                            n_match += 1;
                            "MATCH"
                        } else {
                            n_mm += 1;
                            "MISMATCH"
                        }
                    }
                    None => {
                        if sniff {
                            n_noref += 1;
                            "NOREF-OK"
                        } else {
                            n_mm += 1;
                            "NOREF-SNIFF-FAIL"
                        }
                    }
                };
                let mut det = format!("{}项/{}目录", e.len(), d);
                if let Some(w) = &refm {
                    let diffs = cmp_entries(&e, w);
                    if !diffs.is_empty() {
                        det += &format!(" [{}]", diffs.join(";"));
                    }
                }
                (f, st.to_string(), det)
            }
            Err(e) => {
                n_fail += 1;
                ("-".to_string(), "FAIL".to_string(), format!("{e}"))
            }
        };
        println!("| {name} | {fmt} | {status} | {detail} |");
    }
    println!("\n总计 {} 个 fixture: MATCH={n_match}, NOREF-OK={n_noref}, MISMATCH={n_mm}, FAIL={n_fail}", files.len());
}
