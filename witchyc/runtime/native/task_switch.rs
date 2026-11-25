// Cooperative stack switching for the native ARM64 task runtime.
// The caller provides two 176-byte, 16-byte-aligned context records.
// A fresh context's stack, saved link register, and x19 are initialized
// to its stack top, bootstrap address and task pointer respectively.
core::arch::global_asm!(
    r#"
.text
.p2align 2
.globl {switch}
{switch}:
stp x19, x20, [x0, #0]
stp x21, x22, [x0, #16]
stp x23, x24, [x0, #32]
stp x25, x26, [x0, #48]
stp x27, x28, [x0, #64]
stp x29, x30, [x0, #80]
stp d8, d9, [x0, #96]
stp d10, d11, [x0, #112]
stp d12, d13, [x0, #128]
stp d14, d15, [x0, #144]
mov x2, sp
str x2, [x0, #160]
ldp x19, x20, [x1, #0]
ldp x21, x22, [x1, #16]
ldp x23, x24, [x1, #32]
ldp x25, x26, [x1, #48]
ldp x27, x28, [x1, #64]
ldp x29, x30, [x1, #80]
ldp d8, d9, [x1, #96]
ldp d10, d11, [x1, #112]
ldp d12, d13, [x1, #128]
ldp d14, d15, [x1, #144]
ldr x2, [x1, #160]
mov sp, x2
ret
.p2align 2
.globl {bootstrap}
{bootstrap}:
mov x0, x19
bl {enter}
brk #0
"#,
    switch = sym super::witchy_task_switch,
    bootstrap = sym super::witchy_task_bootstrap,
    enter = sym super::witchy_task_enter,
);
