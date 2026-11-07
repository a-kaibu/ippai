/* jit32test's native runner: a freestanding 32-bit Linux program (gcc -m32 -nostdlib -static)
 * that runs one test program on this machine's CPU. stdin: registers (8 x u32, EAX..EDI),
 * EFLAGS, the code's length, an FXSAVE image (512 bytes), the code (at 0x100000), DATA (0x1000
 * bytes at 0x200000). The program ends by jumping to 0x180000; stdout then: registers, EFLAGS,
 * the FXSAVE image, DATA, the stack page below 0x300000. A fault kills the process (the test
 * harness sees no output). Guest memory [0x100000, 0x400000) is mapped RWX and zeroed. */
        .text
        .globl _start
_start:
        /* mmap2(0x100000, 0x300000, RWX, MAP_PRIVATE|MAP_ANONYMOUS|MAP_FIXED, -1, 0) */
        mov $192, %eax
        mov $0x100000, %ebx
        mov $0x300000, %ecx
        mov $7, %edx
        mov $0x32, %esi
        mov $-1, %edi
        xor %ebp, %ebp
        int $0x80
        cmp $0x100000, %eax
        jne fail
        mov $hdr, %ecx
        mov $40, %edx
        call readn
        mov $fx_in, %ecx
        mov $512, %edx
        call readn
        mov $0x100000, %ecx
        mov hdr+36, %edx
        call readn
        mov $0x200000, %ecx
        mov $0x1000, %edx
        call readn
        /* the end: jmp *[end_ptr] */
        movb $0xff, 0x180000
        movb $0x25, 0x180001
        movl $end_ptr, 0x180002
        fxrstor fx_in
        pushl hdr+32
        popfl
        mov hdr+4, %ecx
        mov hdr+8, %edx
        mov hdr+12, %ebx
        mov hdr+20, %ebp
        mov hdr+24, %esi
        mov hdr+28, %edi
        mov hdr+0, %eax
        mov hdr+16, %esp
        jmp *code_ptr

end_stub:
        mov %esp, out+16
        mov $stack_top, %esp
        mov %eax, out+0
        mov %ecx, out+4
        mov %edx, out+8
        mov %ebx, out+12
        mov %ebp, out+20
        mov %esi, out+24
        mov %edi, out+28
        pushfl
        popl out+32
        fxsave fx_out
        cld
        mov $out, %ecx
        mov $36, %edx
        call writen
        mov $fx_out, %ecx
        mov $512, %edx
        call writen
        mov $0x200000, %ecx
        mov $0x1000, %edx
        call writen
        mov $0x2ff000, %ecx
        mov $0x1000, %edx
        call writen
        mov $1, %eax
        xor %ebx, %ebx
        int $0x80

fail:
        mov $1, %eax
        mov $3, %ebx
        int $0x80

/* read(0) exactly %edx bytes to %ecx */
readn:
        test %edx, %edx
        jz 2f
1:      mov $3, %eax
        xor %ebx, %ebx
        int $0x80
        test %eax, %eax
        jle fail
        add %eax, %ecx
        sub %eax, %edx
        jnz 1b
2:      ret

/* write(1) %edx bytes from %ecx */
writen:
1:      mov $4, %eax
        mov $1, %ebx
        int $0x80
        test %eax, %eax
        jle fail
        add %eax, %ecx
        sub %eax, %edx
        jnz 1b
        ret

        .data
        .align 4
code_ptr: .long 0x100000
end_ptr: .long end_stub

        .bss
        .align 16
fx_in:  .space 512
fx_out: .space 512
hdr:    .space 40
out:    .space 36
        .align 16
        .space 4096
stack_top:
