#!/bin/bash
# 拉最新代码并重新部署 aion web
set -e
cd ~/AION
source ~/.cargo/env
echo '=== pulling latest ==='
git pull origin main 2>&1 | tail -3
echo '=== building release ==='
nice -n 19 env CARGO_BUILD_JOBS=2 cargo build --release -p aion 2>&1 | tail -3
echo '=== restarting ==='
pkill -9 -f 'aion web' 2>/dev/null || true
sleep 1
setsid nohup ./target/release/aion web --port 18080 >> ~/AION/logs/aion-web.log 2>&1 < /dev/null &
sleep 3
pgrep -fa 'aion web'
ss -tlnp | grep 18080
curl -sS -o /dev/null -w 'local_18080=%{http_code}\n' http://127.0.0.1:18080/
curl -sS http://127.0.0.1:18080/api/health | head -c 200; echo
echo DEPLOY_DONE
