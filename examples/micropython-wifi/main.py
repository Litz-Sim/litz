# Pasted into the stock MicroPython REPL by `litz test` / `litz run`.
#
# The board joins the access point the `litz-wifi-router` part puts on the air,
# gets a lease, resolves a name through the simulated router and fetches the
# page it serves. No host network is involved, so the transcript is identical
# on a laptop and in CI.
import machine
import network
import socket
import time

print("litz: wifi")

led = machine.Pin(2, machine.Pin.OUT)
wlan = network.WLAN(network.STA_IF)
wlan.active(True)
wlan.connect("litz")

for _ in range(100):
    if wlan.isconnected():
        break
    time.sleep_ms(200)

if not wlan.isconnected():
    print("litz: no link")
else:
    print("litz: ip", wlan.ifconfig()[0])
    # Any name resolves to the router, except `.local`, which the stack sends
    # to mDNS rather than to the DNS server in its lease.
    address = socket.getaddrinfo("example.com", 80)[0][-1]
    conn = socket.socket()
    conn.connect(address)
    conn.send(b"GET / HTTP/1.0\r\nHost: example.com\r\n\r\n")
    page = b""
    while True:
        chunk = conn.recv(128)
        if not chunk:
            break
        page += chunk
    conn.close()
    print("litz: page", page.split(b"\r\n\r\n")[-1].strip().decode())

led.value(1)
print("litz: done")
