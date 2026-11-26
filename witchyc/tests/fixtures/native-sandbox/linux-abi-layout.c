#include <netinet/in.h>
#include <netdb.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/auxv.h>
#include <stddef.h>
#include <poll.h>
#include <signal.h>
#include <sys/resource.h>
#include <sys/random.h>
#include <sys/wait.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>
#include <linux/openat2.h>
#include <sys/syscall.h>
#define CHECK(T, S, A) _Static_assert(sizeof(T) == S && _Alignof(T) == A, #T)
#define FIELD(T,F,O) _Static_assert(offsetof(T,F) == O, #T "." #F)
CHECK(struct open_how, 24, 8);
FIELD(struct open_how, flags, 0);
FIELD(struct open_how, mode, 8);
FIELD(struct open_how, resolve, 16);
_Static_assert(SYS_openat2 == 437, "openat2 syscall number");
_Static_assert(RESOLVE_NO_SYMLINKS == 0x04 && RESOLVE_BENEATH == 0x08 &&
               RESOLVE_NO_XDEV == 0x01, "openat2 resolve flags");
CHECK(struct pollfd, 8, 4);
FIELD(struct pollfd, fd, 0);
FIELD(struct pollfd, events, 4);
FIELD(struct pollfd, revents, 6);
CHECK(struct timespec, 16, 8);
FIELD(struct timespec, tv_nsec, 8);
CHECK(struct rlimit, 16, 8);
FIELD(struct rlimit, rlim_max, 8);
CHECK(siginfo_t, 128, 8);
FIELD(siginfo_t, si_pid, 16);
FIELD(siginfo_t, si_status, 24);
_Static_assert(sizeof(nfds_t) == 8, "poll count");
#include <sys/stat.h>
CHECK(struct stat, 128, 8);
FIELD(struct stat, st_mode, 16);
FIELD(struct stat, st_size, 48);

CHECK(sigset_t, 128, 8);
CHECK(struct sigaction, 152, 8);
FIELD(struct sigaction, sa_handler, 0);
FIELD(struct sigaction, sa_mask, 8);
FIELD(struct sigaction, sa_flags, 136);
FIELD(struct sigaction, sa_restorer, 144);
CHECK(struct timeval, 16, 8);
FIELD(struct timeval, tv_usec, 8);
CHECK(struct itimerval, 32, 8);
FIELD(struct itimerval, it_value, 16);
_Static_assert(SIGALRM == 14 && SIG_UNBLOCK == 1 && ITIMER_REAL == 0,
               "deadline signal and timer constants");

#define TYPE(S, T) _Static_assert(__builtin_types_compatible_p(__typeof__(S), T), #S)
TYPE(&mmap, void *(*)(void *, size_t, int, int, int, off_t));
TYPE(&mprotect, int (*)(void *, size_t, int));
TYPE(&munmap, int (*)(void *, size_t));
_Static_assert((MAP_PRIVATE | MAP_ANONYMOUS) == 0x22, "task mapping flags");
_Static_assert(PROT_NONE == 0 && (PROT_READ | PROT_WRITE) == 3, "task mapping protections");
TYPE(&malloc, void *(*)(size_t));
_Static_assert(_Alignof(max_align_t) >= 16, "task allocation alignment");
TYPE(&free, void (*)(void *));
_Static_assert(sizeof(off_t) == sizeof(int64_t) && (off_t)-1 < 0, "mmap offset");
TYPE(&getauxval, unsigned long (*)(unsigned long));
_Static_assert(AT_PAGESZ == 6, "task page-size selector");
_Static_assert(sizeof(unsigned long) == sizeof(uintptr_t), "getauxval word");
TYPE(&poll, int (*)(struct pollfd *, nfds_t, int));
TYPE(&waitid, int (*)(idtype_t, id_t, siginfo_t *, int));
TYPE(&sigaction, int (*)(int, const struct sigaction *, struct sigaction *));
TYPE(&sigemptyset, int (*)(sigset_t *));
TYPE(&sigaddset, int (*)(sigset_t *, int));
TYPE(&sigprocmask, int (*)(int, const sigset_t *, sigset_t *));
TYPE(&setitimer, int (*)(__itimer_which_t, const struct itimerval *, struct itimerval *));
_Static_assert(sizeof(__itimer_which_t) == sizeof(int), "setitimer selector width");
_Static_assert(sizeof(((struct timeval *)0)->tv_usec) == sizeof(int64_t),
               "timeval microseconds width");
TYPE(&clock_gettime, int (*)(clockid_t, struct timespec *));
_Static_assert(CLOCK_REALTIME == 0, "runtime realtime clock");
TYPE(&getrandom, ssize_t (*)(void *, size_t, unsigned int));
TYPE(&nanosleep, int (*)(const struct timespec *, struct timespec *));
_Static_assert(P_PID == 1, "waitid PID selector");
_Static_assert(WNOHANG == 1 && WEXITED == 4 && WNOWAIT == 0x01000000,
               "waitid flags");
_Static_assert(EINTR == 4 && ECHILD == 10, "waitid error codes");
_Static_assert(sizeof(idtype_t) == sizeof(int32_t), "waitid kind width");
_Static_assert(sizeof(id_t) == sizeof(uint32_t), "waitid id width");
_Static_assert(sizeof(clockid_t) == sizeof(int32_t), "clock id width");

