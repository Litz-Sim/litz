# MicroPython on the ESP32-S3 DevKitC-1

Stock MicroPython firmware from micropython.org runs on the board and `main.py` is pasted into the REPL, the same way the C3 example works. Fetch the image first:

```sh
tools/fetch-micropython.sh
cargo run -p litz-cli -- test examples/micropython-s3 --update-lock
```
