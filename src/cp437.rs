//! IBM Code Page 437 映射(字节 <-> Unicode)。
//! 表数据改编自 zip-rs v9.1.3 `cp437.rs`(MIT 许可)的 to_char 表。

/// cp437 高位区(0x80..=0xFF)-> Unicode 码点表。
const CP437_HIGH: [u16; 128] = [
        0x00c7,
        0x00fc,
        0x00e9,
        0x00e2,
        0x00e4,
        0x00e0,
        0x00e5,
        0x00e7,
        0x00ea,
        0x00eb,
        0x00e8,
        0x00ef,
        0x00ee,
        0x00ec,
        0x00c4,
        0x00c5,
        0x00c9,
        0x00e6,
        0x00c6,
        0x00f4,
        0x00f6,
        0x00f2,
        0x00fb,
        0x00f9,
        0x00ff,
        0x00d6,
        0x00dc,
        0x00a2,
        0x00a3,
        0x00a5,
        0x20a7,
        0x0192,
        0x00e1,
        0x00ed,
        0x00f3,
        0x00fa,
        0x00f1,
        0x00d1,
        0x00aa,
        0x00ba,
        0x00bf,
        0x2310,
        0x00ac,
        0x00bd,
        0x00bc,
        0x00a1,
        0x00ab,
        0x00bb,
        0x2591,
        0x2592,
        0x2593,
        0x2502,
        0x2524,
        0x2561,
        0x2562,
        0x2556,
        0x2555,
        0x2563,
        0x2551,
        0x2557,
        0x255d,
        0x255c,
        0x255b,
        0x2510,
        0x2514,
        0x2534,
        0x252c,
        0x251c,
        0x2500,
        0x253c,
        0x255e,
        0x255f,
        0x255a,
        0x2554,
        0x2569,
        0x2566,
        0x2560,
        0x2550,
        0x256c,
        0x2567,
        0x2568,
        0x2564,
        0x2565,
        0x2559,
        0x2558,
        0x2552,
        0x2553,
        0x256b,
        0x256a,
        0x2518,
        0x250c,
        0x2588,
        0x2584,
        0x258c,
        0x2590,
        0x2580,
        0x03b1,
        0x00df,
        0x0393,
        0x03c0,
        0x03a3,
        0x03c3,
        0x00b5,
        0x03c4,
        0x03a6,
        0x0398,
        0x03a9,
        0x03b4,
        0x221e,
        0x03c6,
        0x03b5,
        0x2229,
        0x2261,
        0x00b1,
        0x2265,
        0x2264,
        0x2320,
        0x2321,
        0x00f7,
        0x2248,
        0x00b0,
        0x2219,
        0x00b7,
        0x221a,
        0x207f,
        0x00b2,
        0x25a0,
        0x00a0,
];

/// 把按 cp437 解码的字符串编回单字节(反查表;无法映射的字符 -> `?`)。
pub fn encode_str(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        if (c as u32) < 0x80 {
            out.push(c as u8);
        } else {
            out.push(CP437_HIGH.iter().position(|&u| u == c as u16).map_or(b'?', |i| i as u8 + 0x80));
        }
    }
    out
}

/// 严格版:含不可映射字符返回 None。用于 `--encoding` 修复的前置判定 —
/// 真 cp437 乱码必然可逆;不可逆(如已是正确 UTF-8 名)则不应修复。
pub fn try_encode_str(s: &str) -> Option<Vec<u8>> {
    s.chars()
        .map(|c| {
            if (c as u32) < 0x80 {
                Some(c as u8)
            } else {
                CP437_HIGH.iter().position(|&u| u == c as u16).map(|i| i as u8 + 0x80)
            }
        })
        .collect()
}

/// cp437 字节 -> Unicode 字符串。
pub fn decode_bytes(b: &[u8]) -> String {
    b.iter()
        .map(|&x| {
            if x < 0x80 {
                char::from(x)
            } else {
                char::from_u32(CP437_HIGH[(x - 0x80) as usize] as u32).unwrap_or('\u{fffd}')
            }
        })
        .collect()
}
