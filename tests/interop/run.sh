#!/usr/bin/env bash
#
# Rust ↔ Go 互通矩阵（I1–I21）。矩阵定义见 docs/agent/testing.md §2。
#
# 这是本项目的**生命线测试**：协议字节必须与 Go 版 croc v11.5.4 逐字节兼容，
# 单测全绿也不代表能互通 —— 只有真跑一遍才知道。
#
# 环境变量：
#   CROC_RS_BIN                  Rust 版 croc-rs（默认 target/release/croc-rs[.exe]）
#   CROC_GO_BIN                  Go 版 croc（cargo xtask ci-interop 会先找 ../croc-playground/croc.exe，再退回用 ../croc 源码现编）
#   CROC_INTEROP_ONLY            只跑这些用例，逗号分隔，如 "I1,I2"；空 = 全跑
#   CROC_INTEROP_SKIP_NETWORK    非空 = 跳过需要外网的用例（I6）
#   CROC_INTEROP_TIMEOUT         单个用例里等发送方退出的超时秒数（默认 180）
#   CROC_INTEROP_KEEP            非空 = 保留临时目录（调试用）
#   CROC_INTEROP_FORCE_UNIMPLEMENTED
#                                非空 = 连"依赖尚未实现的能力"的用例也照跑（预期红）
#
# 退出码：0 = 全部通过（跳过的也算通过），1 = 有失败。

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT" || exit 1

# Windows 上二进制带 .exe（Git for Windows 的 uname 形如 MINGW64_NT-10.0）。
EXE=""
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) EXE=".exe" ;;
esac

CROC_RS_BIN="${CROC_RS_BIN:-$ROOT/target/release/croc-rs$EXE}"
CROC_GO_BIN="${CROC_GO_BIN:-}"
CROC_INTEROP_ONLY="${CROC_INTEROP_ONLY:-}"
CROC_INTEROP_SKIP_NETWORK="${CROC_INTEROP_SKIP_NETWORK:-}"
CROC_INTEROP_TIMEOUT="${CROC_INTEROP_TIMEOUT:-180}"
CROC_INTEROP_KEEP="${CROC_INTEROP_KEEP:-}"

LOG_DIR="$ROOT/target/interop-logs"
WORK=""
RELAY_PID=""
RELAY_PIDS=()
RELAY_BASE=""
RELAY_PASS="interop-relay-pass"
# 两端共用的固定 codephrase（三词格式）。固定下来是为了失败可复现，也便于日志脱敏。
CODE="acid-acorn-acre"
PASSED=0
FAILED=0
SKIPPED=0
FAILED_CASES=()
SKIP_REASONS=()

# ---------------------------------------------------------------- 输出

if [ -t 1 ]; then
  C_OK=$'\033[32m'; C_BAD=$'\033[31m'; C_SKIP=$'\033[33m'; C_DIM=$'\033[2m'; C_END=$'\033[0m'
else
  C_OK=""; C_BAD=""; C_SKIP=""; C_DIM=""; C_END=""
fi

log()  { printf '%s\n' "$*"; }
ok()   { printf '%s  PASS%s %s\n' "$C_OK" "$C_END" "$*"; }
bad()  { printf '%s  FAIL%s %s\n' "$C_BAD" "$C_END" "$*"; }
skip() { printf '%s  SKIP%s %s\n' "$C_SKIP" "$C_END" "$*"; }
dim()  { printf '%s%s%s\n' "$C_DIM" "$*" "$C_END"; }

# ---------------------------------------------------------------- 前置检查

require_bins() {
  local missing=0
  if [ ! -x "$CROC_RS_BIN" ]; then
    echo "interop: 找不到 Rust 版二进制 $CROC_RS_BIN" >&2
    echo "        先跑：cargo build --release -p croc-cli" >&2
    missing=1
  fi
  if [ -z "$CROC_GO_BIN" ]; then
    echo "interop: 未设置 CROC_GO_BIN" >&2
    missing=1
  elif [ ! -x "$CROC_GO_BIN" ]; then
    echo "interop: CROC_GO_BIN 不是可执行文件：$CROC_GO_BIN" >&2
    missing=1
  fi
  [ "$missing" -eq 0 ]
}

# ---------------------------------------------------------------- 工具

# 脱敏：日志里出现的房间号与口令一律替换掉（风险 R13）。
# BSD sed 与 GNU sed 的 -i 语法不同，所以不传 -i，改用临时文件。
redact() {
  local file="$1"
  [ -f "$file" ] || return 0
  sed -E \
    -e 's/\b[0-9a-f]{32,64}\b/[ROOM]/g' \
    -e "s/${RELAY_PASS}/[REDACTED]/g" \
    -e "s/${CODE}/[CODE]/g" \
    "$file" > "$file.redacted" 2>/dev/null && mv "$file.redacted" "$file"
  return 0
}

# 找一个连续 5 个端口都空闲的基址（relay 控制口 + 4 个数据口）。
free_port_base() {
  python3 - <<'PY'
import socket
for base in range(31000, 45000, 10):
    held, ok = [], True
    for port in range(base, base + 5):
        s = socket.socket()
        try:
            s.bind(("127.0.0.1", port))
        except OSError:
            ok = False
            s.close()
            break
        held.append(s)
    for s in held:
        s.close()
    if ok:
        print(base)
        raise SystemExit(0)
raise SystemExit("no free port range")
PY
}

