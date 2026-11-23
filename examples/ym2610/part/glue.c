// The `litz:part` glue over the Verilator model. Everything here is the
// contract: pins, time and the two audio samples. The chip logic is model.cpp.
#include "component.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

extern void ym_boot(void);
extern void ym_write(uint8_t addr, uint8_t data);
extern uint8_t ym_read(void);
extern void ym_step_to(uint64_t target);
extern uint64_t ym_cycles(void);
extern int ym_irq(void);
extern int16_t ym_left(void);
extern int16_t ym_right(void);

enum {
    PIN_A0 = 0,
    PIN_A1 = 1,
    PIN_AUDIO_L = 2,
    PIN_AUDIO_R = 3,
    PIN_CS = 4,
    PIN_D0 = 5,
    PIN_D7 = 12,
    PIN_GND = 13,
    PIN_IRQ = 14,
    PIN_VCC = 15,
    PIN_WR = 16,
};

static const char *const PINS[] = {
    "A0", "A1", "AUDIO_L", "AUDIO_R", "CS", "D0", "D1", "D2",
    "D3", "D4", "D5", "D6", "D7", "GND", "IRQ", "VCC", "WR",
};

static uint8_t bus;
static uint8_t a0;
static uint8_t a1;
static uint8_t cs_high;
static uint8_t wr_high = 1;
static uint8_t audio_l_high;
static uint8_t audio_r_high;
static uint8_t irq_low;
static int booted;

// One audio frame per 144 clocks at 8 MHz. The level is published every 8
// frames (144 us): often enough for a buzzer and a scope, rarely enough that a
// single `advance` stays inside the host's fuel budget -- the Verilator model
// costs a few thousand instructions per clock, so 24 frames in one call would
// run out.
#define SAMPLE_NS 18000ull
#define PUBLISH_NS (SAMPLE_NS * 8)

static uint64_t next_at;
static int32_t bias;

static component_string_t text(const char *s) {
    component_string_t ret;
    size_t len = strlen(s);
    ret.ptr = malloc(len);
    memcpy(ret.ptr, s, len);
    ret.len = len;
    return ret;
}

static void ensure_boot(void) {
    if (!booted) {
        ym_boot();
        booted = 1;
        next_at = PUBLISH_NS;
    }
}

// The model's mix has a large DC component: the SSG is summed unsigned. A real
// YM2610 feeds a coupling capacitor, so the pin carries the signal around its
// own average; a slow average here plays the same part. With it the level
// follows the waveform's zero crossings instead of a fixed offset.
static void refresh_audio(void) {
    int32_t left = ym_left();
    int32_t right = ym_right();
    const int32_t dc = left + right;
    bias += (dc / 2 - bias) / 16;
    audio_l_high = left > bias;
    audio_r_high = right > bias;
    irq_low = ym_irq() == 0;
}

void exports_litz_part_part_info(exports_litz_part_part_part_info_t *ret) {
    size_t count = sizeof(PINS) / sizeof(PINS[0]);
    ret->name = text("ym2610");
    ret->description = text("Four-operator FM, SSG and ADPCM sound generator");
    ret->pins.ptr = malloc(count * sizeof(component_string_t));
    ret->pins.len = count;
    for (size_t i = 0; i < count; i++) {
        ret->pins.ptr[i] = text(PINS[i]);
    }
}

void exports_litz_part_part_set_pin(uint16_t pin, exports_litz_part_part_level_t level, uint64_t at_ns) {
    (void)at_ns;
    int high = level == LITZ_PART_TYPES_LEVEL_HIGH;
    ensure_boot();
    switch (pin) {
    case PIN_A0:
        a0 = high;
        break;
    case PIN_A1:
        a1 = high;
        break;
    case PIN_CS:
        cs_high = high;
        break;
    case PIN_WR:
        if (high && !wr_high && !cs_high) {
            ym_write((uint8_t)((a1 << 1) | a0), bus);
            refresh_audio();
        }
        wr_high = high;
        break;
    default:
        if (pin >= PIN_D0 && pin <= PIN_D7) {
            uint8_t bit = (uint8_t)(1u << (pin - PIN_D0));
            if (high) {
                bus |= bit;
            } else {
                bus &= (uint8_t)~bit;
            }
        }
        break;
    }
}

