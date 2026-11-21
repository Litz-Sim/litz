# Pasted into the stock MicroPython REPL by `litz test` / `litz run`.
#
# The C3's LEDC peripheral is modelled as configuration here, not as edges on
# a pin, so `machine.PWM` would not reach the servo. A software pulse train
# does: the part only measures the high time between edges.
import machine
import time

print("litz: servo")

signal = machine.Pin(8, machine.Pin.OUT)


def pulse(high_us):
    signal.value(1)
    time.sleep_us(high_us)
    signal.value(0)
    time.sleep_us(20000 - high_us)


# 0.5 ms is 0 degrees, 1.5 ms is 90 and 2.5 ms is 180.
for _ in range(5):
    pulse(1500)

print("litz: 90 degrees")
time.sleep_ms(100)
print("litz: done")