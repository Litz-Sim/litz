/* A bare-metal RP2040 program that drives an SN76489 over its write bus:
 * GPIO 2..9 carry D0..D7, GPIO 10 strobes /WE, and /CE is tied low. The
 * program silences the chip, plays one note per voice, then a noise burst,
 * and prints what it wrote so a test can read the sequence back.
 *
 * There is no SDK here: the image is freestanding and talks to the SIO and
 * UART0 registers directly, the same way examples/pico-c-systick does.
 */
#define REG(a) (*(volatile unsigned int *)(a))
#define UART0 0x40034000u
#define SIO_OUT 0xd0000010u
#define SIO_OUT_SET 0xd0000014u
#define SIO_OUT_CLR 0xd0000018u
#define SIO_OE_SET 0xd0000024u

#define DATA_SHIFT 2
#define DATA_MASK (0xffu << DATA_SHIFT)
#define WE (1u << 10)

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

static void settle(void) {
    for (volatile int i = 0; i < 8; i++) {
    }
}

/* Put a byte on D0..D7 and pulse /WE. The chip latches it on the rising edge. */
static void write_byte(unsigned byte) {
    unsigned out = REG(SIO_OUT);
    out = (out & ~DATA_MASK) | ((byte << DATA_SHIFT) & DATA_MASK);
    REG(SIO_OUT) = out;
    settle();
    REG(SIO_OUT_SET) = WE;
    settle();
    REG(SIO_OUT_CLR) = WE;
    settle();
}

/* A latch byte plus its data byte, the chip's two-write tone update. */
static void tone(unsigned channel, unsigned divider) {
    write_byte(0x80u | (channel << 5) | (divider & 0x0fu));
    write_byte((divider >> 4) & 0x3fu);
}

static void volume(unsigned channel, unsigned attenuation) {
    write_byte(0x80u | (channel << 5) | 0x10u | (attenuation & 0x0fu));
}

static void noise(unsigned control) {
    write_byte(0xe0u | (control & 0x07u));
}

static unsigned hz(unsigned divider) {
    return 3579545u / (32u * divider);
}

volatile unsigned ticks;

void systick_handler(void) {
    ticks++;
}

static void wait_ms(unsigned ms) {
    unsigned until = ticks + ms;
    while (ticks < until) {
        __asm volatile("wfi");
    }
}

void reset(void) {
    REG(SIO_OE_SET) = DATA_MASK | WE;
    uart_init();

    /* Until a volume is written each channel is at 15, which is silence. */
    for (unsigned channel = 0; channel < 4; channel++) {
        volume(channel, 15);
    }

    /* A 1 ms timebase, as in examples/pico-c-systick: the note lengths below
     * are SysTick counts, so the handler has to be running before the first
     * note ends. */
    REG(0xe000e014) = 125000 - 1;
    REG(0xe000e018) = 0;
    REG(0xe000e010) = 7;

    puts_("litz: sn76489\n");

    unsigned notes[4] = { 254, 214, 170, 127 };
    for (unsigned index = 0; index < 4; index++) {
        tone(0, notes[index]);
        volume(0, 0);
        puts_("N=");
        putu(notes[index]);
        puts_(" f=");
        putu(hz(notes[index]));
        putc_('\n');
        wait_ms(40);
    }

    /* Silence the tone, then use the noise channel for a burst. */
    volume(0, 15);
    noise(0x04); /* white noise, fastest shift rate */
    volume(3, 2);
    puts_("noise\n");
    wait_ms(40);
    volume(3, 15);

    puts_("done\n");
    for (;;) {
        __asm volatile("wfi");
    }
}
