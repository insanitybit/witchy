// An Exec program that starts as many processes as it can (up to 200), each waiting, and says
// how many: RLIMIT_NPROC (the plan's exec-processes beyond the user's count) bounds it.
#include <stdio.h>
#include <unistd.h>
int main(void) {
    int started = 0;
    for (int i = 0; i < 200; ++i) {
        pid_t child = fork();
        if (child == 0) {
            pause();
            _exit(0);
        }
        if (child < 0)
            break;
        ++started;
    }
    printf("%d\n", started);
    return 0;
}
