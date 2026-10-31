#!/usr/bin/env bash
# 用系统参照工具为每个 fixture 生成 ground-truth manifest:
# 每行 "<sha256>\t<相对路径>";单文件流格式用解压后文件名。
set -u
DATA=/home/zym/code/patool/tests/data
OUT=/home/zym/code/anyarc/poc/ref
PASS=thereisnotry
rm -rf "$OUT"; mkdir -p "$OUT"

mk_manifest() { # $1=fixture名 $2=目录 $3=可选"strip"(仅 unar 需要去掉顶层同名目录)
  local name="$1" dir="$2" m="$OUT/$1.manifest"
  ( cd "$dir" && find . -type f -printf '%P\n' | sort | while read -r f; do
      printf '%s\t%s\n' "$(sha256sum "$f" | cut -d' ' -f1)" "$f"
    done ) > "$m"
  if [ "${3:-}" = strip ]; then
  python3 - "$m" "$name" <<'EOF'
import sys
m, name = sys.argv[1], sys.argv[2]
lines = open(m).read().splitlines()
stem = name.rsplit('.', 1)[0]
if lines and all('/' in l.split('\t')[1] and l.split('\t')[1].split('/')[0] == stem for l in lines):
    open(m, 'w').write('\n'.join(l.split('\t')[0] + '\t' + l.split('\t')[1][len(stem)+1:] for l in lines) + '\n')
EOF
  fi
  if [ ! -s "$m" ]; then echo "REF-NO $name (空结果)"; rm -rf "$dir" "$m"; else echo "REF-OK $name $(wc -l < "$m") entries"; fi
}

ref_single() { # $1=fixture名 $2=解压命令
  local f="$DATA/$1" dir="$OUT/$1" stem="${1%.*}"
  mkdir -p "$dir"
  if $2 "$f" > "$dir/$stem"; then mk_manifest "$1" "$dir"; else echo "REF-NO $1"; rm -rf "$dir"; fi
}

ref7z() { # $1=fixture名 [$2=密码]
  local base="$1" dir="$OUT/$1" f="$DATA/$1" pass7z=""
  case "$base" in p.*) pass7z="-p$PASS";; "p "*) pass7z="-p$PASS";; esac
  mkdir -p "$dir"
  if 7z x -y -bso0 -bse0 $pass7z -o"$dir" "$f" >/dev/null 2>&1; then mk_manifest "$base" "$dir"; else echo "REF-NO $base"; rm -rf "$dir"; fi
}

refunar() { # $1=fixture名
  local base="$1" dir="$OUT/$1" f="$DATA/$1" pass_unar=()
  case "$base" in p.*) pass_unar=(-p "$PASS");; "p "*) pass_unar=(-p "$PASS");; esac
  mkdir -p "$dir"
  if unar -q -D -nr -o "$dir" "${pass_unar[@]}" "$f" >/dev/null 2>&1; then mk_manifest "$base" "$dir"; else echo "REF-NO $base"; rm -rf "$dir"; fi
}

for f in "$DATA"/*; do
  base=$(basename "$f"); ext="${base##*.}"
  case "$base" in *.foo|t.txt|t2.txt|t.flac.wav) continue;; esac
  [ -d "$f" ] && continue
  # 复合扩展名优先按 base 匹配
  case "$base" in
    *.tar.gz|*.tar.bz2|*.tar.xz|*.tar.zst|*.tar.Z|*.tar.lz|*.tar.lzma|*.tar.bz3)
      dir="$OUT/$base"; mkdir -p "$dir"
      if tar -xf "$f" -C "$dir" 2>/dev/null; then mk_manifest "$base" "$dir"; else echo "REF-NO $base"; rm -rf "$dir"; fi
      continue;;
  esac
  case "$ext" in
    zip|cbz|jar|epub|apk)
      dir="$OUT/$base"; mkdir -p "$dir"
      if unzip -qq -P "$PASS" -d "$dir" "$f" 2>/dev/null || unzip -qq -d "$dir" "$f" 2>/dev/null; then mk_manifest "$base" "$dir"; else echo "REF-NO $base"; rm -rf "$dir"; fi;;
    tar|tgz|taz|tbz2|cbt|cpio) mkdir -p "$OUT/$base"; if tar -xf "$f" -C "$OUT/$base" 2>/dev/null; then mk_manifest "$base" "$OUT/$base"; elif 7z x -y -bso0 -bse0 -o"$OUT/$base" "$f" >/dev/null 2>&1; then mk_manifest "$base" "$OUT/$base"; else echo "REF-NO $base"; rm -rf "$OUT/$base"; fi;;
    gz) ref_single "$base" "gzip -dc";;
    bz2) ref_single "$base" "bzip2 -dc";;
    xz) ref_single "$base" "xz -dc";;
    zst) ref_single "$base" "zstd -dcq";;
    lzma) ref_single "$base" "xz --format=lzma -dc";;
    lz) ref7z "$base";; # 7z 26 支持 lzip
    Z) ref_single "$base" "gzip -dc";;
    7z|001|cb7) ref7z "$base";;
    rar|cbr|ace|cba|alz) refunar "$base";;
    cab) dir="$OUT/$base"; mkdir -p "$dir"; if cabextract -q -d "$dir" "$f" >/dev/null 2>&1; then mk_manifest "$base" "$dir"; else echo "REF-NO $base"; rm -rf "$dir"; fi;;
    deb|rpm|a|iso|udf|chm|wim|vhd) ref7z "$base";;
    arc|arj|zoo|lha|lzh)
      dir="$OUT/$base"; mkdir -p "$dir"
      if 7z x -y -bso0 -bse0 -o"$dir" "$f" >/dev/null 2>&1; then mk_manifest "$base" "$dir"
      elif unar -q -D -nr -o "$dir" "$f" >/dev/null 2>&1; then mk_manifest "$base" "$dir"
      else echo "REF-NO $base"; rm -rf "$dir"; fi;;
    *) echo "REF-SKIP $base";;
  esac
done
done