TYPE(&fstat, int (*)(int, struct stat *));
TYPE(&fstatat, int (*)(int, const char *, struct stat *, int));
FIELD(struct stat, st_dev, 0);
FIELD(struct stat, st_ino, 8);
FIELD(struct stat, st_nlink, 20);
FIELD(struct stat, st_uid, 24);
_Static_assert(sizeof(((struct stat *)0)->st_size) == sizeof(int64_t), "stat size field");

TYPE(&readdir, struct dirent *(*)(DIR *));
FIELD(struct dirent, d_type, 18);
FIELD(struct dirent, d_name, 19);
TYPE(&fdopendir, DIR *(*)(int));
TYPE(&closedir, int (*)(DIR *));

TYPE(&sendmsg, ssize_t (*)(int, const struct msghdr *, int));
TYPE(&recvmsg, ssize_t (*)(int, struct msghdr *, int));
CHECK(struct iovec, 16, 8);
FIELD(struct iovec, iov_base, 0);
FIELD(struct iovec, iov_len, 8);
CHECK(struct msghdr, 56, 8);
FIELD(struct msghdr, msg_name, 0);
FIELD(struct msghdr, msg_namelen, 8);
FIELD(struct msghdr, msg_iov, 16);
FIELD(struct msghdr, msg_iovlen, 24);
FIELD(struct msghdr, msg_control, 32);
FIELD(struct msghdr, msg_controllen, 40);
FIELD(struct msghdr, msg_flags, 48);
CHECK(struct cmsghdr, 16, 8);
FIELD(struct cmsghdr, cmsg_len, 0);
FIELD(struct cmsghdr, cmsg_level, 8);
FIELD(struct cmsghdr, cmsg_type, 12);
_Static_assert(CMSG_LEN(sizeof(int)) == 20, "one descriptor message");
_Static_assert(CMSG_SPACE(sizeof(int)) == 24, "one descriptor storage");

_Static_assert(SOL_SOCKET == 1 && SCM_RIGHTS == 1, "descriptor control type");
_Static_assert(MSG_NOSIGNAL == 0x4000 && MSG_CMSG_CLOEXEC == 0x40000000 &&
               MSG_CTRUNC == 8, "descriptor transport flags");

CHECK(struct sockaddr_storage, 128, 8);
_Static_assert(sizeof(socklen_t) == 4, "socket address length");
CHECK(struct addrinfo, 48, 8);
FIELD(struct addrinfo, ai_flags, 0);
FIELD(struct addrinfo, ai_family, 4);
FIELD(struct addrinfo, ai_socktype, 8);
FIELD(struct addrinfo, ai_protocol, 12);
FIELD(struct addrinfo, ai_addrlen, 16);
FIELD(struct addrinfo, ai_addr, 24);
FIELD(struct addrinfo, ai_canonname, 32);
FIELD(struct addrinfo, ai_next, 40);
TYPE(&getaddrinfo, int (*)(const char *, const char *, const struct addrinfo *, struct addrinfo **));
TYPE(&freeaddrinfo, void (*)(struct addrinfo *));

_Static_assert(MAP_PRIVATE == 2 && MAP_ANONYMOUS == 0x20, "task mapping flags");
_Static_assert(PROT_NONE == 0 && (PROT_READ | PROT_WRITE) == 3, "task protections");
_Static_assert(AT_PAGESZ == 6, "task page-size selector");

TYPE(&fcntl, int (*)(int, int, ...));
TYPE(&close, int (*)(int));
TYPE(&dup2, int (*)(int, int));
TYPE(&strdup, char *(*)(const char *));
_Static_assert(F_GETFD == 1 && F_SETFD == 2 && FD_CLOEXEC == 1 &&
               F_DUPFD_CLOEXEC == 1030, "descriptor flag commands");

TYPE(&read, ssize_t (*)(int, void *, size_t));
TYPE(&write, ssize_t (*)(int, const void *, size_t));
TYPE(&pread, ssize_t (*)(int, void *, size_t, off_t));
TYPE(&send, ssize_t (*)(int, const void *, size_t, int));
TYPE(&recv, ssize_t (*)(int, void *, size_t, int));
_Static_assert(sizeof(ssize_t) == sizeof(int64_t) && (ssize_t)-1 < 0,
               "byte I/O result width");
_Static_assert(sizeof(size_t) == sizeof(uint64_t), "byte I/O length width");

TYPE(&socketpair, int (*)(int, int, int, int *));
TYPE(&accept, int (*)(int, struct sockaddr *, socklen_t *));
TYPE(&bind, int (*)(int, const struct sockaddr *, socklen_t));
TYPE(&connect, int (*)(int, const struct sockaddr *, socklen_t));
TYPE(&getsockname, int (*)(int, struct sockaddr *, socklen_t *));
TYPE(&getsockopt, int (*)(int, int, int, void *, socklen_t *));
TYPE(&setsockopt, int (*)(int, int, int, const void *, socklen_t));
_Static_assert(SOL_SOCKET == 1 && SO_RCVTIMEO == 20 && SO_SNDTIMEO == 21 && SO_ERROR == 4,
               "socket timeout and error options");

CHECK(struct sockaddr_in, 16, 4);
CHECK(struct sockaddr_in6, 28, 4);
FIELD(struct sockaddr_in, sin_port, 2);
FIELD(struct sockaddr_in6, sin6_port, 2);