wait_for_port() {  # <host> <port> <timeout_s>
  python3 - "$1" "$2" "$3" <<'PY'
import socket, sys, time
host, port, timeout = sys.argv[1], int(sys.argv[2]), float(sys.argv[3])
deadline = time.time() + timeout
while time.time() < deadline:
    s = socket.socket()
    s.settimeout(0.5)
    try:
        s.connect((host, port))
        s.close()
        raise SystemExit(0)
    except OSError:
        s.close()
        time.sleep(0.2)
raise SystemExit(1)
PY
}

# 生成 n 字节伪随机文件。用 python 而不是 /dev/urandom：Git for Windows 的 bash
# 对 /dev/urandom 的语义不保证。
make_file() {  # <path> <bytes>
  python3 - "$1" "$2" <<'PY'
import os, sys
path, size = sys.argv[1], int(sys.argv[2])
with open(path, "wb") as f:
    written = 0
    while written < size:
        block = os.urandom(min(1 << 20, size - written))
        f.write(block)
        written += len(block)
PY
}

sha256() {  # <file>
  python3 - "$1" <<'PY'
import hashlib, sys
h = hashlib.sha256()
with open(sys.argv[1], "rb") as f:
    for block in iter(lambda: f.read(1 << 20), b""):
        h.update(block)
print(h.hexdigest())
PY
}

# 目录树的「形状 + 内容」摘要：路径、类型、大小、内容哈希、symlink 目标。
tree_digest() {  # <dir>
  python3 - "$1" <<'PY'
import hashlib, os, sys
root = sys.argv[1]
entries = []
for dirpath, dirnames, filenames in os.walk(root):
    dirnames.sort()
    entries.append(("d", os.path.relpath(dirpath, root)))
    for name in sorted(filenames):
        path = os.path.join(dirpath, name)
        rel = os.path.relpath(path, root)
        if os.path.islink(path):
            entries.append(("l", rel, os.readlink(path)))
        else:
            with open(path, "rb") as f:
                entries.append(("f", rel, os.path.getsize(path), hashlib.sha256(f.read()).hexdigest()))
h = hashlib.sha256()
for entry in entries:
    h.update(repr(entry).encode())
print(h.hexdigest())
PY
}

# ---------------------------------------------------------------- relay

start_relay() {  # <rust|go> <logfile>
  local kind="$1" log="$2" base ports
  base="$(free_port_base)" || { echo "找不到空闲端口段" >&2; return 1; }
  ports="$base,$((base + 1)),$((base + 2)),$((base + 3)),$((base + 4))"

  # 全局 flag 放在子命令之前：urfave/cli 只认这个位置，clap 也接受
  # （Rust 侧的 --pass 声明为 global），所以两边用同一种写法。
  if [ "$kind" = "rust" ]; then
    "$CROC_RS_BIN" --pass "$RELAY_PASS" relay --host 127.0.0.1 --ports "$ports" >"$log" 2>&1 &
  else
    "$CROC_GO_BIN" --pass "$RELAY_PASS" relay --host 127.0.0.1 --ports "$ports" >"$log" 2>&1 &
  fi
  RELAY_PID=$!
  # 累计记一份：`stop_relay` 只收当前这个，而 `$RELAY_PID` 会被下一个用例覆盖——
  # 漏写一次 `stop_relay`，那个中继就再也没人收，退出码却仍是绿的。
  RELAY_PIDS+=("$RELAY_PID")

  if ! wait_for_port 127.0.0.1 "$base" 20; then
    echo "relay（$kind）20 秒内没有监听 127.0.0.1:$base，日志见 $log" >&2
    stop_relay
    return 1
  fi
  RELAY_BASE="$base"
  return 0
}

stop_relay() {
  [ -n "$RELAY_PID" ] || return 0
  kill "$RELAY_PID" 2>/dev/null
  wait "$RELAY_PID" 2>/dev/null
  RELAY_PID=""
}

relay_addr() { printf '127.0.0.1:%s' "$1"; }

# ---------------------------------------------------------------- 用例骨架

selected() {  # <id>
  [ -z "$CROC_INTEROP_ONLY" ] && return 0
  case ",$CROC_INTEROP_ONLY," in *",$1,"*) return 0 ;; esac
  return 1
}

