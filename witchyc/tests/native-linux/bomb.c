// An Exec program that leaves 1500 descendants behind, each in a session of its own, plus one
// that keeps forking: what the runner's sweep must stop completely (or fail the run).
#include <signal.h>
#include <stdlib.h>
#include <unistd.h>
int main(void) {
    for (int i = 0; i < 1500; ++i)
        if (fork() == 0) {
            setsid();
            pause();
            _exit(0);
        }
    if (fork() == 0) {
        setsid();
        for (;;)
            if (fork() == 0) {
                pause();
                _exit(0);
            } else
                usleep(1000);
    }
    write(1, "spawned\n", 8);
    return 0;
}
