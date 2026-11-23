#!/usr/bin/env bash
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$(dirname "$here")/via-disk-metric.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export VIA_TEXTFILE_DIR="$tmp/textfile"
prom="$VIA_TEXTFILE_DIR/via_disk.prom"

cat >"$tmp/fake-df" <<'DF'
#!/usr/bin/env bash
echo "Filesystem 1-blocks Used Available Capacity Mounted on"
echo "/dev/root 1000 400 600 40% /"
echo "/dev/nvme1n1 5000 1000 4000 20% /var/lib/via"
DF
chmod +x "$tmp/fake-df"
export VIA_DF_CMD="$tmp/fake-df"

fails=0
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1 (expected '$3', got '$2')"; fails=$((fails + 1)); fi
}

VIA_NOW=1234 "$script"
check "last run timestamp" "$(grep -F 'via_disk_last_run_timestamp_seconds' "$prom" | grep -v TYPE | cut -d' ' -f2)" 1234
check "root avail" "$(grep -F 'via_disk_avail_bytes{mountpoint="/"}' "$prom" | cut -d' ' -f2)" 600
check "root size" "$(grep -F 'via_disk_size_bytes{mountpoint="/"}' "$prom" | cut -d' ' -f2)" 1000
check "data avail" "$(grep -F 'via_disk_avail_bytes{mountpoint="/var/lib/via"}' "$prom" | cut -d' ' -f2)" 4000
check "data size" "$(grep -F 'via_disk_size_bytes{mountpoint="/var/lib/via"}' "$prom" | cut -d' ' -f2)" 5000
check "no temp files left" "$(find "$VIA_TEXTFILE_DIR" -name '.*' -type f | wc -l | tr -d ' ')" 0

cat >"$tmp/fake-df" <<'DF'
#!/usr/bin/env bash
echo "Filesystem 1-blocks Used Available Capacity Mounted on"
echo "/dev/root 1000 400 600 40% /"
echo "/dev/root 1000 400 600 40% /"
DF
VIA_NOW=1 "$script"
check "same filesystem listed once" "$(grep -c '^via_disk_avail_bytes' "$prom")" 1

[ "$fails" -eq 0 ]
