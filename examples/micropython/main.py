# Pasted into the stock MicroPython REPL by `litz test` / `litz run`; no
# filesystem image is involved. GPIO8 is the DevKitM-1's on-board RGB LED.
import machine
import time

print("litz: micropython")

led = machine.Pin(8, machine.Pin.OUT)
for _ in range(3):
    led.value(1)
    print("+")
    time.sleep_ms(100)
    led.value(0)
    print("-")
    time.sleep_ms(100)

print("litz: done")