#!/bin/bash
# 拉最新代码并重新部署 aion web
set -e
cd ~/AION
source ~/.cargo/env
# 前端静态目录:web 进程按请求读这里,改 index.html 后 git pull 即生效,
# 不必重编 Rust。缺省指向本仓库的前端源码目录。
export AION_STATIC_DIR="$HOME/AION/crates/aion/static"
echo '=== pulling latest ==='
git pull origin main 2>&1 | tail -3
echo '=== building release ==='
nice -n 19 env CARGO_BUILD_JOBS=2 cargo build --release -p aion 2>&1 | tail -3
echo '=== restarting ==='
pkill -9 -f 'aion web' 2>/dev/null || true
sleep 1
setsid nohup env AION_STATIC_DIR="$AION_STATIC_DIR" ./target/release/aion web --port 18080 >> ~/AION/logs/aion-web.log 2>&1 < /dev/null &
sleep 3
pgrep -fa 'aion web'
ss -tlnp | grep 18080
curl -sS -o /dev/null -w 'local_18080=%{http_code}\n' http://127.0.0.1:18080/
curl -sS http://127.0.0.1:18080/api/health | head -c 200; echo
echo DEPLOY_DONE