# 一次传输：启动发送方（后台）→ 接收方（前台）→ 等发送方退出。
#
#   transfer <dir> <stdin|""> <sender...> -- <receiver...>
#
# `--` 分隔两边的命令；`stdin` 非空时作为接收方的标准输入（测交互式拒绝要用）。
transfer() {
  local dir="$1" stdin_src="$2"
  shift 2
  local sender=() receiver=() seen=0 arg
  for arg in "$@"; do
    if [ "$arg" = "--" ] && [ "$seen" -eq 0 ]; then
      seen=1
      continue
    fi
    if [ "$seen" -eq 0 ]; then sender+=("$arg"); else receiver+=("$arg"); fi
  done

  mkdir -p "$dir/send" "$dir/recv"

  ( cd "$dir/send" && "${sender[@]}" ) >"$dir/sender.log" 2>&1 &
  local sender_pid=$!

  # 给发送方一点时间连上 relay 并注册房间，否则接收方可能抢在房间建立之前连接。
  sleep 1

  local receiver_status=0
  if [ -n "$stdin_src" ]; then
    ( cd "$dir/recv" && "${receiver[@]}" ) <"$stdin_src" >"$dir/receiver.log" 2>&1
    receiver_status=$?
  else
    ( cd "$dir/recv" && "${receiver[@]}" ) >"$dir/receiver.log" 2>&1
    receiver_status=$?
  fi

  local sender_status=0 waited=0
  while kill -0 "$sender_pid" 2>/dev/null; do
    if [ "$waited" -ge "$CROC_INTEROP_TIMEOUT" ]; then
      kill "$sender_pid" 2>/dev/null
      sender_status=124
      break
    fi
    sleep 1
    waited=$((waited + 1))
  done
  # 先把 `wait` 的退出码收进变量再判断。写成 `[ ... ] || sender_status=$?` 是错的：
  # 那时 `$?` 是 `[` 自己的退出码（恒为 1），不是 `wait` 的。
  wait "$sender_pid" 2>/dev/null
  local waited_status=$?
  if [ "$sender_status" -ne 124 ]; then
    sender_status=$waited_status
  fi

  redact "$dir/sender.log"
  redact "$dir/receiver.log"

  if [ "$receiver_status" -ne 0 ] || [ "$sender_status" -ne 0 ]; then
    dim "    sender=$sender_status receiver=$receiver_status（日志见 $dir）"
    return 1
  fi
  return 0
}

CASE_ID=""
CASE_DESC=""
CASE_DIR=""

begin_case() {  # <id> <描述>
  CASE_ID="$1"
  CASE_DESC="$2"
  CASE_DIR="$WORK/$CASE_ID"
  # `send` / `recv` 在这里就建好：用例大多**先造载荷再调 transfer()**，
  # 而 transfer() 里才 mkdir 的话，make_file 会因为父目录不存在而失败。
  mkdir -p "$CASE_DIR/send" "$CASE_DIR/recv"
  if ! selected "$CASE_ID"; then
    SKIPPED=$((SKIPPED + 1))
    return 1
  fi
  printf '%s · %s\n' "$CASE_ID" "$CASE_DESC"
  return 0
}

# 跳过"依赖尚未实现的能力"的用例，并把理由记下来在结尾汇总。
#
# 为什么不让它红：恒红的门禁会逼着人学会无视红灯（README 里那句话）。但也不能装作
# 没这回事——所以**必须给出理由**，且理由会打进结尾的跳过清单里。
#
# 环境变量 CROC_INTEROP_FORCE_UNIMPLEMENTED=1 可以强制跑它们（预期会红，用来观察进度）。
skip_unimplemented() {  # <id> <理由>
  if [ -n "${CROC_INTEROP_FORCE_UNIMPLEMENTED:-}" ]; then
    dim "    （CROC_INTEROP_FORCE_UNIMPLEMENTED 已设置，仍然照跑）"
    return 1
  fi
  skip "$1（$2）"
  SKIPPED=$((SKIPPED + 1))
  SKIP_REASONS+=("$1  $2")
  return 0
}

finish_case() {  # <0|1>
  if [ "$1" -eq 0 ]; then
    ok "$CASE_ID"
    PASSED=$((PASSED + 1))
  else
    bad "$CASE_ID（$CASE_DESC）"
    FAILED=$((FAILED + 1))
    FAILED_CASES+=("$CASE_ID")
  fi
}

# 比对单文件是否逐字节一致。
same_file() {  # <期望文件> <实得文件>
  [ -f "$2" ] || { dim "    缺少 $2"; return 1; }
  [ "$(sha256 "$1")" = "$(sha256 "$2")" ] || { dim "    内容不一致：$2"; return 1; }
  return 0
}

# 检查日志里没有崩溃痕迹（"优雅报错"与"崩了"是两回事）。
no_crash() {  # <dir>
  if grep -qiE "panic|segmentation|SIGSEGV|stack overflow" "$1/sender.log" "$1/receiver.log" 2>/dev/null; then
    dim "    进程崩溃了（应优雅报错）"
    return 1
  fi
  return 0
}

# ---------------------------------------------------------------- I1–I5：基本收发

# I1 Rust → Rust，Rust relay，1 MB
case_i1() {
  begin_case I1 "Rust → Rust，Rust relay，1 MB" || return 0
  local result=1
  if start_relay rust "$WORK/relay-rust.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    make_file "$CASE_DIR/send/payload.bin" 1048576
    transfer "$CASE_DIR" "" \
      "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet \
        send --code "$CODE" --no-local payload.bin -- \
      "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet "$CODE"
    if [ $? -eq 0 ] && same_file "$CASE_DIR/send/payload.bin" "$CASE_DIR/recv/payload.bin"; then
      result=0
    fi
  fi
  stop_relay
  finish_case "$result"
}

# I2 Rust → Go，Rust relay，1 MB
case_i2() {
  begin_case I2 "Rust → Go，Rust relay，1 MB" || return 0
  local result=1
  if start_relay rust "$WORK/relay-rust.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    make_file "$CASE_DIR/send/payload.bin" 1048576
    transfer "$CASE_DIR" "" \
      "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet \
        send --code "$CODE" --no-local payload.bin -- \
      "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet "$CODE"
    if [ $? -eq 0 ] && same_file "$CASE_DIR/send/payload.bin" "$CASE_DIR/recv/payload.bin"; then
      result=0
    fi
  fi
  stop_relay
  finish_case "$result"
}

