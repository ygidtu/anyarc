#!/usr/bin/env python3
"""定向头字节 fuzz:对 M2 手写容器解析器的关键结构区域逐字节替换,
验证不 panic(101)/段错误(139)/挂死。用法: python3 poc/fuzz_headers.py"""
import os, random, shutil, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN_SRC = os.path.join(ROOT, "target/release/anyarc")
BIN = os.path.join("/tmp/opencode", "anyarc_fuzz_bin")
shutil.copyfile(BIN_SRC, BIN)
os.chmod(BIN, 0o755)
DATA = os.environ.get("ANYARC_FIXTURES", "/home/zym/code/patool/tests/data")

# (fixture, 需扫描的字节区间列表)
TARGETS = {
    "t.txt.a": [(0, 600)],                       # ar 头 + 成员头
    "t.deb":   [(0, 600)],                       # ar 头 + data.tar.gz payload
    "t.rpm":   [(0, 120), (1384, 1500), (8320, 8400)],  # lead / 主头 / payload magic
    "t.cpio":  [(0, 200)],                       # hpbin 头 + 名字
    "t.iso":   [(32768, 33024), (34816, 35072), (0xf000, 0xf044 + 72)],  # PVD/SVD/根目录记录
}
VALUES = [0x00, 0x01, 0x02, 0x34, 0x41, 0x7F, 0x80, 0xC7, 0xFF]
TIMEOUT = 10
random.seed(20261031)

def run(args):
    try:
        r = subprocess.run([BIN] + args, capture_output=True, timeout=TIMEOUT)
        return r.returncode
    except subprocess.TimeoutExpired:
        return -1

def main():
    crashes, timeouts, n = [], [], 0
    tmp = tempfile.mkdtemp(prefix="anyarc_hdr_")
    for name, ranges in TARGETS.items():
        src = os.path.join(DATA, name)
        base = open(src, "rb").read()
        for (lo, hi) in ranges:
            for off in range(lo, min(hi, len(base))):
                for v in VALUES:
                    if base[off] == v:
                        continue
                    m = bytearray(base)
                    m[off] = v
                    p = os.path.join(tmp, "m")
                    open(p, "wb").write(m)
                    for cmd in ("list", "test"):
                        n += 1
                        rc = run([cmd, p])
                        if rc == -1:
                            timeouts.append((name, hex(off), v, cmd))
                        elif rc not in (0, 1):
                            crashes.append((name, hex(off), v, cmd, rc))
        print(f"{name}: 完成,累计 {n} 次运行", flush=True)
    shutil.rmtree(tmp, ignore_errors=True)
    print(f"\n共 {n} 次运行: CRASH={len(crashes)} TIMEOUT={len(timeouts)}")
    for c in crashes[:20]:
        print("CRASH", c)
    for t in timeouts[:20]:
        print("TIMEOUT", t)
    return 1 if crashes or timeouts else 0

if __name__ == "__main__":
    sys.exit(main())
