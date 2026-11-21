# Pasted into the stock MicroPython REPL by `litz test` / `litz run`.
#
# UART0 is already owned by the REPL, so `machine.UART(0)` refuses to
# re-initialise it. The console stream is the same UART: `sys.stdin` is where
# host bytes arrive and `print` is how they go back.
import sys
import time

print("litz: uart echo")
print("litz: echo ready")

data = sys.stdin.read(5)
print("litz: got", data)

time.sleep_ms(50)
print("litz: echo done")