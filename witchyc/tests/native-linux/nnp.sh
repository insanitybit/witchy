#!/bin/sh
# What a program run through Exec holds: no_new_privs (inherited from the launch) and, even when
# the launch runs as root, no capability in any set.
grep -E '^(NoNewPrivs|CapEff|CapBnd|CapAmb):' /proc/self/status | tr -d ' \t' | tr '\n' ' '