# I3 Go → Rust，Rust relay，1 MB
case_i3() {
  begin_case I3 "Go → Rust，Rust relay，1 MB" || return 0
  local result=1
  if start_relay rust "$WORK/relay-rust.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    make_file "$CASE_DIR/send/payload.bin" 1048576
    transfer "$CASE_DIR" "" \
      "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet \
        send --code "$CODE" --no-local payload.bin -- \
      "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet "$CODE"
    if [ $? -eq 0 ] && same_file "$CASE_DIR/send/payload.bin" "$CASE_DIR/recv/payload.bin"; then
      result=0
    fi
  fi
  stop_relay
  finish_case "$result"
}

# I4 Rust → Go，Go relay，100 MB
case_i4() {
  begin_case I4 "Rust → Go，Go relay，100 MB" || return 0
  local result=1
  if start_relay go "$WORK/relay-go.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    make_file "$CASE_DIR/send/payload.bin" $((100 * 1024 * 1024))
    transfer "$CASE_DIR" "" \
      "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet \
        send --code "$CODE" --no-local payload.bin -- \
      "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet "$CODE"
    if [ $? -eq 0 ] && same_file "$CASE_DIR/send/payload.bin" "$CASE_DIR/recv/payload.bin"; then
      result=0
    fi
  fi
  stop_relay
  finish_case "$result"
}

# I5 Go → Rust，Go relay，100 MB
case_i5() {
  begin_case I5 "Go → Rust，Go relay，100 MB" || return 0
  local result=1
  if start_relay go "$WORK/relay-go.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    make_file "$CASE_DIR/send/payload.bin" $((100 * 1024 * 1024))
    transfer "$CASE_DIR" "" \
      "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet \
        send --code "$CODE" --no-local payload.bin -- \
      "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet "$CODE"
    if [ $? -eq 0 ] && same_file "$CASE_DIR/send/payload.bin" "$CASE_DIR/recv/payload.bin"; then
      result=0
    fi
  fi
  stop_relay
  finish_case "$result"
}

# I6 Rust → Go，官方 relay，1 MB（需外网）
case_i6() {
  begin_case I6 "Rust → Go，官方 relay，1 MB（需外网）" || return 0
  if [ -n "$CROC_INTEROP_SKIP_NETWORK" ]; then
    skip "I6（CROC_INTEROP_SKIP_NETWORK 已设置）"
    SKIPPED=$((SKIPPED + 1))
    return 0
  fi
  local result=1
  make_file "$CASE_DIR/send/payload.bin" 1048576
  # 不传 --relay：走各端自己的默认（croc.schollz.com:9009）。
  transfer "$CASE_DIR" "" \
    "$CROC_RS_BIN" --yes --quiet send --code "$CODE" --no-local payload.bin -- \
    "$CROC_GO_BIN" --yes --quiet "$CODE"
  if [ $? -eq 0 ] && same_file "$CASE_DIR/send/payload.bin" "$CASE_DIR/recv/payload.bin"; then
    result=0
  fi
  finish_case "$result"
}

# I7 Rust → Rust / Rust → Go，Rust relay，目录树（空目录 + symlink）
#
# 两个方向都跑：目录层级是**线路上的约定**（`folder_remote` 里含不含目录自己的名字），
# 由接收方解释。只测 Rust↔Rust 的话，两边一起错也看不出来。
case_i7() {
  begin_case I7 "Rust → Rust / Rust → Go，Rust relay，目录树（含空目录与 symlink）" || return 0
  local result=1
  if start_relay rust "$WORK/relay-rust.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    local src="$CASE_DIR/send/tree"
    mkdir -p "$src/sub/deep/deeper" "$src/empty"
    make_file "$src/a.bin" 4096
    make_file "$src/sub/b.bin" 65536
    make_file "$src/sub/deep/deeper/c.txt" 128
    if ln -s a.bin "$src/link" 2>/dev/null; then
      dim "    （已创建 symlink，参与比对）"
    else
      dim "    （本平台不支持 symlink，该项自动跳过）"
    fi

    local want; want="$(tree_digest "$src")"
    local direction receiver got
    result=0
    for direction in rust go; do
      if [ "$direction" = "rust" ]; then receiver="$CROC_RS_BIN"; else receiver="$CROC_GO_BIN"; fi
      rm -rf "$CASE_DIR/recv"; mkdir -p "$CASE_DIR/recv"

      transfer "$CASE_DIR" "" \
        "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet \
          send --code "$CODE" --no-local tree -- \
        "$receiver" --relay "$r" --pass "$RELAY_PASS" --yes --quiet "$CODE"
      if [ $? -ne 0 ]; then
        dim "    Rust → ${direction}：传输失败"
        result=1
        continue
      fi
      got="$(tree_digest "$CASE_DIR/recv/tree")"
      if [ "$want" = "$got" ]; then
        dim "    Rust → ${direction}：树一致"
      else
        dim "    Rust → ${direction}：源树=$want"
        dim "                   目标树=$got"
        result=1
      fi
    done
  fi
  stop_relay
  finish_case "$result"
}

