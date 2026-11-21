# Pasted into the stock MicroPython REPL by `litz test` / `litz run`.
import machine
import time

print("litz: micropython")

led = machine.Pin(2, machine.Pin.OUT)
for _ in range(3):
    led.value(1)
    print("+")
    time.sleep_ms(100)
    led.value(0)
    print("-")
    time.sleep_ms(100)

print("litz: done")
