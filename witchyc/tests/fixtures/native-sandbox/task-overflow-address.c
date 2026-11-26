#include <signal.h>
#include <stdint.h>
#include <unistd.h>

#ifdef __linux__
#define OVERFLOW_SIGNAL SIGSEGV
#else
#define OVERFLOW_SIGNAL SIGBUS
#endif
static unsigned char alternate_stack[65536] __attribute__((aligned(16)));
static uintptr_t lower_guard;
static size_t guard_size;

static void fault(int signum, siginfo_t *info, void *context) {
    (void)context;
    uintptr_t address = (uintptr_t)info->si_addr;
    _exit(signum == OVERFLOW_SIGNAL && address - lower_guard < guard_size ? 42 : 43);
}

int install_overflow_handler(uintptr_t base, size_t size) {
    lower_guard = base;
    guard_size = size;
    stack_t stack = {.ss_sp = alternate_stack, .ss_size = sizeof alternate_stack};
    struct sigaction action = {.sa_sigaction = fault, .sa_flags = SA_ONSTACK | SA_SIGINFO};
    return sigemptyset(&action.sa_mask) || sigaltstack(&stack, 0) || sigaction(OVERFLOW_SIGNAL, &action, 0);
}
