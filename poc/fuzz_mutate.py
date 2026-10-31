#!/usr/bin/env python3
"""字节翻转型 fuzz:对支持的 fixture 做随机突变,验证解码器
不 panic(退出码 101)、不段错误(139)、不挂死(timeout)。
用法: python3 poc/fuzz_mutate.py [每文件突变数,默认 200]"""
import os, random, subprocess, sys, tempfile, shutil

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN_SRC = os.path.join(ROOT, "target/release/anyarc")
BIN = os.path.join("/tmp/opencode", "anyarc_fuzz_bin")
shutil.copyfile(BIN_SRC, BIN)
os.chmod(BIN, 0o755)
DATA = os.environ.get("ANYARC_FIXTURES", "/home/zym/code/patool/tests/data")
# 只 fuzz 已知可读的格式(L1 支持集)
SUPPORTED = [
    "t.zip", "p.zip", "t.rar", "p.rar", "t.cbr", "t.7z.001", "p.7z",
    "t.tar", "t.tgz", "t.taz", "t.tbz2", "t.tar.gz", "t.tar.bz2", "t.tar.xz",
    "t.tar.Z", "t.tar.zst", "t.txt.gz", "t.txt.bz2", "t.txt.xz", "t.txt.zst",
    "t.txt.Z", "t.arc", "t.arj", "p.arj", "t.lha", "t.lzh", "t.zoo", "t.cab",
    "t.ace", "t.cba", "t.adf", "t.hdf", "t.dms", "t.jar", "t.epub", "t.apk",
    "t.cbz", "t.cb7", "t.cbt", "t.lynx", "t.sq", "t.uc2", "t.ice", "t.hyp",
    # M2 自写容器(手写解析器必须吃 fuzz)
    "t.deb", "t.rpm", "t.cpio", "t.iso", "t.txt.a",
]
N = int(sys.argv[1]) if len(sys.argv) > 1 else 200
random.seed(20261031)
tmp = tempfile.mkdtemp(prefix="anyarc_fuzz_")
crashes, timeouts = [], []
n_run = 0
try:
    for f in SUPPORTED:
        src = os.path.join(DATA, f)
        if not os.path.isfile(src):
            continue
        base = bytearray(open(src, "rb").read())
        for i in range(N):
            m = bytearray(base)
            for _ in range(random.randint(1, 8)):  # 多点翻转
                pos = random.randrange(len(m))
                m[pos] ^= random.randint(1, 255)
            path = os.path.join(tmp, f)
            open(path, "wb").write(m)
            pw = ["-p", "thereisnotry"] if f.startswith("p.") else []
            n_run += 1
            try:
                r = subprocess.run([BIN, "test", "-q"] + pw + [path],
                                   capture_output=True, timeout=30,
                                   env={**os.environ, "ANYARC_PASSWORD": ""})
                if r.returncode == 101 or b"panicked" in r.stderr:
                    crashes.append((f, i, "panic", r.stderr.decode(errors="replace")[:160]))
                    open(os.path.join(ROOT, f"poc/fuzz_crash_{f}_{i}.bin"), "wb").write(m)
                elif r.returncode in (134, 139):
                    crashes.append((f, i, f"signal rc={r.returncode}", ""))
                    open(os.path.join(ROOT, f"poc/fuzz_crash_{f}_{i}.bin"), "wb").write(m)
            except subprocess.TimeoutExpired:
                timeouts.append((f, i))
                open(os.path.join(ROOT, f"poc/fuzz_timeout_{f}_{i}.bin"), "wb").write(m)
finally:
    shutil.rmtree(tmp, ignore_errors=True)
print(f"fuzz 完成: {n_run} 次运行, CRASH={len(crashes)}, TIMEOUT={len(timeouts)}")
for c in crashes[:10]:
    print("CRASH", c)
for t in timeouts[:10]:
    print("TIMEOUT", t)
