#!/usr/bin/env bash
# CLI 端到端冒烟:对全部 fixture 跑 list(必要时 extract),
# 判定支持/预期拒绝/BAD。拒绝 = stderr 含 不支持|无法识别|Backlog|需要密码。
set -u
BIN=${BIN:-/home/zym/code/anyarc/target/release/anyarc}
DATA=${ANYARC_FIXTURES:-/home/zym/code/patool/tests/data}
unset ANYARC_PASSWORD  # 默认无密码;t.arc 等无密 archive 带 env 密码会被 unarc 误解密
ok=0; refused=0; bad=0; badlist=""
for f in "$DATA"/*; do
  [ -f "$f" ] || continue
  case "$(basename "$f")" in t.txt|t2.txt) ;; esac
  # p.* 加密 fixture 带密码跑
  case "$(basename "$f")" in p.*) export ANYARC_PASSWORD=thereisnotry;; *) unset ANYARC_PASSWORD;; esac
  err=$("$BIN" list -l "$f" 2>&1 >/dev/null); rc=$?
  if [ $rc -eq 0 ]; then
    # 可 list 的再试 extract 到临时目录(密码/损坏场景 extract 才暴露)
    t=$(mktemp -d /tmp/opencode/e2e_run_XXXX)
    err2=$( { "$BIN" extract "$f" -o "$t"; } 2>&1 >/dev/null ); rc2=$?
    if [ $rc2 -eq 0 ]; then
      ok=$((ok+1))
    elif [[ "$err2" =~ 不支持|无法识别|Backlog|需要密码|密码 ]]; then
      refused=$((refused+1))
    else
      bad=$((bad+1)); badlist="$badlist $(basename "$f")"
    fi
    rm -rf "$t"
  elif [[ "$err" =~ 不支持|无法识别|Backlog|需要密码|密码 ]]; then
    refused=$((refused+1))
  else
    bad=$((bad+1)); badlist="$badlist $(basename "$f")"
  fi
done
total=$((ok+refused+bad))
echo "E2E total=$total OK=$ok REFUSED=$refused BAD=$bad"
[ -n "$badlist" ] && echo "BAD:$badlist"
exit 0
