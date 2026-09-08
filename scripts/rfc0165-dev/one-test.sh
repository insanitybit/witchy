#!/usr/bin/env bash
# usage: one-test.sh <filter> [extra cargo test args]; poll status/one.status.
source "$(dirname "$0")/common.sh"
filter=$1; shift
rm -f "$STATUS/one.status"
nohup bash -c "timeout 2400 cargo test --test misc '$filter' $* > '$STATUS/one.log' 2>&1; echo \"exit=\$?\" > '$STATUS/one.status'" >/dev/null 2>&1 &
disown
echo "testing $filter; poll $STATUS/one.status"
