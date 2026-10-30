#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
decrypt_zip.py — 通用加密 zip 解压工具

解决的核心问题：
  WinRAR/WinZip 制作的 AES 加密 zip（压缩方式 99），macOS 自带解压工具、
  unzip、lsar 均不支持，本脚本通过 pyzipper 解决。

用法示例：
  # 解压到默认目录（<zip同目录>/<zip文件名>_extracted）
  python3 decrypt_zip.py 归档.zip -p 密码

  # 指定输出目录
  python3 decrypt_zip.py 归档.zip -p 密码 -o /path/to/out

  # 不带 -p，会以隐藏输入方式询问密码（推荐，避免密码进 shell 历史）
  python3 decrypt_zip.py 归档.zip

  # 只列出内容、验证密码，不解压
  python3 decrypt_zip.py 归档.zip -p 密码 -l

  # 文件名乱码时（Windows 中文系统打包的 zip），用 GBK 重新解码文件名
  python3 decrypt_zip.py 归档.zip -p 密码 -e gbk

依赖：
  仅 AES 加密的压缩包需要 pyzipper（pip3 install pyzipper），
  普通加密（ZipCrypto）/无加密压缩包仅用标准库即可。
"""

import argparse
import getpass
import os
import sys
import time
import zipfile


# ---------- 工具函数 ----------

def detect_aes(path):
    """检查压缩包是否包含 AES 加密条目（压缩方式 99）。"""
    with zipfile.ZipFile(path) as z:
        return any(i.compress_type == 99 for i in z.infolist())


def open_archive(path, password, need_aes):
    """打开压缩包并设置密码，返回句柄和实际使用的密码字节。"""
    pwd = password.encode("utf-8") if password is not None else None
    if not need_aes:
        zf = zipfile.ZipFile(path)
        if pwd:
            zf.setpassword(pwd)
        return zf

    try:
        import pyzipper
    except ImportError:
        sys.exit(
            "[错误] 该压缩包使用 AES 加密，需要安装 pyzipper：\n"
            "       pip3 install pyzipper"
        )
    zf = pyzipper.AESZipFile(path)
    zf.setpassword(pwd)
    return zf


def fix_name(info, encoding):
    """
    修正文件名编码。
    - zip 头未标 UTF-8 时，标准库会用 cp437 解码，中文即成乱码；
      此时用指定编码（如 gbk）重新解码。
    - 若已是 UTF-8（flag 0x800）则原样返回。
    """
    if encoding and not (info.flag_bits & 0x800):
        try:
            return info.filename.encode("cp437").decode(encoding)
        except (UnicodeEncodeError, UnicodeDecodeError):
            pass
    return info.filename


def safe_target(outdir, name):
    """防止 zip slip（路径穿越攻击）：过滤危险路径片段。"""
    parts = [p for p in name.replace("\\", "/").split("/") if p not in ("", ".", "..")]
    if not parts:
        raise ValueError(f"非法条目名: {name!r}")
    return os.path.join(outdir, *parts)


# ---------- 主流程 ----------

def cmd_list(zf, encoding):
    total = sum(i.file_size for i in zf.infolist())
    print(f"共 {len(zf.infolist())} 个条目，解压后约 {total / 1024 / 1024 / 1024:.2f} GB")
    print(f"{'大小':>12}  名称")
    for i in zf.infolist():
        size = f"{i.file_size:,}"
        print(f"{size:>12}  {fix_name(i, encoding)}")


def cmd_extract(zf, outdir, encoding):
    infos = zf.infolist()
    total_bytes = sum(i.file_size for i in infos)
    done_bytes, ok, fail = 0, 0, 0
    failures = []
    start = time.time()

    os.makedirs(outdir, exist_ok=True)

    for idx, info in enumerate(infos, 1):
        name = fix_name(info, encoding)
        try:
            target = safe_target(outdir, name)
            if info.is_dir():
                os.makedirs(target, exist_ok=True)
            else:
                os.makedirs(os.path.dirname(target), exist_ok=True)
                with zf.open(info) as src, open(target, "wb") as dst:
                    while True:
                        chunk = src.read(4 * 1024 * 1024)
                        if not chunk:
                            break
                        dst.write(chunk)
                        done_bytes += len(chunk)
            ok += 1
        except Exception as e:
            fail += 1
            failures.append((name, str(e)))
            print(f"[{idx}/{len(infos)}] 失败: {name} -> {e}", flush=True)

        if idx % 20 == 0 or idx == len(infos):
            pct = done_bytes * 100 // max(total_bytes, 1)
            print(f"[{idx}/{len(infos)}] {pct}% 用时 {time.time() - start:.0f}s", flush=True)

    print(f"=== 完成: 成功 {ok}，失败 {fail}，总用时 {time.time() - start:.0f} 秒 ===")
    print(f"输出目录: {outdir}")
    if failures:
        print("失败列表:")
        for n, e in failures:
            print(f"  - {n} | {e}")
    return 0 if fail == 0 else 1


def main():
    ap = argparse.ArgumentParser(
        description="解压加密 zip（支持 WinRAR/WinZip 的 AES 加密）",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__.split("用法示例：")[-1].split("依赖：")[0],
    )
    ap.add_argument("archive", help="zip 文件路径")
    ap.add_argument("-p", "--password", help="密码（不填则隐藏输入询问）")
    ap.add_argument("-o", "--outdir", help="输出目录（默认: <zip所在目录>/<zip名>_extracted）")
    ap.add_argument("-e", "--encoding", help="文件名乱码时指定原编码，如 gbk")
    ap.add_argument("-l", "--list", action="store_true", help="只列出内容并验证密码，不解压")
    args = ap.parse_args()

    path = os.path.abspath(os.path.expanduser(args.archive))
    if not os.path.isfile(path):
        sys.exit(f"[错误] 文件不存在: {path}")

    try:
        need_aes = detect_aes(path)
    except zipfile.BadZipFile as e:
        sys.exit(
            f"[错误] 无法解析 zip 结构: {e}\n"
            "提示: 若文件被拼接过前置数据（如伪装成 MP4），部分工具可能失败，\n"
            "     本脚本从文件末尾定位目录结构，通常仍可处理。"
        )

    if need_aes:
        print("[*] 检测到 AES 加密条目（压缩方式 99），使用 pyzipper 解密")
    password = args.password or getpass.getpass("请输入压缩包密码: ")

    zf = open_archive(path, password, need_aes)

    # 先验证密码：读取最小的条目做一次真实解密
    try:
        infos = sorted(zf.infolist(), key=lambda i: i.file_size)
        probe = infos[0]
        if not probe.is_dir():
            head = zf.read(probe.filename)[:16]
            print(f"[*] 密码验证通过（示例文件头: {head.hex()}）")
    except RuntimeError as e:
        msg = str(e).lower()
        if "password" in msg or "decrypt" in msg or "bad" in msg:
            sys.exit("[错误] 密码错误或压缩包损坏")
        raise
    except Exception as e:
        sys.exit(f"[错误] 解密失败: {e}")

    if args.list:
        cmd_list(zf, args.encoding)
        return

    outdir = args.outdir or os.path.join(
        os.path.dirname(path), os.path.splitext(os.path.basename(path))[0] + "_extracted"
    )
    sys.exit(cmd_extract(zf, outdir, args.encoding))


if __name__ == "__main__":
    main()
