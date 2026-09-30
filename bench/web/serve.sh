#!/bin/bash
# Start / stop a `fvol serve` instance for UI tests (memory-capped via limit.sh).
#   bench/web/serve.sh start NAME PORT IMAGE [extra fvol serve args...]
#   bench/web/serve.sh stop NAME
# Logs and output dirs live under testdata/scratch/webui/NAME (disk, not tmpfs).
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
# the untracked test data (testdata/, bench/ref/, bench/venv/, volatility3/) is in the main checkout,
# which linked worktrees find through git; FASTVOL_DATA overrides
DATA=${FASTVOL_DATA:-$(dirname "$(git -C "$ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || echo "$ROOT/.git")")}
BIN=${BIN:-$ROOT/target/fast/fvol}
SCR=$DATA/testdata/scratch/webui
cmd=$1; name=$2
dir=$SCR/$name
mkdir -p "$dir"
# the fvol process itself (not the limit.sh / systemd-run wrappers)
volpid() { pgrep -f -- "^[^ ]*/fvol serve .*--token testtoken-$name-0123456789" | head -1; }
stop() {
  # wrappers still queued for a limit.sh slot would start a stale server later: drop them too
  pkill -f -- "limit.sh -m 6G .*--token testtoken-$name-0123456789" 2>/dev/null
  local p; p=$(volpid)
  [ -n "$p" ] && kill "$p" 2>/dev/null
  for _ in $(seq 1 50); do [ -z "$(volpid)" ] && break; sleep 0.1; done
}
case $cmd in
  start)
    port=$3; img=$4; shift 4
    stop
    # a HOME of its own: test servers save their analyses there, never in the real ~/.fvol
    # (the caches stay shared: XDG_CACHE_HOME points at the real ~/.cache)
    mkdir -p "$dir/home"
    XDG_CACHE_HOME="${XDG_CACHE_HOME:-$HOME/.cache}" HOME="$dir/home" \
    nohup "$ROOT/bench/scripts/limit.sh" -m 6G "$BIN" serve -f "$img" --port "$port" \
      --token "testtoken-$name-0123456789" -o "$dir/out" "$@" > "$dir/serve.log" 2>&1 &
    for _ in $(seq 1 6000); do   # limit.sh may wait for a free slot
      pid=$(volpid)
      if [ -n "$pid" ] && curl -s -o /dev/null "http://127.0.0.1:$port/favicon.svg"; then
        echo "started $name on $port (pid $pid)"; exit 0
      fi
      sleep 0.1
    done
    echo "failed to start"; cat "$dir/serve.log"; exit 1;;
  stop) stop; echo "stopped $name";;
esac
