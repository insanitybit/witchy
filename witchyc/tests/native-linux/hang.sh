#!/bin/sh
# Orphaned setsid descendants, then a program that never ends: the guest's deadline stops it.
for i in 1 2 3 4 5; do /usr/bin/perl -e 'use POSIX; if (fork() == 0) { POSIX::setsid(); close STDOUT; sleep 30; open(F, ">>/w/hung"); print F 1; exit 0; } exit 0;'; done
exec sleep 1000
