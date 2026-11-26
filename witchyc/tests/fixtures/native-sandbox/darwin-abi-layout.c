#include <CommonCrypto/CommonDigest.h>
#include <netinet/in.h>
#include <netdb.h>
#include <sys/socket.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <libproc.h>
#include <mach/vm_types.h>
#include <sys/proc_info.h>
#include <mach/vm_page_size.h>
#include <stddef.h>
#include <poll.h>
#include <signal.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>
#define CHECK(T, S, A) _Static_assert(sizeof(T) == S && _Alignof(T) == A, #T)
#define FIELD(T,F,O) _Static_assert(offsetof(T,F) == O, #T "." #F)
CHECK(struct pollfd, 8, 4);
FIELD(struct pollfd, fd, 0);
FIELD(struct pollfd, events, 4);
FIELD(struct pollfd, revents, 6);
CHECK(struct timespec, 16, 8);
FIELD(struct timespec, tv_nsec, 8);
CHECK(struct rlimit, 16, 8);
FIELD(struct rlimit, rlim_max, 8);
CHECK(siginfo_t, 104, 8);
FIELD(siginfo_t, si_pid, 12);
FIELD(siginfo_t, si_status, 20);
_Static_assert(sizeof(nfds_t) == 4, "poll count");

CHECK(sigset_t, 4, 4);
CHECK(struct sigaction, 16, 8);
FIELD(struct sigaction, sa_handler, 0);
FIELD(struct sigaction, sa_mask, 8);
FIELD(struct sigaction, sa_flags, 12);
CHECK(struct timeval, 16, 8);
FIELD(struct timeval, tv_usec, 8);
CHECK(struct itimerval, 32, 8);
FIELD(struct itimerval, it_value, 16);
_Static_assert(SIGALRM == 14 && SIG_UNBLOCK == 2 && ITIMER_REAL == 0,
               "deadline signal and timer constants");

#define TYPE(S, T) _Static_assert(__builtin_types_compatible_p(__typeof__(S), T), #S)
TYPE(&mmap, void *(*)(void *, size_t, int, int, int, off_t));
TYPE(&mprotect, int (*)(void *, size_t, int));
TYPE(&munmap, int (*)(void *, size_t));
_Static_assert((MAP_PRIVATE | MAP_ANONYMOUS) == 0x1002, "task mapping flags");
_Static_assert(PROT_NONE == 0 && (PROT_READ | PROT_WRITE) == 3, "task mapping protections");
TYPE(&malloc, void *(*)(size_t));
TYPE(&free, void (*)(void *));
_Static_assert(sizeof(off_t) == sizeof(int64_t) && (off_t)-1 < 0, "mmap offset");
TYPE(vm_page_size, uintptr_t);
TYPE(&poll, int (*)(struct pollfd *, nfds_t, int));
TYPE(&waitid, int (*)(idtype_t, id_t, siginfo_t *, int));
TYPE(&sigaction, int (*)(int, const struct sigaction *, struct sigaction *));
TYPE(&sigemptyset, int (*)(sigset_t *));
TYPE(&sigaddset, int (*)(sigset_t *, int));
TYPE(&sigprocmask, int (*)(int, const sigset_t *, sigset_t *));
TYPE(&setitimer, int (*)(int, const struct itimerval *, struct itimerval *));
TYPE(&clock_gettime_nsec_np, uint64_t (*)(clockid_t));
_Static_assert(sizeof(((struct timeval *)0)->tv_usec) == sizeof(int32_t),
               "timeval microseconds width");
TYPE(&clock_gettime, int (*)(clockid_t, struct timespec *));
_Static_assert(CLOCK_REALTIME == 0, "runtime realtime clock");
TYPE(&arc4random_buf, void (*)(void *, size_t));
TYPE(&nanosleep, int (*)(const struct timespec *, struct timespec *));
_Static_assert(P_PID == 1, "waitid PID selector");
_Static_assert(WNOHANG == 1 && WEXITED == 4 && WNOWAIT == 0x20,
               "waitid flags");
_Static_assert(EINTR == 4, "waitid interrupted error");
_Static_assert(sizeof(idtype_t) == sizeof(int32_t), "waitid kind width");
_Static_assert(sizeof(id_t) == sizeof(uint32_t), "waitid id width");
_Static_assert(sizeof(clockid_t) == sizeof(int32_t), "clock id width");

CHECK(struct proc_fdinfo, 8, 4);
FIELD(struct proc_fdinfo, proc_fd, 0);
FIELD(struct proc_fdinfo, proc_fdtype, 4);
TYPE(&proc_pidinfo, int (*)(int, int, uint64_t, void *, int));
_Static_assert(PROC_PIDLISTFDS == 1, "descriptor list flavor");

TYPE(&readdir, struct dirent *(*)(DIR *));
TYPE(&fdopendir, DIR *(*)(int));
TYPE(&closedir, int (*)(DIR *));
FIELD(struct dirent, d_type, 20);
FIELD(struct dirent, d_name, 21);

CHECK(struct sockaddr_storage, 128, 8);
_Static_assert(sizeof(socklen_t) == 4, "socket address length");
CHECK(struct addrinfo, 48, 8);
FIELD(struct addrinfo, ai_flags, 0);
FIELD(struct addrinfo, ai_family, 4);
FIELD(struct addrinfo, ai_socktype, 8);
FIELD(struct addrinfo, ai_protocol, 12);
FIELD(struct addrinfo, ai_addrlen, 16);
FIELD(struct addrinfo, ai_addr, 32);
FIELD(struct addrinfo, ai_canonname, 24);
FIELD(struct addrinfo, ai_next, 40);
TYPE(&getaddrinfo, int (*)(const char *, const char *, const struct addrinfo *, struct addrinfo **));
TYPE(&freeaddrinfo, void (*)(struct addrinfo *));

_Static_assert(MAP_PRIVATE == 2 && MAP_ANON == 0x1000, "task mapping flags");
_Static_assert(PROT_NONE == 0 && (PROT_READ | PROT_WRITE) == 3, "task protections");

TYPE(&fcntl, int (*)(int, int, ...));
TYPE(&close, int (*)(int));
TYPE(&dup2, int (*)(int, int));
TYPE(&strdup, char *(*)(const char *));
_Static_assert(F_GETFD == 1 && F_SETFD == 2 && FD_CLOEXEC == 1 &&
               F_DUPFD_CLOEXEC == 67, "descriptor flag commands");

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
_Static_assert(SOL_SOCKET == 0xffff && SO_RCVTIMEO == 0x1006 && SO_SNDTIMEO == 0x1005 && SO_ERROR == 0x1007,
               "socket timeout and error options");

CHECK(struct sockaddr_in, 16, 4);
CHECK(struct sockaddr_in6, 28, 4);
FIELD(struct sockaddr_in, sin_port, 2);
FIELD(struct sockaddr_in6, sin6_port, 2);

TYPE(&CC_SHA256, unsigned char *(*)(const void *, CC_LONG, unsigned char *));
TYPE(&CC_SHA384, unsigned char *(*)(const void *, CC_LONG, unsigned char *));
TYPE(&CC_SHA512, unsigned char *(*)(const void *, CC_LONG, unsigned char *));
_Static_assert(sizeof(CC_LONG) == 4, "digest input length");
_Static_assert(CC_SHA256_DIGEST_LENGTH == 32 && CC_SHA384_DIGEST_LENGTH == 48
               && CC_SHA512_DIGEST_LENGTH == 64, "digest sizes");
