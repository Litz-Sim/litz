# Pasted into the stock MicroPython REPL by `litz test` / `litz run`.
import machine
import time

print("litz: button")

led = machine.Pin(8, machine.Pin.OUT)
btn = machine.Pin(0, machine.Pin.IN, machine.Pin.PULL_UP)

start = time.ticks_ms()
announced = False
while time.ticks_diff(time.ticks_ms(), start) < 500:
    held = btn.value() == 0
    led.value(1 if held else 0)
    if held and not announced:
        announced = True
        print("litz: pressed")
    time.sleep_ms(10)

led.value(0)
print("litz: done")