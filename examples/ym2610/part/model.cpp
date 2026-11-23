// The Verilator model behind the wasm part: a C ABI over Vjt10.
//
// Nothing here knows about Litz. It exposes the bus, a cycle target and the
// two audio samples, all plain integers, so the WIT glue can be C.
#include "Vjt10.h"

#include <cstdint>

static Vjt10 *top;
static uint64_t cycles;
static int16_t left_sample;
static int16_t right_sample;
static uint8_t data_out;

static void tick() {
    top->clk = 0;
    top->eval();
    top->clk = 1;
    top->eval();
    cycles++;
}

extern "C" void ym_boot() {
    top = new Vjt10;
    top->rst = 1;
    top->cen = 1;
    top->cs_n = 1;
    top->wr_n = 1;
    top->addr = 0;
    top->din = 0;
    top->adpcma_data = 0;
    top->adpcmb_data = 0;
    top->ch_enable = 0x3f;
    for (int i = 0; i < 64; i++) {
        tick();
    }
    top->rst = 0;
    for (int i = 0; i < 64; i++) {
        tick();
    }
    data_out = top->dout;
}

extern "C" void ym_write(uint8_t addr, uint8_t data) {
    // One bus write is /CS and /WR low for four clocks, then high for four:
    // half a microsecond at 8 MHz, and long enough for the chip to latch.
    top->din = data;
    top->addr = addr & 3;
    top->cs_n = 0;
    top->wr_n = 0;
    for (int i = 0; i < 4; i++) {
        tick();
    }
    top->wr_n = 1;
    top->cs_n = 1;
    for (int i = 0; i < 4; i++) {
        tick();
    }
    data_out = top->dout;
}

extern "C" uint8_t ym_read() {
    return data_out;
}

extern "C" void ym_step_to(uint64_t target) {
    while (cycles < target) {
        tick();
        if (cycles % 144 == 0) {
            left_sample = static_cast<int16_t>(top->snd_left);
            right_sample = static_cast<int16_t>(top->snd_right);
        }
    }
}

extern "C" uint64_t ym_cycles() {
    return cycles;
}

extern "C" int ym_irq() {
    return top->irq_n ? 1 : 0;
}

extern "C" int16_t ym_left() {
    return left_sample;
}

extern "C" int16_t ym_right() {
    return right_sample;
}
