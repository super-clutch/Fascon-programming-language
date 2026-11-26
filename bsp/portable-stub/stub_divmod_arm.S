/* stub_divmod_arm.S — ARM EABI の 64 ビット割り算の入口（__aeabi_uldivmod / __aeabi_ldivmod）を C の __udivmoddi4 /
 * __divmoddi4 へ包む（商は r0:r1、余りは r2:r3）。ARMv7 の移植用 / the ARM EABI entries for 64-bit division wrapped
 * around the C __udivmoddi4 / __divmoddi4 (quotient in r0:r1, remainder in r2:r3); for ARMv7 ports */
    .syntax unified
    .text
    .global __aeabi_uldivmod
    .type   __aeabi_uldivmod, %function
__aeabi_uldivmod:
    push    {r4, lr}
    sub     sp, sp, #16
    add     r4, sp, #8
    str     r4, [sp]
    bl      __udivmoddi4
    ldr     r2, [sp, #8]
    ldr     r3, [sp, #12]
    add     sp, sp, #16
    pop     {r4, pc}
    .global __aeabi_ldivmod
    .type   __aeabi_ldivmod, %function
__aeabi_ldivmod:
    push    {r4, lr}
    sub     sp, sp, #16
    add     r4, sp, #8
    str     r4, [sp]
    bl      __divmoddi4
    ldr     r2, [sp, #8]
    ldr     r3, [sp, #12]
    add     sp, sp, #16
    pop     {r4, pc}
