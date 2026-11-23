/* A bare-metal RP2040 program that programs a YM2610 over its write bus.
 *
 * GPIO 2..9 carry D0..D7, GPIO 10 is A0, GPIO 11 is A1, GPIO 12 is /CS and
 * GPIO 13 is /WR. The program keys on two FM voices and one SSG voice, lets
 * them ring, then releases everything and prints what it did. There is no
 * SDK: the image talks to SIO and UART0 directly, as examples/pico-c-systick
 * does.
 */
#define REG(a) (*(volatile unsigned int *)(a))
#define UART0 0x40034000u
#define SIO_OUT 0xd0000010u
#define SIO_OUT_SET 0xd0000014u
#define SIO_OUT_CLR 0xd0000018u
#define SIO_OE_SET 0xd0000024u

#define DATA_SHIFT 2
#define DATA_MASK (0xffu << DATA_SHIFT)
#define A0 (1u << 10)
#define A1 (1u << 11)
#define CS (1u << 12)
#define WR (1u << 13)

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

/* One bus write: address on A0/A1, data on D0..D7, /CS low, /WR pulses low.
 * The chip latches the byte on the rising edge of /WR. */
static void wr(unsigned address, unsigned data) {
    unsigned out = REG(SIO_OUT);
    out &= ~(DATA_MASK | A0 | A1);
    out |= ((data << DATA_SHIFT) & DATA_MASK) |
           ((address & 1u) ? A0 : 0u) |
           ((address & 2u) ? A1 : 0u);
    REG(SIO_OUT) = out;
    REG(SIO_OUT_CLR) = CS;
    settle();
    REG(SIO_OUT_CLR) = WR;
    settle();
    REG(SIO_OUT_SET) = WR;
    settle();
    REG(SIO_OUT_SET) = CS;
    settle();
}

/* Port 0 register write: address byte, then data byte. */
static void reg(unsigned address, unsigned data) {
    wr(0, address);
    wr(1, data);
}

static void fm_voice(unsigned channel, unsigned fnum, unsigned block) {
    static const unsigned operators[4] = { 0x00, 0x04, 0x08, 0x0c };
    for (unsigned op = 0; op < 4; op++) {
        unsigned base = 0x30 + operators[op] + channel;
        reg(base, 0x01);      /* DT 0, MULT 1 */
        reg(base + 0x10, 0x20); /* TL 32 */
        reg(base + 0x20, 0x1f); /* attack rate 31 */
        reg(base + 0x30, 0x00); /* decay rate 0 */
        reg(base + 0x40, 0x00); /* sustain rate 0 */
        reg(base + 0x50, 0x0f); /* release rate 15 */
        reg(base + 0x60, 0x00); /* SSG-EG off */
    }
    reg(0xa0 + channel, fnum & 0xff);
    reg(0xa4 + channel, ((block & 7u) << 3) | ((fnum >> 8) & 7u));
    reg(0xb0 + channel, 0x07); /* feedback 0, algorithm 7 */
    reg(0xb4 + channel, 0xc0); /* left and right on */
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
    REG(SIO_OE_SET) = DATA_MASK | A0 | A1 | CS | WR;
    REG(SIO_OUT_SET) = CS;
    uart_init();

    REG(0xe000e014) = 125000 - 1;
    REG(0xe000e018) = 0;
    REG(0xe000e010) = 7;

    puts_("litz: ym2610\n");

    reg(0x22, 0x00); /* LFO off */
    reg(0x27, 0x00); /* timers off */
    reg(0x28, 0x00); /* all voices off */

    fm_voice(0, 0x269, 2); /* A4 */
    fm_voice(1, 0xc25, 2); /* E5 */

    /* SSG channel A: 8 MHz / (64 * 428) is about 292 Hz. */
    reg(0x00, 0xac);
    reg(0x01, 0x01);
    reg(0x07, 0x3e); /* tone A on, others off */
    reg(0x08, 0x0f); /* SSG A at full volume */

    reg(0x28, 0x1f); /* key on FM channel 1 */
    reg(0x28, 0x2f); /* key on FM channel 2 */
    puts_("A4 + E5 + SSG 292 Hz\n");

    wait_ms(200);

    reg(0x28, 0x00); /* release the FM voices */
    reg(0x08, 0x00); /* SSG A silent */
    puts_("done\n");
    for (;;) {
        __asm volatile("wfi");
    }
}
