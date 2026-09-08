#!/usr/bin/env bash
# Build the witchy binary in the background; poll status/build.status.
source "$(dirname "$0")/common.sh"
rm -f "$STATUS/build.status"
nohup bash -c "timeout 2400 cargo build -p witchy > '$STATUS/build.log' 2>&1; echo \"exit=\$?\" > '$STATUS/build.status'" >/dev/null 2>&1 &
disown
echo "building; poll $STATUS/build.status"
