#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

extern int probe_running_child(uint32_t child);
extern int probe(uint32_t child, int no_child_error);

int main(void) {
    int gate[2];
    if (pipe(gate) == -1) {
        perror("pipe");
        return 1;
    }
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }
    if (child == 0) {
        close(gate[1]);
        char byte;
        while (read(gate[0], &byte, 1) == -1 && errno == EINTR) {
        }
        _exit(42);
    }

    close(gate[0]);
    int running_result = probe_running_child((uint32_t)child);
    close(gate[1]);
    int result = probe((uint32_t)child, ECHILD);
    if (result != 0) {
        while (waitpid(child, NULL, 0) == -1 && errno == EINTR) {
        }
        fprintf(stderr, "Darwin waitid probe failed with result %d\n", result);
        return 1;
    }
    if (running_result != 0) {
        fputs("Darwin waitid reported an event for a running child\n", stderr);
        return 1;
    }
    puts("Darwin waitid running, peek, reap, and ECHILD passed");
    return 0;
}
