# Pasted into the stock MicroPython REPL by `litz test` / `litz run`.
import machine

ADDR = 0x76  # SDO tied low


def main():
    print("litz: bmp280")

    i2c = machine.I2C(0, scl=machine.Pin(5), sda=machine.Pin(4), freq=400_000)
    print("litz: scan", i2c.scan())

    # 0xD0 is the read-only chip-id register.
    chip = i2c.readfrom_mem(ADDR, 0xD0, 1)[0]
    print("litz: chip 0x%02x" % chip)

    if chip == 0x58:
        # One burst covers the calibration block a driver reads at start-up.
        calib = i2c.readfrom_mem(ADDR, 0x88, 8)
        print("litz: calib", list(calib))
        print("litz: bmp280 ok")
    else:
        print("litz: wrong chip")

    print("litz: done")


main()