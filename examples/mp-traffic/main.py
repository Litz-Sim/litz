# Pasted into the stock MicroPython REPL by `litz test` / `litz run`.
import machine
import time

print("litz: traffic")

red = machine.Pin(4, machine.Pin.OUT)
amber = machine.Pin(5, machine.Pin.OUT)
green = machine.Pin(6, machine.Pin.OUT)
lamps = (("red", red), ("amber", amber), ("green", green))


def phase(name, pin):
    for _, lamp in lamps:
        lamp.value(0)
    pin.value(1)
    print("litz:", name)
    time.sleep_ms(200)


for _ in range(2):
    phase("red", red)
    phase("amber", amber)
    phase("green", green)

for _, lamp in lamps:
    lamp.value(0)
print("litz: done")