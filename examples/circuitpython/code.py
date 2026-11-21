# Pasted into the CircuitPython REPL by `litz test` / `litz run`; no
# filesystem image is involved. GPIO4 is the pin wired to the LED in
# diagram.json; the ESP32-S3 port names that pin IO4, and `board`/`digitalio`
# are CircuitPython's pin API.
import time

import board
import digitalio

print("litz: circuitpython")

led = digitalio.DigitalInOut(board.IO4)
led.direction = digitalio.Direction.OUTPUT
for _ in range(3):
    led.value = True
    print("+")
    time.sleep(0.1)
    led.value = False
    print("-")
    time.sleep(0.1)

print("litz: done")
