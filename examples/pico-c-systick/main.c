/* A SysTick-driven blinker: the core has to deliver an exception, run a C
 * handler and resume the WFI idle loop for the LED test to pass. */
#define REG(a) (*(volatile unsigned int *)(a))
#define UART0 0x40034000u
#define SIO_OUT_SET (0xd0000000u + 0x14)
#define SIO_OUT_CLR (0xd0000000u + 0x18)
#define SIO_OE_SET (0xd0000000u + 0x24)
#define LED (1u << 15)

static void uart_init(void) {
    REG(UART0 + 0x24) = 67;
    REG(UART0 + 0x28) = 52;
    REG(UART0 + 0x2c) = 0x70;
    REG(UART0 + 0x30) = 0x301;
}

static void putc_(char c) {
    while (REG(UART0 + 0x18) & (1u << 5)) {
    }
    REG(UART0 + 0x00) = (unsigned char)c;
}

static void puts_(const char *s) {
    while (*s) {
        putc_(*s++);
    }
}

static void putu(unsigned value) {
    char digits[8];
    int at = 0;
    do {
        digits[at++] = (char)('0' + value % 10);
        value /= 10;
    } while (value);
    while (at) {
        putc_(digits[--at]);
    }
}

volatile unsigned ticks;

void systick_handler(void) {
    ticks++;
    if (ticks == 1) {
        // Light the LED on the first tick and leave it on: the prints below are
        // what show the handler keeps running.
        REG(SIO_OUT_SET) = LED;
    }
    if ((ticks & 15) == 0) {
        puts_("tick ");
        putu(ticks);
        putc_('\n');
    }
}

void reset(void) {
    REG(SIO_OE_SET) = LED;
    uart_init();
    puts_("litz: systick\n");
    REG(0xe000e014) = 125000 - 1; /* LOAD: 1 ms at 125 MHz */
    REG(0xe000e018) = 0;
    REG(0xe000e010) = 7; /* CLKSOURCE | TICKINT | ENABLE */
    for (;;) {
        __asm volatile("wfi");
    }
}
