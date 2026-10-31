//! Shorten v2/v3 解码器(自研)。
//!
//! 依据公开格式事实(TR.156 与社区规范文档)实现;字段语义经 GNU shorten
//! 产物(t.shn → t.wav)字节级验证,未复制任何第三方代码。
//!
//! 流布局:"ajkg" + 版本字节(@4);v2 起头部为 6 个 `ulong`(两段 uvar)
//! 字段:filetype、channels、blocksize、maxlpcorder、meanblocks、skipbytes;
//! skipbytes 之后跟 skipbytes 个 `uvar(7)` 字节(宿主头前置数据)。
//! 命令流:每通道一个 `uvar(2)` 命令码,通道轮转:
//!   0..3 DIFF0-3 / 7 QLPC / 8 ZERO → [uvar(3) resn] + blocksize 个样本
//!   4 QUIT(结束)/ 5 BLOCKSIZE → ulong / 6 BITSHIFT → uvar(2)
//!   9 VERBATIM → uvar(5) 长度 + 逐字节 uvar(8)
//! `var(n)` = `uvar(n+1)` 无符号值 zig-zag 展开为带符号。

/// 命令码
const FN_DIFF0: u32 = 0;
const FN_QUIT: u32 = 4;
const FN_BLOCKSIZE: u32 = 5;
const FN_BITSHIFT: u32 = 6;
const FN_QLPC: u32 = 7;
const FN_ZERO: u32 = 8;
const FN_VERBATIM: u32 = 9;

const NWRAP: usize = 3;
const LPCQUANT: u32 = 5;
const MAX_CHANNELS: u32 = 8;
const MAX_BLOCKSIZE: usize = 1 << 22;
const MAX_ENVELOPE: usize = 16 << 20;

/// 解码结果:PCM(每通道一条带符号样本序列)+ verbatim 宿主信封字节。
pub struct ShnStream {
    pub filetype: u32,
    pub channels: u32,
    pub verbatim: Vec<u8>,
    /// 每通道样本(已应用 bitshift 放大),各通道等长。
    pub channel_samples: Vec<Vec<i32>>,
}

/// MSB-first 位读取器(uvar 语义:连续 k 个 0,一个终止 1,再读 n 位值)。
struct BitReader<'a> {
    d: &'a [u8],
    p: usize, // 位偏移
}

impl BitReader<'_> {
    fn bit(&mut self) -> anyhow::Result<bool> {
        let i = self.p / 8;
        if i >= self.d.len() {
            anyhow::bail!("Shorten 数据截断");
        }
        let b = (self.d[i] >> (7 - (self.p % 8))) & 1 != 0;
        self.p += 1;
        Ok(b)
    }

    fn uvar(&mut self, n: u32) -> anyhow::Result<u32> {
        let mut k: u64 = 0;
        while !self.bit()? {
            k += 1;
            if k > 40 {
                anyhow::bail!("Shorten 数据损坏(uvar 前缀过长)");
            }
        }
        let mut m: u64 = 0;
        for _ in 0..n {
            m = (m << 1) | u64::from(self.bit()?);
        }
        let v = (k << n) | m;
        if v > u32::MAX as u64 {
            anyhow::bail!("Shorten 数据损坏(uvar 溢出)");
        }
        Ok(v as u32)
    }

    /// 两段式 ulong:宽度 w=uvar(2),值=uvar(w)。
    fn ulong(&mut self) -> anyhow::Result<u32> {
        let w = self.uvar(2)?;
        if w > 32 {
            anyhow::bail!("Shorten 数据损坏(ulong 宽度 {w})");
        }
        self.uvar(w)
    }

    /// zig-zag 带符号 var。
    fn var(&mut self, n: u32) -> anyhow::Result<i32> {
        let u = self.uvar(n + 1)?;
        Ok(if u & 1 == 1 { !(u >> 1) as i32 } else { (u >> 1) as i32 })
    }
}

fn rounded_shift_down(x: i32, n: u32) -> i32 {
    if n == 0 {
        x
    } else {
        (x >> (n - 1).min(31)) >> 1
    }
}

