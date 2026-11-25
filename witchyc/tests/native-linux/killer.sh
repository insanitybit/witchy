#!/bin/sh
# A program that leaves orphaned setsid descendants and then kills the runner (its parent): the
# broker, a subreaper, must stop what is left.
for i in 1 2 3; do /usr/bin/perl -e 'use POSIX; if (fork() == 0) { POSIX::setsid(); close STDOUT; sleep 3; open(F, ">>/w/killed"); print F 1; exit 0; } exit 0;'; done
kill -9 $PPID
sleep 30