# I8 Rust → Go，Rust relay，--text
case_i8() {
  begin_case I8 "Rust → Go，Rust relay，--text" || return 0
  local result=1
  if start_relay rust "$WORK/relay-rust.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    local text="croc-rs interop 文本载荷 · $(date -u +%s)"
    transfer "$CASE_DIR" "" \
      "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet \
        send --code "$CODE" --no-local --text "$text" -- \
      "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet "$CODE"
    if [ $? -eq 0 ]; then
      # 文本落盘为 croc-stdin-<时间戳>，内容必须逐字节一致。
      local got=""
      for f in "$CASE_DIR"/recv/croc-stdin-*; do
        [ -f "$f" ] || continue
        got="$(cat "$f")"
      done
      if [ "$got" = "$text" ]; then
        result=0
      else
        dim "    期望：$text"
        dim "    实得：$got"
      fi
    fi
  fi
  stop_relay
  finish_case "$result"
}

# I9 Rust → Go，Rust relay，断点续传
case_i9() {
  begin_case I9 "Rust → Go，Rust relay，断点续传" || return 0
  local result=1
  if start_relay rust "$WORK/relay-rust.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    # 8 MB：够大到能在中途打断，又不至于让 CI 太久。
    make_file "$CASE_DIR/send/payload.bin" $((8 * 1024 * 1024))
    local expect; expect="$(sha256 "$CASE_DIR/send/payload.bin")"

    # 第一轮：接收方跑 2 秒后打断，留下半截文件。
    ( cd "$CASE_DIR/send" && "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" \
        --yes --quiet send --code "$CODE" --no-local payload.bin ) >"$CASE_DIR/sender-1.log" 2>&1 &
    local s1=$!
    sleep 1
    ( cd "$CASE_DIR/recv" && "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" \
        --yes --quiet "$CODE" ) >"$CASE_DIR/receiver-1.log" 2>&1 &
    local r1=$!
    sleep 2
    kill "$r1" 2>/dev/null; wait "$r1" 2>/dev/null
    kill "$s1" 2>/dev/null; wait "$s1" 2>/dev/null
    redact "$CASE_DIR/sender-1.log"
    redact "$CASE_DIR/receiver-1.log"

    # 第二轮：同名重传，接收方用 --overwrite 走续传。
    ( cd "$CASE_DIR/send" && "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" \
        --yes --quiet send --code "$CODE" --no-local payload.bin ) >"$CASE_DIR/sender-2.log" 2>&1 &
    local s2=$!
    sleep 1
    ( cd "$CASE_DIR/recv" && "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" \
        --yes --quiet --overwrite "$CODE" ) >"$CASE_DIR/receiver-2.log" 2>&1
    local r2=$?
    wait "$s2" 2>/dev/null
    redact "$CASE_DIR/sender-2.log"
    redact "$CASE_DIR/receiver-2.log"

    if [ "$r2" -eq 0 ] && [ "$expect" = "$(sha256 "$CASE_DIR/recv/payload.bin")" ]; then
      result=0
    else
      dim "    第二轮退出码=$r2"
    fi
  fi
  stop_relay
  finish_case "$result"
}

# I10 Rust → Go，Rust relay，同名文件的三种策略
case_i10() {
  begin_case I10 "Rust → Go，Rust relay，同名文件（覆盖 / 重命名 / 拒绝）" || return 0
  local result=1
  if start_relay rust "$WORK/relay-rust.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    result=0

    # 覆盖：接收目录预置同名文件（内容不同），--overwrite 后应被完整替换。
    local d="$CASE_DIR/overwrite"
    mkdir -p "$d/send" "$d/recv"
    make_file "$d/send/payload.bin" 65536
    make_file "$d/recv/payload.bin" 1024
    transfer "$d" "" \
      "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet \
        send --code "$CODE" --no-local payload.bin -- \
      "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet --overwrite "$CODE"
    if [ $? -ne 0 ] || ! same_file "$d/send/payload.bin" "$d/recv/payload.bin"; then
      dim "    覆盖策略未达预期"
      result=1
    fi

    # 重命名：原文件保持不变，新文件带 (1) 后缀。
    if [ "$result" -eq 0 ]; then
      d="$CASE_DIR/rename"
      mkdir -p "$d/send" "$d/recv"
      make_file "$d/send/payload.bin" 65536
      make_file "$d/recv/payload.bin" 1024
      transfer "$d" "" \
        "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet \
          send --code "$CODE" --no-local payload.bin -- \
        "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet --rename "$CODE"
      if [ $? -ne 0 ] || [ ! -f "$d/recv/payload (1).bin" ]; then
        dim "    重命名策略未达预期（应有 payload (1).bin）"
        result=1
      fi
    fi

    # 拒绝：不给 --overwrite/--rename/--yes，标准输入给 EOF ⇒ 提示收到 EOF ⇒ 视为拒绝。
    if [ "$result" -eq 0 ]; then
      d="$CASE_DIR/decline"
      mkdir -p "$d/send" "$d/recv"
      make_file "$d/send/payload.bin" 65536
      make_file "$d/recv/payload.bin" 1024
      local before; before="$(sha256 "$d/recv/payload.bin")"
      transfer "$d" /dev/null \
        "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet \
          send --code "$CODE" --no-local payload.bin -- \
        "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" --quiet "$CODE"
      # 接收方应失败退出，且原文件不被改动。
      if [ $? -eq 0 ] || [ "$before" != "$(sha256 "$d/recv/payload.bin")" ]; then
        dim "    拒绝策略未达预期"
        result=1
      fi
    fi
  fi
  stop_relay
  finish_case "$result"
}

