/* A real I2C transaction from compiled C: I2C0 reads the LM75 on GP4/GP5 at
 * address 0x48, and the temperature goes out on UART0. The write of the
 * pointer register is one transaction with STOP; the two-byte read is another,
 * which is the register-read handshake every I2C sensor uses.
 *
 * The LED on GP15 lights only after the read produced a plausible value, so
 * the `expect_state` in litz.toml says the bus reached the part rather than
 * that the firmware merely started. */
#define REG(a) (*(volatile unsigned int *)(a))

#define RESETS_BASE 0x4000c000u
#define IO_BANK0_BASE 0x40014000u
#define PADS_BANK0_BASE 0x4001c000u
#define UART0 0x40034000u
#define I2C0 0x40044000u
#define SIO_OUT_SET (0xd0000000u + 0x14)
#define SIO_OUT_CLR (0xd0000000u + 0x18)
#define SIO_OE_SET (0xd0000000u + 0x24)

#define LED (1u << 15)
#define LM75_ADDR 0x48u

/* DW_apb_i2c registers. */
#define IC_CON 0x00
#define IC_TAR 0x04
#define IC_DATA_CMD 0x10
#define IC_SS_SCL_HCNT 0x14
#define IC_SS_SCL_LCNT 0x18
#define IC_FS_SCL_HCNT 0x1c
#define IC_FS_SCL_LCNT 0x20
#define IC_INTR_MASK 0x30
#define IC_CLR_INTR 0x40
#define IC_RX_TL 0x38
#define IC_TX_TL 0x3c
#define IC_ENABLE 0x6c
#define IC_STATUS 0x70
#define IC_RXFLR 0x78

#define IC_CON_MASTER (1u << 0)
#define IC_CON_FAST (2u << 1)
#define IC_CON_RESTART_EN (1u << 5)
#define IC_CON_SLAVE_DISABLE (1u << 6)
#define IC_DATA_CMD_READ (1u << 8)
#define IC_DATA_CMD_STOP (1u << 9)

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

/* GP4 (SDA) and GP5 (SCL) to the I2C function, the mux setting the Pico SDK's
 * gpio_set_function(4/5, GPIO_FUNC_I2C) writes. */
static void i2c_pins(void) {
    REG(IO_BANK0_BASE + 0x04 + 4 * 8) = 3; /* CTRL for GP4: I2C */
    REG(IO_BANK0_BASE + 0x04 + 5 * 8) = 3; /* CTRL for GP5: I2C */
    REG(PADS_BANK0_BASE + 0x04 + 4 * 4) = 0x56; /* IE, pull-up, schmitt */
    REG(PADS_BANK0_BASE + 0x04 + 5 * 4) = 0x56;
}

static void i2c_init(void) {
    /* Release I2C0 from reset (RESETS_RESET_I2C0 is bit 3). The plain RESET
     * register is used rather than the +0x3000 clear alias: this model's
     * address decoder only routes each block's 4 KiB base window. */
    REG(RESETS_BASE + 0x00) = 0;
    while (!(REG(RESETS_BASE + 0x08) & (1u << 3))) {
    }

    REG(I2C0 + IC_ENABLE) = 0;
    REG(I2C0 + IC_CON) =
        IC_CON_MASTER | IC_CON_FAST | IC_CON_RESTART_EN | IC_CON_SLAVE_DISABLE;
    /* Standard-mode counts for 100 kHz, fast-mode for 400 kHz. The model does
     * not clock the bus, but a real part needs them. */
    REG(I2C0 + IC_SS_SCL_HCNT) = 0x3c;
    REG(I2C0 + IC_SS_SCL_LCNT) = 0x4b;
    REG(I2C0 + IC_FS_SCL_HCNT) = 0x0e;
    REG(I2C0 + IC_FS_SCL_LCNT) = 0x16;
    REG(I2C0 + IC_RX_TL) = 0;
    REG(I2C0 + IC_TX_TL) = 0;
    REG(I2C0 + IC_INTR_MASK) = 0;
    REG(I2C0 + IC_CLR_INTR) = 0;
    REG(I2C0 + IC_ENABLE) = 1;
}

static void i2c_target(unsigned char address) {
    REG(I2C0 + IC_ENABLE) = 0;
    REG(I2C0 + IC_TAR) = address;
    REG(I2C0 + IC_ENABLE) = 1;
}

/* Write one pointer byte and STOP. */
static void lm75_select(unsigned char reg) {
    i2c_target(LM75_ADDR);
    while (!(REG(I2C0 + IC_STATUS) & (1u << 1))) {
    }
    REG(I2C0 + IC_DATA_CMD) = reg | IC_DATA_CMD_STOP;
}

/* Read `len` bytes from the pointer just written. */
static void lm75_read(unsigned char *buf, int len) {
    int i;
    i2c_target(LM75_ADDR);
    for (i = 0; i < len; i++) {
        unsigned cmd = IC_DATA_CMD_READ;
        if (i == len - 1) {
            cmd |= IC_DATA_CMD_STOP;
        }
        while (!(REG(I2C0 + IC_STATUS) & (1u << 1))) {
        }
        REG(I2C0 + IC_DATA_CMD) = cmd;
    }
    for (i = 0; i < len; i++) {
        while (REG(I2C0 + IC_RXFLR) == 0) {
        }
        buf[i] = (unsigned char)REG(I2C0 + IC_DATA_CMD);
    }
}

void reset(void) {
    unsigned char bytes[2];
    int raw;
    int tenths;

    REG(SIO_OE_SET) = LED;
    uart_init();
    puts_("litz: i2c\n");

    i2c_pins();
    i2c_init();

    /* Pointer register 0 is the temperature, a 9-bit word left-justified in
     * two bytes at 0.5 C per count: 0x1900 is 25.0 C. */
    lm75_select(0x00);
    lm75_read(bytes, 2);

    raw = ((int)bytes[0] << 8) | bytes[1];
    if (raw & 0x8000) {
        raw -= 0x10000;
    }
    tenths = (raw >> 7) * 5;

    puts_("litz: lm75 ");
    if (tenths < 0) {
        putc_('-');
        tenths = -tenths;
    }
    putu((unsigned)(tenths / 10));
    putc_('.');
    putc_((char)('0' + tenths % 10));
    puts_(" C\n");

    /* A real LM75 leaves the low seven bits of the 9-bit temperature word
     * clear; a bus with nothing answering reads back 0xffff. Light the LED
     * only for a word with the part's shape, so `expect_state` says the read
     * reached the sensor. */
    if ((bytes[1] & 0x7f) == 0) {
        REG(SIO_OUT_SET) = LED;
    }
    for (;;) {
    }
}
