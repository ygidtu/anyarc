use std::fs::File;
use std::io::BufReader;
use unarc_rs::unified::{ArchiveFormat, UnifiedArchive};

fn probe(p: &str) {
    println!("== {p}");
    // 1) unarc-rs 内容探测(带/不带路径提示)
    if let Ok(mut r) = File::open(p) {
        let d1 = ArchiveFormat::detect(&mut r, Some(std::path::Path::new(p))).ok().flatten();
        let d2 = ArchiveFormat::detect(&mut r, None).ok().flatten();
        println!("  detect(path hint): {:?}", d1.map(|f| f.name()));
        println!("  detect(pure)   : {:?}", d2.map(|f| f.name()));
    }
    // 2) unarc-rs 强制按 Zip 打开 + 全读
    match File::open(p) {
        Ok(f) => match UnifiedArchive::open_with_format_and_options(BufReader::new(f), ArchiveFormat::Zip, unarc_rs::unified::ArchiveOptions::new().with_password("secret")) {
            Err(e) => println!("  unarc force-Zip: FAIL {e}"),
            Ok(mut a) => {
                a.set_password("secret");
                let mut n = 0;
                let mut err = String::new();
                while let Some(en) = a.next_entry().unwrap_or(None) {
                    match a.read(&en) {
                        Ok(_) => n += 1,
                        Err(e) => { err = format!("{e}"); break; }
                    }
                }
                if err.is_empty() { println!("  unarc force-Zip: OK {n} 条目读全") }
                else { println!("  unarc force-Zip: 读到第{n}条失败: {err}") }
            }
        },
        Err(e) => println!("  open: {e}"),
    }
    // 3) zip-rs 直接打开
    match File::open(p) {
        Ok(f) => match zip::ZipArchive::new(f) {
            Err(e) => println!("  zip-rs: FAIL {e}"),
            Ok(z) => println!("  zip-rs: OK {} 条目", z.len()),
        },
        Err(e) => println!("  zip-rs open: {e}"),
    }
}

fn main() {
    for p in std::env::args().skip(1) { probe(&p); }
}
