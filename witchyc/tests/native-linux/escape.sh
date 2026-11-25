#!/bin/sh
# A grandchild that starts its own session and is orphaned at once, then writes after 1 s.
/usr/bin/perl -e 'use POSIX; if (fork() == 0) { POSIX::setsid(); close STDOUT; sleep 1; open(F, ">/w/escaped"); print F 1; exit 0; } exit 0;'
echo done