# I11 Rust → Go，Rust relay，压缩开 / 关
case_i11() {
  begin_case I11 "Rust → Go，Rust relay，压缩开 / 关" || return 0
  local result=1
  if start_relay rust "$WORK/relay-rust.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    result=0
    local mode flags d
    for mode in compress nocompress; do
      flags=""
      [ "$mode" = "nocompress" ] && flags="--no-compress"
      d="$CASE_DIR/$mode"
      mkdir -p "$d/send" "$d/recv"
      # 高压缩比内容（重复字节）：压没压得住体现在耗时上，正确性上两者都必须一致。
      python3 -c "import sys; open(sys.argv[1],'wb').write(b'croc'*262144)" "$d/send/payload.bin"

      # shellcheck disable=SC2086  # $flags 需要按空格拆成多个参数
      transfer "$d" "" \
        "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet $flags \
          send --code "$CODE" --no-local payload.bin -- \
        "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet "$CODE"
      if [ $? -ne 0 ] || ! same_file "$d/send/payload.bin" "$d/recv/payload.bin"; then
        dim "    模式 $mode 未达预期"
        result=1
        break
      fi
    done
  fi
  stop_relay
  finish_case "$result"
}

# I12 Rust → Go，局域网本地直连（不走中继）
case_i12() {
  begin_case I12 "Rust → Go，局域网本地直连（--local，不启 relay）" || return 0
  # **`--local` 已经实现了**（内嵌 relay + UDP 组播发现，见 croc-engine/src/local.rs），
  # 但这个用例在单机上必然失败，原因在 Go 那一侧：Go 的 peerdiscovery 默认
  # `AllowSelf=false`，会**把源 IP 是本机的广播丢掉**（listener.go:130）。于是同一台
  # 机器上 Go 永远发现不到发送方——Go 发送 / Go 接收也一样失败，与本实现无关。
  # 要验证得两台机器；在此之前跳过。
  if skip_unimplemented I12 "--local 已实现，但 Go 的组播发现会过滤本机广播（AllowSelf=false），单机上发现不到"; then
    return 0
  fi
  # 不启 relay —— 这正是本用例要验证的：走不到中继也能传。
  make_file "$CASE_DIR/send/payload.bin" 262144
  local result=1
  transfer "$CASE_DIR" "" \
    "$CROC_RS_BIN" --local --yes --quiet send --code "$CODE" payload.bin -- \
    "$CROC_GO_BIN" --local --yes --quiet "$CODE"
  if [ $? -eq 0 ] && same_file "$CASE_DIR/send/payload.bin" "$CASE_DIR/recv/payload.bin"; then
    result=0
  fi
  finish_case "$result"
}

# I19 Rust → Rust，局域网本地直连（不走中继）
#
# 与 I12 同形，但两端都是 Rust：不用 Go 的组播发现，所以单机能跑。
# **不启 relay** —— 这正是本用例要验证的：全程碰不到任何外部中继。
case_i19() {
  begin_case I19 "Rust → Rust，局域网本地直连（--local，不启 relay）" || return 0
  make_file "$CASE_DIR/send/payload.bin" 262144
  local result=1
  transfer "$CASE_DIR" "" \
    "$CROC_RS_BIN" --local --yes --quiet send --code "$CODE" payload.bin -- \
    "$CROC_RS_BIN" --local --yes --quiet "$CODE"
  if [ $? -eq 0 ] && same_file "$CASE_DIR/send/payload.bin" "$CASE_DIR/recv/payload.bin"; then
    result=0
  fi
  finish_case "$result"
}

# I20 Rust → Go，--hash md5：接收方本地**已有同内容文件**，Go 拿它自己算的 md5 判定
# "不需要传"，于是发送方报 0 字节。摘要只要差一个字节，这里就会退化成真传输。
#
# 这是**跨实现**的摘要一致性验证——单测里钉的是 Go 生成的静态向量，这条走的是活的两端。
# 另外三种算法不在这里各跑一遍：`croc-engine/src/hash.rs` 的单测已用 Go 生成的向量钉住。
case_i20() {
  begin_case I20 "Rust → Go，--hash md5，接收方已有同内容文件（摘要跨实现一致）" || return 0
  local result=1
  if start_relay rust "$WORK/relay-rust.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    make_file "$CASE_DIR/send/payload.bin" 262144
    cp "$CASE_DIR/send/payload.bin" "$CASE_DIR/recv/payload.bin"
    transfer "$CASE_DIR" "" \
      "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet \
        send --hash md5 --code "$CODE" payload.bin -- \
      "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet "$CODE"
    # 判定看的是**发送方**报的字节数（Rust 自己的文案，比解析 Go 的输出稳）：0 才说明
    # Go 一个字节都没要——也就是它自己算出来的 md5 与我们清单里的一致。
    if [ $? -eq 0 ] && grep -q "0 字节" "$CASE_DIR/sender.log"; then
      result=0
    fi
  fi
  stop_relay
  finish_case "$result"
}
# I21：曲线 ed25519。
#
# 曲线由**接收方**决定（发送方照着用），所以 `--curve` 给在 Go 那一侧。盯的是五条曲线里
# 最后补上的那条：ed25519 的点编码与 Weierstrass 那几条不同（点整体塞在 x 坐标里、y 恒
# 为 0），编解码走偏就是握不上手——而不是报一条好看的错误。
case_i21() {
  begin_case I21 "Rust → Go，--curve ed25519（曲线由接收方决定）" || return 0
  local result=1
  if start_relay rust "$WORK/relay-rust.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    make_file "$CASE_DIR/send/payload.bin" 262144
    transfer "$CASE_DIR" "" \
      "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet \
        send --code "$CODE" payload.bin -- \
      "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet --curve ed25519 "$CODE"
    if [ $? -eq 0 ] && same_file "$CASE_DIR/send/payload.bin" "$CASE_DIR/recv/payload.bin" \
       && no_crash "$CASE_DIR"; then
      result=0
    fi
  fi
  stop_relay
  finish_case "$result"
}


