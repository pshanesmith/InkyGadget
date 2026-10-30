#!/usr/bin/env bash
# One-screen health summary of the frame.
echo "version:  $(cat /opt/inkyframe/VERSION 2>/dev/null || echo unknown)"
echo "service:  $(systemctl is-active inkyframe 2>&1) (restarts: $(systemctl show inkyframe -p NRestarts --value 2>/dev/null))"
echo "smbd:     $(systemctl is-active smbd 2>&1)"
if [[ -f /var/lib/inkyframe/state.json ]]; then
  last=$(sed -n 's/.*"last_refresh": *\([0-9]*\).*/\1/p' /var/lib/inkyframe/state.json)
  [[ -n $last ]] && echo "refresh:  last $(date -d "@$last" '+%F %T') ($(( ($(date +%s) - last) / 3600 )) h ago)"
fi
echo "photos:   $(find /srv/inkyframe/Images -type f 2>/dev/null | wc -l) files, $(ls /var/lib/inkyframe/cache 2>/dev/null | grep -c '\.jpg$') prepared"
echo "memory:   $(free -m | awk '/Mem:/{print $7 " MB available of " $2}')"
echo "disk:     $(df -h --output=avail / | tail -1 | tr -d ' ') free"
command -v vcgencmd >/dev/null && echo "pi:       $(vcgencmd measure_temp) $(vcgencmd get_throttled)"
echo "clock:    $([[ -e /run/systemd/timesync/synchronized ]] && echo synchronized || echo NOT synchronized)"
echo "recent log:"; journalctl -u inkyframe --no-pager -n 8 2>/dev/null | cut -c1-160
