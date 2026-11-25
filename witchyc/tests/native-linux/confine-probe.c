#include <sys/wait.h>
#include <sys/syscall.h>
#include <signal.h>
// The Linux broker's confinement, from outside the broker: after witchy_confine_broker over one
// read root and one planned port, a file outside the root, a write inside it, a connection to
// another port and an exec are each refused, and the root still reads.
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>
bool witchy_confine_broker(int read_root, int write_root, int output_parent, const uint16_t *ports, unsigned port_count, int signal_fd, int message_fd);
static int refused(int result) { return result < 0 && (errno == EACCES || errno == EPERM); }
int main(void) {
    int root = open("/w/notes", O_RDONLY | O_DIRECTORY);
    uint16_t port = 47031;
    // The one process the broker may signal (as its warden): a child, by a pidfd opened now;
    // another process's pidfd, also opened now, must stay unusable.
    pid_t warden = fork();
    if (warden == 0) {
        pause();
        _exit(0);
    }
    int warden_fd = (int)syscall(SYS_pidfd_open, warden, 0), other_fd = (int)syscall(SYS_pidfd_open, getppid(), 0);
    if (warden_fd < 0 || other_fd < 0)
        return 11;
    if (root < 0 || !witchy_confine_broker(root, -1, -1, &port, 1, warden_fd, -1))
        return 10;
    int fails = 0;
    int inside = openat(root, "a.txt", O_RDONLY);
    if (inside < 0) { puts("FAIL the root no longer reads"); ++fails; }
    if (!refused(open("/etc/hostname", O_RDONLY))) { puts("FAIL a file outside the root opened"); ++fails; }
    if (!refused(openat(root, "new.txt", O_WRONLY | O_CREAT, 0600))) { puts("FAIL a read root was written"); ++fails; }
    int s = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in other = { .sin_family = AF_INET, .sin_port = htons(47032), .sin_addr.s_addr = htonl(INADDR_LOOPBACK) };
    if (s >= 0 && !refused(connect(s, (struct sockaddr *)&other, sizeof other))) { puts("FAIL an unplanned port was dialled"); ++fails; }
    // No signal by number, no new pidfd, no signal through another pidfd; the warden's works.
    if (!refused(kill(getppid(), 0))) { puts("FAIL kill was allowed"); ++fails; }
    if (!refused((int)syscall(SYS_pidfd_open, getppid(), 0))) { puts("FAIL pidfd_open was allowed"); ++fails; }
    if (!refused((int)syscall(SYS_pidfd_send_signal, other_fd, 0, NULL, 0))) { puts("FAIL another process's pidfd was signalled"); ++fails; }
    if (syscall(SYS_pidfd_send_signal, warden_fd, SIGKILL, NULL, 0) != 0) { puts("FAIL the warden could not be signalled"); ++fails; }
    siginfo_t seen;
    if (waitid((idtype_t)3, (id_t)warden_fd, &seen, WEXITED) != 0) { puts("FAIL the warden could not be waited for"); ++fails; }
    char *argv[] = {"/bin/true", NULL};
    execv("/bin/true", argv);
    if (errno != EACCES && errno != EPERM) { puts("FAIL exec was not refused"); ++fails; }
    if (!fails) puts("confined");
    return fails;
}
