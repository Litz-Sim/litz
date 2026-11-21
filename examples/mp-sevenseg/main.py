# Pasted into the stock MicroPython REPL by `litz test` / `litz run`.
import machine
import time

print("litz: sevenseg")

# a..g on GPIO0..GPIO6, common on GND.
SEGMENTS = [machine.Pin(n, machine.Pin.OUT) for n in range(7)]

# Digit patterns, bit 0 is segment a and bit 6 is segment g.
ONE = 0x06    # b, c
SEVEN = 0x07  # a, b, c


def show(mask):
    for bit, pin in enumerate(SEGMENTS):
        pin.value((mask >> bit) & 1)


show(ONE)
print("litz: one")
time.sleep_ms(300)

show(SEVEN)
print("litz: seven")
time.sleep_ms(300)

print("litz: done")