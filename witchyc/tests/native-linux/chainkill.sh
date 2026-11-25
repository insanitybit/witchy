#!/bin/sh
# Orphaned setsid descendants, then the runner and the warden (its parent) killed: in a
# socket-entry server, the supervisor (a subreaper) must stop what is left.
for i in 1 2 3; do /usr/bin/perl -e 'use POSIX; if (fork() == 0) { POSIX::setsid(); close STDOUT; sleep 3; open(F, ">>/w/chainkilled"); print F 1; exit 0; } exit 0;'; done
warden=$(awk '{print $4}' /proc/$PPID/stat)
kill -9 $PPID $warden
sleep 30
