# Pasted into the stock MicroPython REPL by `litz test` / `litz run`.
#
# Raw paste has no filesystem image, so the driver travels in this one file
# rather than in an importable ssd1306.py.
import machine
import time

ADDR = 0x3C

# A minimal SSD1306 controller: power on, 128x64, internal charge pump.
INIT = bytes([
    0xAE,        # display off
    0xD5, 0x80,  # clock divide
    0xA8, 0x3F,  # multiplex 64
    0xD3, 0x00,  # display offset 0
    0x40,        # start line 0
    0x8D, 0x14,  # charge pump on
    0x20, 0x00,  # horizontal addressing
    0xA1,        # segment remap
    0xC8,        # COM scan descending
    0xDA, 0x12,  # COM pins
    0x81, 0xCF,  # contrast
    0xD9, 0xF1,  # precharge
    0xDB, 0x40,  # VCOMH
    0xA4,        # display follows RAM
    0xA6,        # not inverted
    0x2E,        # scrolling off
    0xAF,        # display on
])


class SSD1306:
    def __init__(self, i2c):
        self.i2c = i2c
        # Control byte 0x00 starts a command stream.
        self.i2c.writeto(ADDR, bytes([0x00]) + INIT)

    def pixels(self, column, page, data):
        self.i2c.writeto(ADDR, bytes([0x00, 0xB0 | page, column & 0x0F, 0x10 | (column >> 4)]))
        # Control byte 0x40 starts a data stream.
        self.i2c.writeto(ADDR, bytes([0x40]) + data)


print("litz: oled")

i2c = machine.I2C(0, scl=machine.Pin(5), sda=machine.Pin(4), freq=400_000)
found = i2c.scan()
print("litz: scan", found)
if ADDR not in found:
    print("litz: missing")
else:
    print("litz: found 0x3c")
    display = SSD1306(i2c)
    # A small filled square and a hollow one, eight columns of page 0.
    display.pixels(0, 0, bytes([0x3C, 0x42, 0xA9, 0xA9, 0x42, 0x3C, 0x00, 0x00]))
    time.sleep_ms(50)
    print("litz: oled ok")

print("litz: done")