# I13 Rust → Go，Rust relay，10 万文件清单
case_i13() {
  begin_case I13 "Rust → Go，Rust relay，10 万文件清单" || return 0
  local result=1
  if start_relay rust "$WORK/relay-rust.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    # 10 万个空文件：考的是分片清单（chunked-fileinfo-v1）的内存占用与批大小控制。
    python3 - "$CASE_DIR/send/many" <<'PY'
import os, sys
root = sys.argv[1]
for bucket in range(100):
    d = os.path.join(root, f"d{bucket:03d}")
    os.makedirs(d, exist_ok=True)
    for i in range(1000):
        open(os.path.join(d, f"f{i:04d}"), "wb").close()
PY

    transfer "$CASE_DIR" "" \
      "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet \
        send --code "$CODE" --no-local many -- \
      "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" --yes --quiet "$CODE"
    if [ $? -eq 0 ]; then
      local n
      n="$(find "$CASE_DIR/recv/many" -type f 2>/dev/null | wc -l | tr -d ' ')"
      if [ "$n" = "100000" ]; then
        result=0
      else
        dim "    收到 $n 个文件，期望 100000"
      fi
    fi
  fi
  stop_relay
  finish_case "$result"
}

# I14 Rust → Go，Rust relay，错误的中继口令
#
# 这里**不能用 `transfer()`**：它的判据是"两边都退出 0"，而本用例要的恰恰是
# "接收端非零退出"。而且中继口令对不上时发送端只会干等，用 `transfer()` 就得陪它
# 等到超时。所以手写：发送端照常起，接收端带错口令跑（它会立刻被中继拒掉），
# 再断言"接收端失败 + 双方都不崩"，最后收掉发送端。
#
# 注意"口令"指的是中继口令（`--pass`）。codephrase 是端到端口令，但**它同时决定
# 房间名**，两边给不同的 codephrase 会各自待在不同的房间里，根本碰不上面——
# 那样测的是"找不到对端"，不是"口令错"。
case_i14() {
  begin_case I14 "Rust → Go，Rust relay，错误的中继口令" || return 0
  local result=1
  if start_relay rust "$WORK/relay-rust.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    make_file "$CASE_DIR/send/payload.bin" 65536

    ( cd "$CASE_DIR/send" && "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" \
        --yes --quiet send --code "$CODE" --no-local payload.bin ) >"$CASE_DIR/sender.log" 2>&1 &
    local sender_pid=$!
    sleep 1

    local receiver_status=0
    ( cd "$CASE_DIR/recv" && "$CROC_GO_BIN" --relay "$r" --pass "definitely-not-$RELAY_PASS" \
        --yes --quiet "$CODE" ) >"$CASE_DIR/receiver.log" 2>&1
    receiver_status=$?

    kill "$sender_pid" 2>/dev/null
    wait "$sender_pid" 2>/dev/null
    redact "$CASE_DIR/sender.log"
    redact "$CASE_DIR/receiver.log"

    # 期望：接收端非零退出（中继拒绝），且双方都不崩，也没有文件落地。
    if [ "$receiver_status" -ne 0 ] && no_crash "$CASE_DIR" \
       && [ ! -f "$CASE_DIR/recv/payload.bin" ]; then
      result=0
    else
      dim "    错误口令竟然传成功了（receiver=$receiver_status），或进程崩溃"
    fi
  fi
  stop_relay
  finish_case "$result"
}

