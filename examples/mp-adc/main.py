# Pasted into the stock MicroPython REPL by `litz test` / `litz run`.
import machine
import time

print("litz: adc")

led = machine.Pin(8, machine.Pin.OUT)
adc = machine.ADC(machine.Pin(0))
adc.atten(machine.ADC.ATTN_11DB)

time.sleep_ms(50)
raw = adc.read()
print("litz: adc raw", raw)

# The wiper is at half travel, which is well inside the middle band under any
# of the C3's attenuations. The edges are loose so the example tests the
# conversion, not the calibration curve.
if raw > 3600:
    print("litz: high")
elif raw < 400:
    print("litz: low")
else:
    print("litz: mid")

led.value(1 if raw > 400 else 0)
time.sleep_ms(100)
print("litz: done")