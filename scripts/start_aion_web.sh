#!/bin/bash
# 启动 aion web :18080(完全 detach)
cd ~/AION
pkill -9 -f 'aion web' 2>/dev/null
sleep 1
setsid nohup ./target/release/aion web --port 18080 >> ~/AION/logs/aion-web.log 2>&1 < /dev/null &
sleep 2
pgrep -fa 'aion web'
ss -tlnp | grep 18080
curl -sS -o /dev/null -w 'http=%{http_code}\n' --connect-timeout 3 http://127.0.0.1:18080/
echo AION_STARTED
