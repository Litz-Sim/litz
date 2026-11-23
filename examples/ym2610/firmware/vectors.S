.syntax unified
.thumb
.section .vectors,"a",%progbits
.word 0x20042000
.word reset + 1
.word 0
.word 0
.word 0
.word 0
.word 0
.word 0
.word 0
.word 0
.word 0
.word 0
.word 0
.word 0
.word 0
.word systick_handler + 1