/// 解码 Shorten v2/v3 流。max_total_samples 限制输出样本总数(防御)。
pub fn decode(data: &[u8], max_total_samples: u64) -> anyhow::Result<ShnStream> {
    if data.len() < 5 || &data[..4] != b"ajkg" {
        anyhow::bail!("Shorten 魔数不符");
    }
    let version = data[4];
    if !matches!(version, 2 | 3) {
        anyhow::bail!("Shorten v{version} 不支持(仅 v2/v3;v0/v1 请用其他工具)");
    }
    let mut br = BitReader { d: data, p: 40 };

    let filetype = br.ulong()?;
    let channels = br.ulong()?;
    if channels == 0 || channels > MAX_CHANNELS {
        anyhow::bail!("Shorten 声道数非法({channels})");
    }
    let init_blocksize = br.ulong()? as usize;
    let maxnlpc = br.ulong()? as usize;
    let nmean = br.ulong()? as usize;
    let nskip = br.ulong()? as usize;
    if init_blocksize == 0 || init_blocksize > MAX_BLOCKSIZE {
        anyhow::bail!("Shorten 块大小非法({init_blocksize})");
    }
    if maxnlpc > 1024 || nmean > 1024 {
        anyhow::bail!("Shorten 头参数过大(maxlpc={maxnlpc}, meanblocks={nmean})");
    }

    match filetype {
        0 | 7 | 8 | 9 | 10 => {
            anyhow::bail!("Shorten filetype={filetype}(u-law/alaw 有损类型)不支持");
        }
        _ => {}
    }

    let nwrap = NWRAP.max(maxnlpc);
    // 每通道样本环:前 nwrap 个为历史区,payload 从 [nwrap] 起
    let mut bufs: Vec<Vec<i32>> =
        vec![vec![0i32; nwrap + init_blocksize]; channels as usize];
    let init_mean: i32 = match filetype {
        2 => 0x80,        // U8
        4 | 6 => 0x8000,  // U16
        _ => 0,
    };
    let mean_n = nmean.max(1);
    let mut offs: Vec<Vec<i32>> = vec![vec![init_mean; mean_n]; channels as usize];
    let mut qlpc = vec![0i32; maxnlpc];
    let mut bitshift: u32 = 0;

    let mut verbatim: Vec<u8> = Vec::new();
    for _ in 0..nskip {
        verbatim.push(br.uvar(7)? as u8); // XBYTESIZE=7
        if verbatim.len() > MAX_ENVELOPE {
            anyhow::bail!("Shorten 信封过大");
        }
    }

    let mut out: Vec<Vec<i32>> = vec![Vec::new(); channels as usize];
    let mut total_samples: u64 = 0;
    let mut blocksize = init_blocksize;
    let mut chan = 0usize;

    loop {
        let cmd = br.uvar(2)?;
        match cmd {
            FN_DIFF0..=3 | FN_QLPC | FN_ZERO => {
                let resn = if cmd == FN_ZERO {
                    0
                } else {
                    let r = br.uvar(3)?; // ENERGYSIZE
                    if r > 30 {
                        anyhow::bail!("Shorten resn 过大({r})");
                    }
                    r
                };
                // 均值补偿偏移(当前通道)
                let coffset: i32 = if nmean == 0 {
                    offs[chan][0]
                } else {
                    let mut sum: i32 = (nmean / 2) as i32;
                    for o in &offs[chan] {
                        sum = sum.wrapping_add(*o);
                    }
                    rounded_shift_down(sum / nmean as i32, bitshift)
                };
                let b = &mut bufs[chan];
                match cmd {
                    0 => {
                        for i in 0..blocksize {
                            b[nwrap + i] = br.var(resn)?.wrapping_add(coffset);
                        }
                    }
                    1 => {
                        for i in 0..blocksize {
                            b[nwrap + i] =
                                br.var(resn)?.wrapping_add(b[nwrap + i - 1]);
                        }
                    }
                    2 => {
                        for i in 0..blocksize {
                            b[nwrap + i] = br.var(resn)?.wrapping_add(
                                2i32.wrapping_mul(b[nwrap + i - 1])
                                    - b[nwrap + i - 2],
                            );
                        }
                    }
                    3 => {
                        for i in 0..blocksize {
                            b[nwrap + i] = br.var(resn)?.wrapping_add(
                                3 * (b[nwrap + i - 1] - b[nwrap + i - 2])
                                    + b[nwrap + i - 3],
                            );
                        }
                    }
                    FN_ZERO => {
                        b[nwrap..nwrap + blocksize].fill(0);
                    }
                    _ => {
                        // FN_QLPC
                        let nlpc = br.uvar(2)? as usize;
                        if nlpc > maxnlpc {
                            anyhow::bail!("Shorten nlpc({nlpc}) 超出头声明({maxnlpc})");
                        }
                        for q in qlpc[..nlpc].iter_mut() {
                            *q = br.var(LPCQUANT)?;
                        }
                        for i in 0..nlpc {
                            b[nwrap + i - nlpc] =
                                b[nwrap + i - nlpc].wrapping_sub(coffset);
                        }
                        for i in 0..blocksize {
                            let mut s: i64 = 1 << LPCQUANT; // V2LPCQOFFSET
                            for j in 0..nlpc {
                                s += qlpc[j] as i64 * b[nwrap + i - j - 1] as i64;
                            }
                            b[nwrap + i] =
                                br.var(resn)?.wrapping_add((s >> LPCQUANT) as i32);
                        }
                        if coffset != 0 {
                            for v in b[nwrap..nwrap + blocksize].iter_mut() {
                                *v = v.wrapping_add(coffset);
                            }
                        }
                    }
                }
                // 均值环更新(用未放大样本;与 GNU 语义一致)
                if nmean > 0 {
                    let mut sum: i32 = (blocksize / 2) as i32;
                    for v in &b[nwrap..nwrap + blocksize] {
                        sum = sum.wrapping_add(*v);
                    }
                    for j in 1..nmean {
                        offs[chan][j - 1] = offs[chan][j];
                    }
                    offs[chan][nmean - 1] =
                        (sum / blocksize as i32).wrapping_shl(bitshift);
                }
                // 历史区滚动(未放大),随后本块整体左移 bitshift
                for j in 0..nwrap {
                    b[j] = b[j + blocksize];
                }
                if bitshift != 0 {
                    for v in b[nwrap..nwrap + blocksize].iter_mut() {
                        *v = v.wrapping_shl(bitshift);
                    }
                }
                if chan == channels as usize - 1 {
                    total_samples += blocksize as u64 * channels as u64;
                    if total_samples > max_total_samples {
                        anyhow::bail!("Shorten 解码样本数超限({max_total_samples})");
                    }
                    for (ch, buf) in bufs.iter().enumerate() {
                        out[ch].extend_from_slice(&buf[nwrap..nwrap + blocksize]);
                    }
                }
                chan = (chan + 1) % channels as usize;
            }
            FN_QUIT => break,
            FN_BLOCKSIZE => {
                let nb = br.ulong()? as usize;
                if nb == 0 || nb > init_blocksize {
                    anyhow::bail!("Shorten blocksize 非法({nb},上限 {init_blocksize})");
                }
                blocksize = nb;
            }
            FN_BITSHIFT => {
                bitshift = br.uvar(2)?.min(31);
            }
            FN_VERBATIM => {
                let cklen = br.uvar(5)? as usize; // VERBATIM_CKSIZE_SIZE=5
                if verbatim.len() + cklen > MAX_ENVELOPE {
                    anyhow::bail!("Shorten 信封过大");
                }
                for _ in 0..cklen {
                    verbatim.push(br.uvar(8)? as u8); // VERBATIM_BYTE_SIZE=8
                }
            }
            _ => anyhow::bail!("Shorten 数据损坏:未知命令 {cmd}"),
        }
    }

    Ok(ShnStream { filetype, channels, verbatim, channel_samples: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 GNU shorten 产物对照:t.shn 解码必须字节级复现 t.wav。
    /// (fixture 缺失时跳过;矩阵/ e2e 另有全局把关)
    #[test]
    fn shn_decodes_like_gnu_shorten_oracle() {
        let dir = std::path::Path::new("/home/zym/code/patool/tests/data");
        if !dir.join("t.shn").is_file() || !dir.join("t.wav").is_file() {
            return;
        }
        let shn = std::fs::read(dir.join("t.shn")).unwrap();
        let wav = std::fs::read(dir.join("t.wav")).unwrap();
        let ds = decode(&shn, u64::MAX).unwrap();
        assert_eq!(ds.filetype, 5); // TYPE_S16LH
        assert_eq!(ds.channels, 1);
        assert_eq!(ds.channel_samples[0].len(), 2012);
        // 信封 + PCM 整体与 t.wav 一致(信封修正后)
        let mut out = ds.verbatim.clone();
        if out.len() == 44 {
            out[4..8].copy_from_slice(&((36 + 2012 * 2) as u32).to_le_bytes());
            out[40..44].copy_from_slice(&(2012u32 * 2).to_le_bytes());
        }
        for s in &ds.channel_samples[0] {
            out.extend_from_slice(&(*s as u16).to_le_bytes());
        }
        assert_eq!(out, wav);
    }

    /// 畸形/截断输入必须报错而非 panic。
    #[test]
    fn shn_malformed_inputs_error_not_panic() {
        let full = b"ajkg\x02";
        for n in 0..full.len() {
            assert!(decode(&full[..n], u64::MAX).is_err());
        }
        // 合法魔数 + 全 0 位流(uvar 前缀无限延伸 → 必须被上限截停)
        let mut d = full.to_vec();
        d.extend_from_slice(&[0u8; 200]);
        assert!(decode(&d, u64::MAX).is_err());
        // 0xFF 填充(width 字段全 1)也不得 panic
        let mut d2 = full.to_vec();
        d2.extend_from_slice(&[0xFFu8; 200]);
        let _ = decode(&d2, u64::MAX);
    }
}
