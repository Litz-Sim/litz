# Pasted into the stock MicroPython REPL by `litz test` / `litz run`.
import machine
import time

print("litz: blink")

led = machine.Pin(8, machine.Pin.OUT)
for _ in range(3):
    led.value(1)
    time.sleep_ms(150)
    led.value(0)
    time.sleep_ms(150)

print("litz: done")