void exports_litz_part_part_advance(uint64_t to_ns) {
    ensure_boot();
    ym_step_to(to_ns * 8 / 1000);
    refresh_audio();
    next_at = (to_ns / PUBLISH_NS + 1) * PUBLISH_NS;
}

bool exports_litz_part_part_next_event(uint64_t *ret) {
    ensure_boot();
    *ret = next_at;
    return true;
}

void exports_litz_part_part_drive(uint16_t pin, exports_litz_part_part_drive_result_t *ret) {
    ensure_boot();
    ret->strength = LITZ_PART_TYPES_STRENGTH_STRONG;
    ret->level = LITZ_PART_TYPES_LEVEL_LOW;
    switch (pin) {
    case PIN_AUDIO_L:
        ret->level = audio_l_high ? LITZ_PART_TYPES_LEVEL_HIGH : LITZ_PART_TYPES_LEVEL_LOW;
        break;
    case PIN_AUDIO_R:
        ret->level = audio_r_high ? LITZ_PART_TYPES_LEVEL_HIGH : LITZ_PART_TYPES_LEVEL_LOW;
        break;
    case PIN_IRQ:
        ret->level = irq_low ? LITZ_PART_TYPES_LEVEL_LOW : LITZ_PART_TYPES_LEVEL_HIGH;
        break;
    default:
        ret->level = LITZ_PART_TYPES_LEVEL_FLOATING;
        ret->strength = LITZ_PART_TYPES_STRENGTH_WEAK;
        break;
    }
}

void exports_litz_part_part_state(exports_litz_part_part_part_state_t *ret) {
    ret->tag = LITZ_PART_TYPES_PART_STATE_NONE;
}

bool exports_litz_part_part_press(bool pressed) {
    (void)pressed;
    return false;
}

bool exports_litz_part_part_set_control(uint16_t permille) {
    (void)permille;
    return false;
}

bool exports_litz_part_part_analog(uint16_t pin, float *ret) {
    ensure_boot();
    if (pin != PIN_AUDIO_L && pin != PIN_AUDIO_R) {
        return false;
    }
    int32_t sample = pin == PIN_AUDIO_L ? ym_left() : ym_right();
    *ret = (float)(sample - bias) / 32768.0f * 1.65f + 1.65f;
    return true;
}

void exports_litz_part_part_attributes(exports_litz_part_part_list_attribute_def_t *ret) {
    ret->ptr = NULL;
    ret->len = 0;
}

void exports_litz_part_part_set_attribute(component_string_t *name, exports_litz_part_part_attribute_value_t *value) {
    (void)name;
    (void)value;
}

void exports_litz_part_part_i2c_connect(uint16_t addr, bool read) {
    (void)addr;
    (void)read;
}
uint8_t exports_litz_part_part_i2c_read(void) {
    return 0;
}
bool exports_litz_part_part_i2c_write(uint8_t byte) {
    (void)byte;
    return false;
}
void exports_litz_part_part_i2c_stop(void) {}

void exports_litz_part_part_spi_select(bool selected) {
    (void)selected;
}
void exports_litz_part_part_spi_transfer(component_list_u8_t *tx, uint32_t rx_len, component_list_u8_t *ret) {
    (void)tx;
    ret->ptr = malloc(rx_len);
    ret->len = rx_len;
    memset(ret->ptr, 0xff, rx_len);
}
void exports_litz_part_part_uart_receive(component_list_u8_t *bytes, uint64_t at_ns) {
    (void)bytes;
    (void)at_ns;
}
void exports_litz_part_part_uart_poll(component_list_u8_t *ret) {
    ret->ptr = NULL;
    ret->len = 0;
}
bool exports_litz_part_part_framebuffer(exports_litz_part_part_framebuffer_info_t *ret) {
    (void)ret;
    return false;
}
void exports_litz_part_part_draw(uint32_t offset, component_list_u8_t *pixels) {
    (void)offset;
    (void)pixels;
}
bool exports_litz_part_part_damage(exports_litz_part_part_damage_region_t *ret) {
    (void)ret;
    return false;
}