# I15 Rust → Go，传输中 SIGINT：优雅退出，不留临时文件
case_i15() {
  begin_case I15 "Rust → Go，Rust relay，传输中中断（SIGINT）" || return 0
  local result=1
  if start_relay rust "$WORK/relay-rust.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    make_file "$CASE_DIR/send/payload.bin" $((32 * 1024 * 1024))
    mkdir -p "$CASE_DIR/recv"

    ( cd "$CASE_DIR/send" && "$CROC_RS_BIN" --relay "$r" --pass "$RELAY_PASS" \
        --yes --quiet send --code "$CODE" --no-local payload.bin ) >"$CASE_DIR/sender.log" 2>&1 &
    local s=$!
    sleep 1
    ( cd "$CASE_DIR/recv" && "$CROC_GO_BIN" --relay "$r" --pass "$RELAY_PASS" \
        --yes --quiet "$CODE" ) >"$CASE_DIR/receiver.log" 2>&1 &
    local rp=$!
    sleep 2
    # SIGINT 等价于 Ctrl-C：应当清理临时文件后退出。
    kill -INT "$rp" 2>/dev/null
    local waited=0
    while kill -0 "$rp" 2>/dev/null; do
      [ "$waited" -ge 10 ] && break
      sleep 1
      waited=$((waited + 1))
    done
    kill -9 "$rp" 2>/dev/null; wait "$rp" 2>/dev/null
    kill "$s" 2>/dev/null; wait "$s" 2>/dev/null
    redact "$CASE_DIR/sender.log"
    redact "$CASE_DIR/receiver.log"

    # 不留 .part / .croc-tmp 之类的临时文件。
    local leftovers
    leftovers="$(find "$CASE_DIR/recv" \( -name '*.part' -o -name '*.croc-tmp' -o -name '*.croc-part' \) 2>/dev/null)"
    if [ -z "$leftovers" ] && no_crash "$CASE_DIR"; then
      result=0
    else
      dim "    残留临时文件：${leftovers:-（无）}"
    fi
  fi
  stop_relay
  finish_case "$result"
}

# I16–I18：`--transport derp` 在 pending 期间必须**降级到 relay 并成功**（不能崩）。
# 语义见 docs/agent/plan/tailscale.md —— tailscale 整块 PENDING。
case_derp() {  # <id> <发送方> <接收方> <relay kind> <描述后缀>
  local id="$1" sbin="$2" rbin="$3" relaykind="$4" suffix="$5"
  begin_case "$id" "--transport derp 降级为 relay（$suffix）" || return 0
  local result=1
  if start_relay "$relaykind" "$WORK/relay-$relaykind.log"; then
    local r; r="$(relay_addr "$RELAY_BASE")"
    make_file "$CASE_DIR/send/payload.bin" 262144
    transfer "$CASE_DIR" "" \
      "$sbin" --relay "$r" --pass "$RELAY_PASS" --yes --quiet \
        send --code "$CODE" --no-local --transport derp payload.bin -- \
      "$rbin" --relay "$r" --pass "$RELAY_PASS" --yes --quiet "$CODE"
    if [ $? -eq 0 ] && same_file "$CASE_DIR/send/payload.bin" "$CASE_DIR/recv/payload.bin" \
       && no_crash "$CASE_DIR"; then
      result=0
    fi
  fi
  stop_relay
  finish_case "$result"
}

# ---------------------------------------------------------------- 主流程

cleanup() {
  stop_relay
  # 兜底：把本次跑过的中继全部收掉。`stop_relay` 只认 `$RELAY_PID`（当前这个），
  # 用例漏写一次它，那份中继就会一直留在进程表里——而矩阵的退出码照样是绿的。
  local pid
  for pid in ${RELAY_PIDS[@]+"${RELAY_PIDS[@]}"}; do
    kill "$pid" 2>/dev/null
  done
  if [ -z "$CROC_INTEROP_KEEP" ] && [ -n "$WORK" ] && [ -d "$WORK" ]; then
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

main() {
  if ! require_bins; then
    exit 1
  fi

  WORK="$(mktemp -d "${TMPDIR:-/tmp}/croc-interop.XXXXXX")" || exit 1
  # Git for Windows 的 `TMPDIR` 是 Windows 形式（`C:\Users\…\Temp`）。直接把它交给
  # `rm -rf` 会被安全删除策略拒掉（`SAFE_DELETE_INVALID_PATH`），临时目录就清不掉了。
  if command -v cygpath >/dev/null 2>&1; then
    WORK="$(cygpath -u "$WORK")"
  fi
  mkdir -p "$LOG_DIR"

  log "互通矩阵（Rust ↔ Go v11.5.4）"
  log "  Rust  $("$CROC_RS_BIN" --version 2>/dev/null || echo '(unknown)')"
  log "  Go    $("$CROC_GO_BIN" --version 2>/dev/null | head -1 || echo '(unknown)')"
  log "  只跑=${CROC_INTEROP_ONLY:-全部}  工作目录=$WORK"
  log ""

  case_i1
  case_i2
  case_i3
  case_i4
  case_i5
  case_i6
  case_i7
  case_i8
  case_i9
  case_i10
  case_i11
  case_i12
  case_i13
  case_i14
  case_i15
  case_i19
  case_i20
  case_i21
  case_derp I16 "$CROC_RS_BIN" "$CROC_GO_BIN" go   "Rust 发送 / Go 接收 / Go relay"
  case_derp I17 "$CROC_RS_BIN" "$CROC_RS_BIN" rust "Rust 两端 / Rust relay"
  case_derp I18 "$CROC_RS_BIN" "$CROC_GO_BIN" rust "Rust 发送 / Go 接收 / Rust relay"

  log ""
  log "通过 $PASSED · 失败 $FAILED · 跳过 $SKIPPED"
  if [ "${#SKIP_REASONS[@]}" -gt 0 ]; then
    log "跳过的用例："
    local reason
    for reason in "${SKIP_REASONS[@]}"; do
      log "  $reason"
    done
  fi
  if [ "$FAILED" -gt 0 ]; then
    log "失败用例：${FAILED_CASES[*]}"
    [ -n "$CROC_INTEROP_KEEP" ] && log "日志保留在：$WORK"
    exit 1
  fi
  exit 0
}

main "$@"
