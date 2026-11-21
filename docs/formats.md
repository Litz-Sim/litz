# Litz file formats

Five files describe a simulation. Three describe hardware, two describe how to run it.

| File | Answers | Written by |
|---|---|---|
| `diagram.json` | what is placed and what is wired to what | editors, hand |
| `part.toml` | what a leaf component is and how it behaves | maintainers |
| `board.toml` (+ onboard schematic) | what a dev board is | maintainers |
| `litz.toml` | which firmware to run and what to assert | project author |
| `litz.lock` | which catalog versions produced a result | the runner |

Circuits are JSON; manifests are TOML because they are read and edited by people, and comments matter.

Two rules override everything else:

1. **Presentation never changes behaviour.** Coordinates, rotation, `hide`, wire colour and wire routing are parsed so a file round-trips, then dropped. Two diagrams that differ only in geometry simulate identically.
2. **Catalog ids never change.** `litz-led` means the same thing forever. Behaviour may change under a version bump; the name may not.

## 1. Circuit — `diagram.json`

```json
{
  "version": 1,
  "author": "Litz",
  "editor": "litz",
  "parts": [
    { "id": "esp",  "type": "board-esp32-c3-devkitm-1", "attrs": {} },
    { "id": "r1",   "type": "litz-resistor", "attrs": { "value": "220" } },
    { "id": "led1", "type": "litz-led", "attrs": { "color": "green" } }
  ],
  "connections": [
    ["esp:GPIO8", "r1:1", "green", []],
    ["r1:2", "led1:A", "green", []],
    ["led1:C", "esp:GND.1", "black", []],
    ["esp:TX", "$serialMonitor:RX", "", []]
  ]
}
```

| Key | Required | Meaning |
|---|---|---|
| `version` | no, default 1 | schema version |
| `author`, `editor` | no | provenance, ignored |
| `parts` | yes | placed instances |
| `connections` | yes | wires |
| `dependencies` | no | editor metadata, ignored |

A part instance:

| Field | Required | Meaning |
|---|---|---|
| `id` | yes | unique in the file; `[A-Za-z0-9_-]+`; `$` is reserved |
| `type` | yes | catalog key |
| `attrs` | no | scalar attributes, validated against the catalog |
| `left`, `top`, `rotate`, `hide` | no | presentation only |

A connection is a four-element array `[from, to, colour, route]`. Only the first two elements carry meaning; `colour` and `route` are presentation. A three-way join is written as three connections — there are no junction markers.

### 1.1 Pin references

```
part:pin            esp:GPIO8
part:pin.index      esp:GND.1        second GND contact
$rail               $gnd, $v33, $v5, $vin
$host:pin           $serialMonitor:RX
```

The trailing dot-segment is an index **only when it is all digits**. `btn1:1.l` is a pin literally named `1.l`, which is what the pushbutton part uses. `GND` with no index means the first contact.

Host devices and rails do not need a `parts` entry. They are the only `$` names a diagram may use:

| Name | Meaning |
|---|---|
| `$serialMonitor` | host serial endpoint; `RX` receives what the firmware prints, `TX` feeds the firmware |
| `$gnd` | global ground |
| `$v33`, `$v5`, `$vin` | global power rails |

## 2. Part — `part.toml`

A leaf component. One file per catalog entry; a directory of them is a catalog.

```toml
[part]
id = "litz-led"
name = "LED"
version = "1.0.0"
description = "5 mm through-hole LED"
license = "MIT"

[pins.A]
kind = "digital-in"
number = 1
label = "anode"

[pins.C]
kind = "ground"
number = 2

[attrs.color]
type = "enum"
values = ["red", "green", "blue", "yellow"]
default = "red"

[simulation]
backend = "builtin"
engine = "led"
```

`[part]` fields: `id` (required, immutable), `name`, `version` (semver, required), `description`, `license`.

Pin fields: `kind` (required), `number` (package pin, presentation), `repeat` (how many contacts share the name, default 1), `label`.

`kind` is one of `digital-in`, `digital-out`, `digital-io`, `analog-in`, `analog-out`, `analog-io`, `passive`, `power`, `ground`, `nc`, or one of the bus kinds `i2c-sda`, `i2c-scl`, `spi-mosi`, `spi-miso`, `spi-sck`, `spi-cs`, `uart-tx`, `uart-rx`. A bus kind says what the wire carrying that pin is; it is what lets flatten find the SDA, SCL, clock, chip-select and UART lines of a device without reading its engine.

Attribute fields: `type` (required; `bool`, `int`, `float`, `string`, `enum`), `values` (enum only, non-empty), `default` (must match `type`; for an enum it must be one of `values`).

### 2.1 Buses

A part that answers transactions instead of pin levels declares the one bus it sits on. The section name is the bus kind and controller index, and the body carries that kind's one field:

```toml
[buses.i2c0]
address = 0x76        # 7-bit I2C address; 0x08..=0x7f

[buses.spi0]
cs = "CS"             # the pin name that acts as chip select; that pin must be spi-cs

[buses.uart0]
port = 0              # the UART port; must agree with the name's index
```

The declared bus and the diagram's wires are the whole source of membership. The part's bus pins (`i2c-sda`/`i2c-scl`, `spi-sck`/`spi-mosi`/`spi-miso`/`spi-cs`, `uart-tx`/`uart-rx`) must each be wired to a chip pin; those are the GPIOs the bus uses. There is no second declaration on the board or in the header `functions`: a pin carrying a bus role is the declaration. Power and ground pins of a bus part remain ordinary pins, so a VCC and GND wire still reach the rails; the part's pin-level side never drives them. The device belongs to the board whose chip pin its bus pins reach, and the fabric is keyed by board and controller: two boards may each wire their own `i2c0`, and a declaration whose bus pins land on two different boards is refused rather than guessed.

A part may declare at most one `[buses.*]` section, and each kind requires exactly its own field: `i2c` takes `address`, `spi` takes `cs`, `uart` takes `port`. The catalog rejects a missing or foreign field, a second section, a non-canonical name such as `i2c01`, and a UART name whose `port` disagrees with its index. Flatten rejects ambiguous wiring: two devices at one address on one controller, two devices whose declared bus pins resolve to different GPIOs for the same controller, two devices on one chip select or UART port, and one GPIO claimed by two different bus roles. An SPI device needs `spi-sck`, its named `spi-cs`, and at least one of `spi-mosi` or `spi-miso`.

Simulation backends:

| Backend | Required | Meaning |
|---|---|---|
| `builtin` | `engine` | a model compiled into the runner, selected by name |
| `wasm` | `module` | a `.wasm` implementing the part ABI; `abi` defaults to `litz-part-2` (`litz-part-1` is still accepted) |

### 2.2 Builtin engines

These ids and engine names are Rust models compiled into the runner. The id is the stable name a `diagram.json` uses; a part is listed with the attributes that carry its simulated value.

| Part id | `engine` | Bus / pins | Attributes and control |
|---|---|---|---|
| `litz-ssd1306` | `ssd1306` | `[buses.i2c0] address = 0x3c`; `SDA`, `SCL` | none; 128x64 page-addressed GDDRAM |
| `litz-ds3231` | `ds3231` | `[buses.i2c0] address = 0x68`; `SDA`, `SCL` | `time` (`YYYY-MM-DDTHH:MM:SS`, default `2024-01-01T00:00:00`), `weekday` (1–7), `temperature` (°C); all in BCD registers |
| `litz-pcf8574` | `pcf8574` | `[buses.i2c0] address = 0x20`; `SDA`, `SCL` | none; `P0`–`P7` are a quasi-bidirectional port |
| `litz-mpu6050` | `mpu6050` | `[buses.i2c0] address = 0x68`; `SDA`, `SCL` | `ax`, `ay`, `az` (g, default 0/0/1) and `gx`, `gy`, `gz` (deg/s, default 0) |
| `litz-mcp3008` | `mcp3008` | `[buses.spi0] cs = "CS"`; `CLK`, `DIN`, `DOUT`, `CS` | `v0`–`v7` (volts per channel, default 0), `vref` (default 3.3) |
| `litz-dht22` | `dht22` | `SDA` (single wire) | `temperature` (°C, default 25), `humidity` (% default 50); a control moves temperature across −40…80 °C |
| `litz-pms5003` | `pms5003` | `[buses.uart0]`; `TX`, `RX` | `pm1`, `pm25`, `pm10` (µg/m³, defaults 4/7/9), `interval` (ms, default 1000); a 32-byte Plantower frame with checksum once per interval |
| `litz-hc-sr04` | `hc-sr04` | `TRIG`, `ECHO` | `distance` (cm, default 100); a control spans 2…400 cm |
| `litz-pir-motion-sensor` | `pir-motion-sensor` | `OUT` | a control at or above half raises `OUT` |
| `litz-photoresistor-sensor` | `photoresistor-sensor` | `AO`, `DO` | `light` (0–1000, default 500); a control moves the light level |
| `litz-ntc-temperature-sensor` | `ntc-temperature-sensor` | `AO`, `DO` | `temperature` (°C, default 25); a control spans −20…100 °C |
| `litz-rgb-led` | `rgb-led` | `R`, `G`, `B`, `COM` | none; a channel is on when its pin is high |
| `litz-led-bar-graph` | `led-bar-graph` | `A1`–`A10`, `C` | none; the count of high anodes is the bar level |
| `litz-relay` | `relay` | `IN`, `NO`, `NC`, `COM` | none; a high `IN` energizes the coil and moves COM to NO |
| `litz-l293d` | `l293d` | `IN1`, `IN2`, `EN`, `OUT1`, `OUT2` | none; `EN` enables the bridge and the two inputs pick direction or brake |
| `litz-74hc165` | `74hc165` | `D0`–`D7`, `PL`, `CP`, `CE`, `DS`, `Q7`, `Q7_N` | none; `PL` low loads, each rising `CP` shifts out MSB first |
| `litz-74hc138` | `74hc138` | `A0`–`A2`, `E1`, `E2`, `E3`, `Y0`–`Y7` | none; `E1`/`E2` active low, `E3` active high, outputs active low |
| `litz-capacitor` | `capacitor` | `1`, `2` | `value` (capacitance text, default `100n`); open at DC |
| `litz-inductor` | `inductor` | `1`, `2` | `value` (inductance text, default `10mH`); short at DC |
| `litz-bjt` | `bjt` | `C`, `B`, `E` | `polarity` (`npn`/`pnp`), `beta` (default 100), `vbe` (default 0.7 V) |
| `litz-mosfet` | `mosfet` | `D`, `G`, `S` | `channel` (`n`/`p`), `vth` (default 2.0 V) |
| `litz-opamp` | `opamp` | `+`, `-`, `OUT`, `V+`, `V-` | `gain` (default 100000) |

The last five are analog shells: `crates/litz-board/src/analog.rs` interprets them; their pin-level model only validates the pins and, for the inductor, passes DC through weakly.

A host control is the ABI's `set_control(permille)`: 0 is the bottom of the part's range and 1000 the top. A sensor also starts from its manifest attribute, so a diagram with no control wired still reads a sensible value. Timing parts use simulated nanoseconds only: the DHT22 frame, the HC-SR04 ECHO pulse and the PMS5003 measurement interval are scheduled through `next_event`/`advance` or the `Tick` a poll carries, never a wall clock.

The fourth group covers protocol displays, analog modules and bus sensors. Several ids share one engine where the electrical behaviour is identical; the attribute that differs is named in the last column.

| Part id | `engine` | Bus / pins | Attributes and control |
|---|---|---|---|
| `litz-tm1637-7segment` | `tm1637` | `CLK`, `DIO` | `color`, `type`; six display registers written with the two-wire start/bit/acknowledge sequence |
| `litz-max7219-matrix` | `max7219` | `[buses.spi0] cs = "CS"`; `DIN`, `CLK`, `DOUT` | `chain`, `color`, `layout`; 16-bit register words fill the row RAM |
| `litz-nokia-5110-screen` | `pcd8544` | `[buses.spi0] cs = "CE"`; `RST`, `DC`, `DIN`, `CLK`, `BL` | none; 84x48 bank framebuffer, 0x00 command / 0x40 data framing |
| `litz-ky-040` | `rotary-encoder` | `CLK`, `DT`, `SW` | none; shares the quadrature model with `litz-rotary-encoder` |
| `litz-reed-switch` | `reed-switch` | `1`, `2` | none; a magnet closes the contact (the tilt-switch model) |
| `litz-soil-moisture-sensor` | `analog-sensor` | `AO`, `DO` | `value` (0–1000), `threshold`, `activeLow`, `vref` |
| `litz-sound-sensor` | `analog-sensor` | `AO`, `DO` | same attributes as the soil sensor |
| `litz-vibration-sensor` | `analog-sensor` | `AO`, `DO` | same attributes as the soil sensor |
| `litz-rain-sensor` | `analog-sensor` | `AO`, `DO` | same attributes, defaulting to an active-low output |
| `litz-water-level-sensor` | `analog-sensor` | `AO` | `value`, `threshold`, `vref` |
| `litz-dc-motor` | `dc-motor` | `IN1`, `IN2`, `PWM` | none; direction from the inputs, speed from the measured PWM duty; a control sets signed speed |
| `litz-vibration-motor` | `vibration-motor` | `1`, `2` | none; runs while pin 1 is high and pin 2 is low |
| `litz-traffic-light` | `traffic-light` | `R`, `Y`, `G` | none; any driven lamp reports the module on |
| `litz-led-strip` | `neopixel` | `DIN`, `DOUT` | `pixels`; shares the WS2812 decoder |
| `litz-led-matrix` | `neopixel` | `DIN`, `DOUT` | `rows`, `cols`; shares the WS2812 decoder |
| `litz-relay-module` | `relay` | `IN`, `NC`, `COM`, `NO` | `transistor`; shares the single-pole relay model |
| `litz-clock-generator` | `clock-generator` | `CLK` | `frequency` (Hz with optional `k`/`m` suffix, default `10k`); edges are scheduled on simulated time |
| `litz-signal-generator` | `signal-generator` | `OUT`, `GND` | `wave` (`square`, `sine`, `triangle`, `sawtooth`), `frequency` (default `1k`, clamped 1 Hz–1 MHz), `duty` (percent, default `50`), `amplitude` (peak-to-peak volts, default `3.3`), `offset` (volts, default half the amplitude), `phase` (degrees); a square is scheduled edge by edge, other shapes step 64 times per period |
| `litz-a4988` | `a4988` | `STEP`, `DIR`, `ENABLE`, `RESET`, `SLEEP`, `MS1`–`MS3`, `1A`, `1B`, `2A`, `2B` | none; `ENABLE`/`RESET`/`SLEEP` active low, microstep from the `MS` pins |
| `litz-ks2e-m-dc5` | `dpdt-relay` | `COIL1`, `COIL2`, `P1`/`NC1`/`NO1`, `P2`/`NC2`/`NO2` | none; one coil moves both poles |
| `litz-pcf8591` | `pcf8591` | `[buses.i2c0] address = 0x48`; `AIN0`–`AIN3`, `AOUT` | `ain0`–`ain3` (volts), `vref`; control register, auto-increment |
| `litz-ads1115` | `ads111x` | `[buses.i2c0] address = 0x48` | `a0`–`a3` (volts); 16-bit conversion register |
| `litz-ads1015` | `ads111x` | `[buses.i2c0] address = 0x48` | `a0`–`a3`; 12-bit result left-aligned in the same register |
| `litz-ds18b20` | `ds18b20` | `DQ` (1-Wire) | `temperature` (°C), `deviceID`; reset/presence, ROM and scratchpad commands |
| `litz-sht31` | `sht31` | `[buses.i2c0] address = 0x44` | `temperature`, `humidity`; CRC-checked six-byte result |
| `litz-bh1750` | `bh1750` | `[buses.i2c0] address = 0x23` | `lux`; continuous and one-shot modes |
| `litz-tcs34725` | `tcs34725` | `[buses.i2c0] address = 0x29` | `clear`, `red`, `green`, `blue`; ID 0x44 |
| `litz-vl53l0x` | `vl53l0x` | `[buses.i2c0] address = 0x29` | `distance` (mm); model ID 0xEE |
| `litz-pca9685` | `pca9685` | `[buses.i2c0] address = 0x40`; `OE`, `PWM0`–`PWM15` | `frequency`; 12-bit ON/OFF counters per channel |
| `litz-uln2003` | `uln2003` | `IN1`–`IN7`, `OUT1`–`OUT7` | none; a high input pulls its open-collector output low |
| `litz-mcp4725` | `mcp4725` | `[buses.i2c0] address = 0x60`; `VOUT` | `value` (0–4095); normal and fast write both decode |
| `litz-ina219` | `ina219` | `[buses.i2c0] address = 0x40`; `VIN+`, `VIN-` | `busVoltage`, `shuntMillivolts`, `currentMilliamps` |
| `litz-bme280` | `bme280` | `[buses.i2c0] address = 0x76` | `temperature`, `humidity`, `pressure`; raw data registers, ID 0x60 |
| `litz-apds9960` | `apds9960` | `[buses.i2c0] address = 0x39` | `clear`, `red`, `green`, `blue`, `proximity`; ID 0xAB |
| `litz-74hc4067` | `cd4067` | `S0`–`S3`, `E`, `COM`, `I0`–`I15` | none; the selected channel and `COM` are one conductor |
| `litz-tsl2561` | `tsl2561` | `[buses.i2c0] address = 0x39` | `lux`; ID 0x50, two 16-bit channels |
| `litz-veml7700` | `veml7700` | `[buses.i2c0] address = 0x10` | `lux`; 16-bit little-endian ALS and white registers |
| `litz-mlx90614` | `mlx90614` | `[buses.i2c0] address = 0x5a` | `object`, `ambient` (°C); SMBus word reads |
| `litz-fuse` | `fuse` | `1`, `2` | `rating`; conductive until a host control blows it |
| `litz-trimmer-potentiometer` | `potentiometer` | `1`, `2` (wiper), `3` | `value` (percent of travel) |
| `litz-dip-switch-4` | `dip-switch` | `1a`/`1b` through `4a`/`4b` | none; the control mask sets the four positions |
| `litz-logic-level-converter` | `level-shifter` | `LV`, `HV`, `LV1`–`LV4`, `HV1`–`HV4` | none; a level on one side appears weakly on the other |

Two limits of the shared bus layer show through in this group. The PCD8544 model reads its command/data framing from a leading 0x00/0x40 byte on the SPI stream, as the ILI9341 model does, because [`SpiDevice`] sees bytes rather than a DC line. The PCA9685's PWM pins are declared for wiring, but the register model exposes its duty through its typed read-back rather than driving those nets; the same split applies to any device whose behaviour lives on a bus. The BME280 exposes the raw pressure, temperature and humidity words but leaves the factory calibration registers at zero.

### 2.3 Radio, network and Bluetooth parts

These parts own an air interface rather than a pin-level behaviour. A radio part decodes the SPI edges a driver produces and hands frames to the board's packet medium (`litz-radio`), which schedules each frame at the tick it was armed plus its own airtime and destroys two frames whose airtimes overlap on one network and channel. Two radios hear each other when their network and channel agree, whether they are on one board or on two boards a runner put on the same medium. There is no wall clock and no randomness, so the same calls deliver the same frames in the same order.

| Part id | `engine` | Pins | Attributes |
|---|---|---|---|
| `litz-wifi-router` | `router` | `3V3`, `GND` | `ssid` (default `litz`), `psk`, `channel` (1–14, default 6), `http` (the body on port 80, default `hello from litz\\n`), `echo` (TCP echo port, default 7), `udp_echo`, `mqtt` (MQTT broker port, default 1883, 0 for none), `nearby` (networks a scan reports, `ssid=NAME[,rssi=-70]`) |
| `litz-ble-beacon` | `ble-beacon` | `3V3`, `GND` | `name` (1–26 bytes), `address` (`aa:bb:cc:dd:ee:ff`, empty derives a stable random-static address from the advertising data), `rssi` (default −60), `services` (four-digit UUIDs), `adv` and `rsp` (hex; `adv` replaces the data `name` and `services` build, so it and `name` are exclusive), `gatt` (services and characteristics a central discovers: `180f.2a19=64, 180f.2a1a+w`, where flags after `+` are `r`, `w` and `n`) |
| `litz-nrf24l01` | `nrf24l01` | `SCK`, `MOSI`, `MISO`, `CSN`, `CE`, `IRQ` | `network`, `channel` (0–125, default 76), `rate` (`1M`/`2M`), `address` (five bytes as hex), `payload_size` (1–32, default 32) |
| `litz-sx1276` | `sx1276` | `SCK`, `MOSI`, `MISO`, `NSS`, `RESET`, `DIO0`, `DIO1` | `network`, `frequency` (MHz, default 868), `sf` (spreading factor, default 7), `bandwidth` (kHz, default 125), `power` |
| `litz-at86rf233` | `at86rf233` | `SCK`, `MOSI`, `MISO`, `SELN`, `RSTN`, `IRQ`, `SLPTR` | `network`, `channel` (11–26, default 15), `pan` (default `0xabcd`), `address` (short, default `0x0001`), `extended_address` (16 hex digits), `power` |

The Wi-Fi router has no behaviour of its own: its attributes become the network the chip's radio puts on the air, read through `Board::network`, and `[sim.wifi]` or `--wifi` replaces that network for a project or a run. Everything past the gateway — DNS, TCP, UDP, TLS, WebSocket and the MQTT broker — is `litz-net`, so the same circuit serves an ESP32, a C6 or a wasm MCU. The MQTT broker speaks 3.1.1 and 5.0: CONNECT with clean and persistent sessions, SUBSCRIBE with shared subscriptions (`$share/<group>/<filter>`), PUBLISH at QoS 0, 1 and 2, retained messages, will messages delivered on an abnormal disconnect, PINGREQ, DISCONNECT and the MQTT 5 property block. A persistent session queues QoS > 0 messages while its client is away and replays them on reconnect.

`--pcap FILE` writes every exchange the router answered as a nanosecond-resolution pcap: synthetic Ethernet II frames carrying IPv4, TCP, UDP and DNS, timestamped from simulated time. The capture is a property of the network, not of the backend, so a run on an ESP32, a C6 or a wasm MCU produces the same bytes for the same firmware, and two runs of the same firmware produce byte-identical files. TLS records appear as ciphertext, exactly as they cross the wire.

The TLS endpoint is TLS 1.3 with the simulated CA in `crates/litz-net/certs/` (`api.litz.dev`, valid 2000–2100). Its handshake randomness comes from a ChaCha20 generator seeded at compile time and its X25519 key exchange from the same stream, so two runs of the same firmware produce the same records and a pcap of an HTTPS connection is reproducible; the same bytes appear whether the run is native or in the browser. A firmware trusts the server by loading `ca.pem`; a client that does not is rejected. `Service::https`/`Service::wss` wrap the HTTP and WebSocket services, and `[[sim.wifi.service]]` reaches them with `tls = true`. `websocket = true` makes a WebSocket echo server, which answers the RFC 6455 upgrade and echoes text and binary frames without a mask. The browser (`wasm32`) build carries the same endpoint: the provider is pure Rust, the suite is pinned to `TLS13_CHACHA20_POLY1305_SHA256`, and the module is instantiated with no imports, so an HTTPS service behaves the same on a page and on the command line.

The Bluetooth beacon works the same way through `Board::bluetooth`; a scanning guest reaches it through the chip's HCI controller, which is started by patching the guest's own controller task, so a project with a beacon also needs `[sim] elf` (§4). Without one the runner refuses the run instead of putting nothing on the air. A beacon with a `gatt` table accepts a connection from a custom chip: discovery reports the UUIDs, reads return the current values, writes update them and are queued for the owner, and a notifiable characteristic reaches every subscribed central. A pure beacon has no table and refuses the connection, as a real one would. A connection is not instantaneous: the first event follows the connect request by 7.5 ms and each ATT exchange rides one 30 ms connection event, so the host's clock advances by the interval for every discovery, read, write, subscribe and notification poll. A native ESP32 refreshes its HCI scan list from the board whenever a runtime advertiser starts or stops, and the list carries each beacon's GATT table, so a guest acting as a central can connect to a scanned address and discover, read and write that table. The C3's full link controller (`enable_full_ble`, driven by `ble_command`) models connection events, channel hopping and the 30 ms interval for GATT transactions. The scan-list change lives in `vendor/esp-soc` (see `PATCH.md`) because upstream at `d043ff1` fixes the table to one built-in battery service.

The nRF24L01 and SX1276 models answer the register sequences a real driver sends: mode 0 SPI, a chip select around each transaction, and the subset of registers that changes behaviour. The AT86RF233 speaks its register map and frame buffer and decodes IEEE 802.15.4 MAC data frames (FCF, PAN and address filtering, FCS, broadcast); a bad FCS is dropped, a broadcast on another PAN is filtered, and a frame is only readable after its airtime has elapsed. Security, information elements and the hardware acknowledgement path are not modelled.

## 3. Board — `board.toml`

A board is a composition: one MCU, the parts soldered to it, and the header pins the outside world sees. It resolves to the same `PartManifest` a leaf does, so a diagram treats `board-*` exactly like `litz-*`.

```toml
[board]
id = "board-esp32-c3-devkitm-1"
name = "ESP32-C3-DevKitM-1"
version = "1.0.0"
chip = "esp32c3"
flash_size = "4MB"
psram = false
schematic = "board-esp32-c3-devkitm-1.onboard.json"

[[pins]]
name = "GND"
fixed = "gnd"
repeat = 2

[[pins]]
name = "GPIO8"
gpio = 8
functions = ["strap"]

[[pins]]
name = "TX"
gpio = 21
functions = ["uart"]
```

`[board]`: `id` (immutable), `name`, `version`, `flash_size`, `psram`, `schematic` (file name next to the manifest), and exactly one of `chip` (a backend chip id such as `esp32c3`) or `[board.simulator]` (§3.1).

Header pin fields: exactly one of `gpio` or `fixed`; plus `repeat` (default 1), `label`, `functions` (free-form capability tags such as `adc`, `pwm`, `i2c`, `uart`, `spi`).

`fixed` is one of `gnd`, `3v3`, `5v`, `vin`.

### 3.1 A board whose MCU is a wasm module

`chip` names a backend compiled into the runner. A board can instead name a WebAssembly component that speaks the `litz:mcu` contract (see §10):

```toml
[board]
id = "board-litz-rv32"
name = "Litz RV32 reference"
version = "1.0.0"
schematic = "board-litz-rv32.onboard.json"

[board.simulator]
kind = "wasm"                # the only kind today
module = "litz-rv32.wasm"    # relative to this manifest, no ".."
abi = "litz-mcu-1"           # optional; this is the default
```

A chip that needs buses or the radio is built against `litz:mcu@0.2.0` and names `abi = "litz-mcu-2"` (§10). `litz board check <dir>` parses the manifest, resolves the schematic and module beside it, and instantiates the component under the ABI the manifest names, so a bundle is tested before a project depends on it.

`chip` and `[board.simulator]` are mutually exclusive, and one of them is required; a board with both or neither is rejected with `board-simulator`. `module` is resolved next to the manifest, never against the current directory, and a path that is absolute or contains `..` is refused when the catalog loads. Builtin boards carry the module inside the binary, so a stock install needs no files.

A board installed by a user is a directory, `boards/<id>/board.toml` with its module and schematic beside it; the runner and `litz catalog` both read that one level. `litz catalog new board` creates exactly this layout, plus the simulator's source.

### 3.2 Expansion rules

A board instance is flattened before the netlist is built, with names that cannot collide:

| Outside | Inside |
|---|---|
| `esp:GPIO8` | `esp/chip:GPIO8` |
| `esp:TX` (gpio 21) | `esp/chip:GPIO21` |
| `esp:GND.1` | `$gnd` |
| `esp/led_pwr:A` | child part, prefixed |

The onboard schematic uses one reserved type, `$chip`, for the MCU. It exposes exactly `GPIO<n>` for every header pin with `gpio = n`; power comes from the global rails. Child part ids are prefixed with `<instance>/`, so an onboard `led_pwr` is reachable but never collides with a top-level `led_pwr`.

Expansion is depth-first and deterministic. A board inside another board's schematic is an error: a nested board's chip pins would have no second board index to name them, and its firmware no second entry to hide behind.

## 4. Run configuration — `litz.toml`

```toml
[project]
diagram = "diagram.json"

[sim]
firmware = "build/hello.bin"
bootloader = "build/bootloader.bin"
partition_table = "build/partition_table.bin"
seed = 0
runtime = "native"          # or "micropython" with an optional script
# script = "main.py"

[[test]]
name = "boot"
timeout = "10s"
expect = "Hello world!"
fail = "Guru Meditation"
expect_state = "led1:on"

[[test.input]]
at = "2s"
board = "b"                 # only needed when the diagram has several boards
serial = "status\n"

[[probe]]
name = "pwm"
point = "esp:GPIO8"         # <board>:<GPIO> or <part>:<pin>, e.g. led1:A

[[probe]]
name = "tx"
point = "esp:GPIO8"         # a line the firmware drives itself, e.g. a bit-banged UART

[[decode]]
kind = "uart"               # uart, i2c or spi
rx = "tx"                   # a channel already named by a [[probe]]
baud = 115200               # optional; inferred from the shortest run when absent
```

`[sim]`: `firmware`, optional `bootloader` and `partition_table` (ESP-IDF layout: 0x0, 0x8000, 0x10000 — with neither, the app is written at 0x10000 and started directly, which is what a bare image needs), optional `flash_size`, `mac`, `rom`, `elf`, `runtime`, `script` and `analog`. `elf` names the firmware's symbol table: the image runs without it, but a Bluetooth scan needs it, and it is what names addresses in logs, disassembly and the debugger. `analog = true` solves the circuit's analog network alongside the digital run; the `--analog` flag turns it on for one invocation without editing the file. `wifi = "ssid=NAME,psk=PASS"` attaches a simulated access point to an ESP32-family radio; `--wifi` overrides it for one run. The radio only exists on imported esp32sim chips, so an `atmega328p` board refuses the option rather than ignoring it. `seed` is reserved: it is parsed and does nothing yet. The mask ROM is Espressif's, Apache-2.0, and is not vendored: `rom` names it, or `LITZ_ROM_DIR` names the directory holding `esp32c3_rev3_rom.elf`. Durations are strings with an SI suffix (`ms`, `s`, `m`); the runner converts them to ticks.

With several boards, `litz.lock` has no single `[firmware]` table to describe the run, so the images are folded into the one firmware hash in board order: each board's id, then the length and bytes of its application image and its mask ROM. Every byte that booted is in that hash, so the run is still repeatable; `rom_sha256`, `bootloader_sha256` and `partition_table_sha256` are left out because the folded hash already covers them.

`firmware` is either an application image or a merged image, decided by its bytes rather than its file name. A merged image has the ESP image magic `0xE9` at byte 0 and the partition-table magic `0xAA50` at 0x8000, or at 0x7000 for a classic ESP32 factory image (whose flash base is 0x1000); a file longer than 0x10000 with an image header at 0x10000 is also merged. A merged image is written in one piece at the bootloader offset (0 on the RISC-V parts) and the mask ROM starts it, so it needs `rom`/`LITZ_ROM_DIR`; it already contains the partition table and application, and setting `bootloader` or `partition_table` alongside it is an error. An application image is written at 0x10000 and started directly, unless `bootloader` and `partition_table` name the pieces that should boot it.

A board whose MCU is an RP2040 takes a flat dump of QSPI flash starting at the XIP window, `0x1000_0000`: the file is written to flash offset 0 with no header, and the run follows the chip's own boot. The first 256 bytes are stage 2, which the ROM copies to SRAM at `0x2000_0000` and calls with `LR` pointing back at the ROM and the stack at `0x2004_0000`; the application's vector table must be the 256 bytes after it, at `0x1000_0100`, so stage 2 returning is what starts the program. A `boot2` image from the Pico SDK is such a file already, and `cargo run -p litz-fixture -- stage2 <out>` writes the 256-byte stage 2 the fixtures use. MicroPython publishes the Pico build as a UF2 rather than a flat image; `tools/fetch-micropython.sh` verifies it and converts it.

`runtime` is `"native"` (the default), `"micropython"` or `"circuitpython"`. With a Python runtime and a `script`, the runner waits for the REPL prompt (`>>> `) on the console — bounded by the test's `timeout` (`run` and a scenario use a ten-second simulated default), never a wall clock — then sends the runtime's interrupt (Ctrl-C for CircuitPython, which may still be running `code.py`), the raw-paste sequence Ctrl-A (0x01), the script bytes and Ctrl-D (0x04). MicroPython runs `main.py`; CircuitPython runs `code.py` and accepts `main.py` as the fallback entry. The script then runs immediately from the REPL; no filesystem image is needed. A paste is capped at 64 KiB, and a prompt that never appears is reported as an error rather than a failed assertion. With no `script`, the firmware simply runs, and `expect`, `fail` and scenario steps read the same console either way. `script` is a project-relative path and is only valid with a Python runtime.

CircuitPython differs from MicroPython in three ways the runner has to respect. Its build ships with Bluetooth and brings the controller up before `code.py` runs, so a `circuitpython` board gets a Bluetooth controller whether or not the project asks for beacons — the registers alone are not enough, and a controller-less chip stops in the ROM's init. It runs `code.py` on its own and then idles at "Press any key to enter the REPL", so a key has to arrive before the friendly prompt does; the runner sends Ctrl-C every 200 ms of simulated time while the console has said anything and the prompt has not appeared. And its console is the chip's USB serial/JTAG block, which the ESP32-C3 has: `examples/circuitpython` boots the official C3 build through the mask ROM and reaches the REPL. An ESP32-S3 build prints nothing here, because its console is the USB OTG controller, which the model does not provide.

A diagram with more than one development board gives each a `[[board]]` entry, and each chip boots its own image:

```toml
[sim]
flash_size = "4MB"

[[board]]
id = "a"                       # the board instance's id in diagram.json
firmware = "firmware/a.bin"

[[board]]
id = "b"
firmware = "firmware/b.bin"
bootloader = "firmware/b-bootloader.bin"   # optional, per board
partition_table = "firmware/b-ptable.bin"
rom = "build/roms/esp32c3_rev3_rom.elf"
elf = "firmware/b.elf"
runtime = "micropython"
script = "b.py"
flash_size = "4MB"
mac = "24:0a:c4:12:34:56"
```

`[[board]]` fields: `id` (required, the instance id), `firmware` (required), and the same optional inputs as `[sim]` — `bootloader`, `partition_table`, `rom`, `elf`, `runtime`, `script`, `flash_size`, `mac`. A field a board leaves out falls back to `[sim]`. Every development board in the diagram must be named exactly once and no other id may be listed; the runner refuses a missing or duplicate entry rather than guessing which firmware goes where. `[sim] firmware` and `[[board]]` are exclusive. With no `[[board]]`, the project has one board and `[sim]` describes it, exactly as before.

The consoles of all boards are merged in board order at each scheduling batch, and an `expect` or `fail` entry matches the merged text. A leading `<board>@` narrows one entry to that board's console — `expect = ["a@litz: holding GPIO8 low", "b@litz: press the button"]` — and is read as a board only when it names one, so any other text (an `@` included) stays a plain match. Part-state assertions read the one shared board. `[[test.input]]` takes a `board` field: required when the diagram has more than one development board, omitted for a single-board project. Time advanced in the run loop is the frontier every running board has reached, so a board that halted does not hold the others back. `--gdb [<board>@]<port>` serves one board; without a board it picks the first one whose backend has a debug target, because a session owns its chip's clock until the client disconnects.

`[[test]]`: `name`, `timeout`, `expect`, `fail`, `expect_state`, `state_at`, `state_stays`, and repeatable `[[test.input]]` steps with `at`, `board`, and `serial` or `pin`/`level`.

`[[test.input]]` schedules host input at `at` (simulated time) on the board `board` names — the only board unless the diagram has more than one. `serial` feeds a line into that board's UART0. `pin` with `level` names either a GPIO number — the level is asserted onto the chip pin — or a part id, in which case the part is pressed: `level = 0` holds a button down and `1` releases it, and any circuit behind the switch reacts as it would to a person.

`[[decode]]` is repeatable, one entry per protocol stream: `kind` (`uart`, `i2c` or `spi`), optional `name` (the report label, default `<kind> <first channel>`), the channels that protocol reads — `rx` for UART, `sda` and `scl` for I2C, `sck`, `mosi` and `cs` with an optional `miso` for SPI — and optional `baud` (UART) or `mode` (SPI, 0–3). Every channel is a probe name, not a point: each one must already be named by a `[[probe]]` (or `--probe`) channel, because a decoder reads the probe record and nothing else, and a name that is not a probe channel is refused before the firmware boots.

`expect_state` asserts on the circuit rather than on text: `"<part>:<state>"`, one string or a list, where the state is `on`/`off` for an LED and `pressed`/`released` (`down`/`up` for short) for a button. Each entry has to be true at some point before the test ends — the same "at some point" semantics as `expect`, so a blinking LED is asserted with `led1:on` alone. `state_at = "1s"` changes that to a sample at one simulated instant: a state that was true earlier and false at 1 s does not pass. `state_stays = "300ms"` extends a match into an interval, starting at `state_at` when both are given and at the first match otherwise; one false observation breaks it. A part name that is not in the board is an error before the firmware even boots; an onboard part is addressed with its instance prefix (`esp/led_pwr`).

A test passes when every `expect` string appeared, no `fail` string appeared, and the simulation did not stop on a trap before `timeout` of simulated time.

The command has three forms, with fixed exit codes:

| Command | Does | Exit |
|---|---|---|
| `litz run [dir]` | boots the firmware, streams serial to stdout | 0 when every board halts, 42 at the time limit, 1 on a fault |
| `litz test [dir]` | runs the `[[test]]` entries | 0 all passed, 1 any failed |
| `litz check [dir]` | validates the diagram, loads nothing | 0 valid, 1 errors |

`litz lint` is an alias for `check`. `--strict` (also spelled `--warnings-as-errors`) promotes validation warnings to errors; `--coverage` asks the backend for an instruction-coverage report and prints it after the run without changing the exit code; `--interactive[=BOARD]` forwards lines typed on stdin to that board's console, the first board when none is named. `--screenshot [<board>@]<file>` writes the board's display or panel memory as a PNG at the end of a `run` and when a test fails; without a board the first board that has a framebuffer is captured, and a board with none says so on stderr while the run's exit code stays what it was. A board name that is not in the diagram is refused before the firmware boots. `--probe POINT` watches a point for one run and may be repeated; `--scope FILE` writes every probe's record as a CSV when the run ends. `--decode SPEC` adds a protocol decoder for one run and may be repeated; a spec is `uart:rx=tx[,baud=N]`, `i2c:sda=a,scl=b` or `spi:sck=c,mosi=d,cs=e[,miso=f][,mode=N]`, each optionally with `name=`, and channel names cannot contain `,` or `=` because those are the separators. Decoding reads the probe record and never changes the run. `--gdb [<board>@]<port>` serves the GDB remote protocol on 127.0.0.1 once the chip is booted and returns when the client disconnects, after which the run continues; the RISC-V adapters have a target, the Xtensa and AVR ones refuse rather than advertising a layout they do not have. With several boards the board name picks the chip; without one the first board that has a target is served, and naming a board without one is an error that says so.

Electrical faults are reported as `litz: electrical fault on net N: <detail>` after the serial output, and they fail the run whether or not a test was asserted.

For a CI consumer there are two machine-readable forms, both derived from the same result the exit code comes from: `--json` prints one record (`status`, `reason`, `exit_code`, `sim_time_ms`, and one entry per test with its transcript) instead of the human output, and `--junit FILE` writes a JUnit XML report with each test as a `testcase` and the serial log as `system-out`.

### 4.1 Probes and the scope

A probe names one point in the circuit and records what happens there. The point is `<board>:<GPIO>` with a board instance id and a GPIO number (`esp:GPIO8`, `b:4`) or `<part>:<pin>` with a part id and a catalog pin name (`led1:A`, `esp/led_pwr:A`); a point that is not wired to anything, a part that is not in the diagram and a pin the part does not have are all refused before the firmware boots. `[[probe]]` in `litz.toml` names the channel with `name` (default: the point text), and `--probe` adds one for a single run.

A channel is a step waveform, not a fixed-rate sampling grid: the digital part is every net transition since recording started, and the analog part is the solved voltage at every settle. A value therefore holds between changes and no sample falls between two states the circuit was never in. Volts only appear when analog co-simulation is on (`--analog`); the level channel is recorded either way.

The report prints one line per probe after the run: frequency, duty, edge count, pulse range and the voltage range when there is one (`litz: pwm: 1.000 kHz, duty 50.0%, 20 edges, pulse 500us..750us, 0.000..3.300 V`). Frequency comes from the rising edges, a pulse is a complete high or low run between two transitions, and a release to high impedance is not an edge. `--scope FILE` writes the same record as a CSV: `time_ns` then one column per channel (plus `<name>_volts` when that channel has analog samples), one row per time any channel changes, values held between changes, levels written `1`, `0` or `z`.

**Protocol decoding.** A decoder reads the same record a probe already holds. `[[decode]]` in `litz.toml`, or `--decode` for one run, names a protocol and the channels it reads: UART takes `rx` and an optional `baud`, I2C takes `sda` and `scl`, SPI takes `sck`, `mosi` and `cs` plus an optional `miso` and a `mode` from 0 to 3. Every channel is a probe name, so each line a decoder reads needs a `[[probe]]` (or `--probe`) already watching it; a name that is not a probe channel is refused before the firmware boots. Decoding is a reading of the record, not an instrument connected to the circuit: it adds no load and does not change the simulation.

A byte-level peripheral is observable too. When an ESP32 or AVR adapter moves a UART byte, an I2C transaction or an SPI frame through the board's bus layer, the runtime reconstructs the edges a probe on the wired pins would have seen (`crates/litz-board/src/waveform.rs`) and merges them into the record in time order. The transaction is still the byte transfer that reached the device; the waveform is an observation and drives nothing. The reconstruction uses the rates the observation record declares — 115 200 baud for UART, 100 kHz for I2C, 1 MHz SPI mode 0 for SPI (`litz_board::runtime::OBSERVED_*`) — because the byte layer does not carry a peripheral's configured divisor. That is the right shape for decoding and measurement; a firmware that bit-bangs a pin is recorded from its real edges instead. Idiomatic I2C, SPI and UART firmware on the built-in backends therefore decodes from a probe on SDA/SCL, SCK/MOSI or TX, and the bus device still answers exactly once.

UART is 8N1, LSB first, with the line idle high; when `baud` is absent it is inferred from the shortest run between changes. I2C is 7-bit, with START and STOP framed by ACK/NACK, MSB first. SPI is MSB first with chip select active low, and `mode` 0–3 samples on the rising edge for 0 and 3 and on the falling edge for 1 and 2.

After the run each decoder prints one line per frame:

```text
litz: uart tx: "hello"
litz: uart tx: 0x00 (stop bit was low)
litz: i2c bus: start 0x3c write, 0x00 ack, 0xaf nack, stop
litz: spi flash: 0x9f 0x00 0x00 -> 0x12 0x34 0x56
```

A decoder only sees the channels the project probes, and its output is a report rather than an assertion: a decoded frame never changes the exit code. A decoder that finds nothing prints one `<label>: no frames` line, so silence always means the decoder was not configured.

## 5. Lock file — `litz.lock`

Written by the runner, committed by the project. It records the exact catalog entries and firmware hashes a result was produced with, so a green CI run can be reproduced after the catalog moves. A merged image is hashed as `[firmware]` like any other: the lock records bytes, and whether the runner writes them at the bootloader or application offset does not change them.

```toml
version = 1

[[entry]]
type = "litz-led"
version = "1.0.0"
sha256 = "…"

[[entry]]
type = "board-esp32-c3-devkitm-1"
version = "1.0.0"
sha256 = "…"

[firmware]
sha256 = "…"
```

The runner refuses to start when the lock and the catalog disagree unless `--update-lock` is passed.

## 6. Electrical rules

Nets are the transitive closure of connections. There are no junction objects.

Every pin has a `kind`, and validation rejects a wire between incompatible kinds:

| Kind | Connects to |
|---|---|
| `nc` | nothing |
| `power` | anything but `ground` |
| `ground` | anything but `power` |
| anything else | anything |

A GPIO wired to ground is therefore legal in the file and a question for the electrical layer: while the pin is an input nothing is wrong, and the moment it drives high the run stops with a fault. That is the behaviour a breadboard has.

Two different fixed rails on one wire are a `rail-short`, and that one is rejected before anything runs.

Drive resolution on a net, in order:

| Situation | Result |
|---|---|
| one or more strong drivers agree | that level |
| strong drivers disagree | electrical fault, the run stops |
| no strong driver, weak drivers agree | that level |
| no strong driver, weak drivers disagree | floating |
| nothing drives | floating; a receiving pin's pull applies |

Weak drivers pulling opposite ways describe a resistor divider — legal hardware holding a level between the rails. Two states cannot say "half way", so the net reads as floating rather than as a fault.

Each time a chip output or a part changes, every net is recomputed and every part notified, until nothing moves. Eight passes bound that; a board that has not settled by then is reported as a fault instead of looping.

Wires propagate in zero time within a tick. A part that needs a delay declares it in its model; there is no wire delay in the format.

Without analog co-simulation (§6.1), a net is a single resolved level. A part that needs analog behaviour samples or drives a voltage and the runner maps it, but nothing solves the circuit. With the solver on, DC and a caller-driven backward-Euler transient are in scope; transmission lines, distributed parasitics, automatic step control and small-signal AC analysis are not.

### 6.1 Analog co-simulation

An optional DC solve, or a backward-Euler transient, turns the same flat nets into a netlist, so a chip's ADC can read a real node voltage. It is off by default; when it is off the digital results above are unchanged. The diagram needs no new syntax: the analog values already in a part's attributes are what the solve reads.

| Part (builtin engine) | Attribute | Analog meaning |
|---|---|---|
| `litz-resistor` (`resistor`) | `value` | resistance in ohms, in electronics notation: `220`, `10k`, `4k7`, `2.2k`, `1M`, `4R7`. Default `220`. |
| `litz-led` (`led`) | `color` | LED forward voltage: red 1.8 V, green/yellow/orange 2.0 V, blue 3.0 V, white 3.2 V. The junction is a Shockley diode rated for that drop at 20 mA, in series with a small internal resistance. |
| `litz-potentiometer` (`potentiometer`) | `value` | initial position, 0–100 percent of travel from pin 1 to pin 3. It is an initial value, not a resistance. Track resistance is 10 kΩ unless the part also carries `resistance`. |
| `litz-capacitor` (`capacitor`) | `value` or `capacitance` | capacitance in farads with an SI prefix: `100p`, `100n`, `100nF`, `4.7u`, `2.2µF`, `1mF`. Default `1u`. |
| `litz-inductor` (`inductor`) | `value` or `inductance` | inductance in henries: `100n`, `10u`, `10uH`, `4.7mH`. Default `1m`. |
| `litz-pushbutton` (`pushbutton`) | none | a contact between pins `1.l`/`1.r` and `2.l`/`2.r`: 0.1 Ω pressed, 10 MΩ released. Driven by `press`. |
| `litz-slide-switch` (`slide-switch`) | none | common pin `2`: 0.1 Ω to the resting throw `1` or `3`, 10 MΩ to the other. Driven by `press` (it latches). |
| `litz-ntc-temperature-sensor` (`ntc-temperature-sensor`) | `temperature` | initial °C from −20 to 100. A 10 kΩ divider from VCC to ground whose ratio follows the part's control; the solved `AO` voltage is the temperature as a fraction of the reference. |
| `litz-photoresistor-sensor` (`photoresistor-sensor`) | `light` | initial light 0–1000. The same divider model; a control moves the light level. |
| generic transistor (`bjt`) | `beta`, `vbe`, `early_voltage` | NPN Ebers-Moll: current gain (default 100), base-emitter turn-on at 1 mA (default 0.7 V), Early voltage (default 100 V). Pins `C`, `B`, `E`. |
| generic transistor (`mosfet`) | `vth`, `k`, `lambda` | enhancement NMOS level 1: threshold (default 2 V), transconductance parameter in A/V² (default 10 mA/V²; `beta` is accepted as another name), channel-length modulation (default 0). Pins `D`, `G`, `S`. |
| generic op-amp (`opamp`, `op-amp`) | `gain`, `rail_low`, `rail_high` | finite-gain voltage-mode amplifier (default gain 1e5) clamping to its rails; rails default to 0 V and `$v33`. Pins `+`, `-`, `OUT`. |
| generic diode (`diode`) | `vf` | forward voltage used to derive the diode model. Omit it for a small-signal silicon diode. |
| generic source (`voltage-source`, `dc-source`, `vsource`, `battery`) | `voltage` or `value` | ideal voltage from the positive pin to the negative pin. |
| generic source (`current-source`, `isource`) | `amps` or `value` | ideal current from the positive pin to the negative pin. |

Mapping rules:

- One net is one node. The net carrying `$gnd` is ground (node 0). `$v33`, `$v5` and `$vin` are ideal sources to ground at 3.3 V, 5 V and 5 V unless the runtime configures them otherwise.
- A chip pin that is a strong output is a Thevenin source at 3.3 V or 0 V behind a 50 Ω driver resistance. A high-impedance input contributes nothing.
- A potentiometer's wiper is the junction between two resistors; its solved voltage is what the chip's ADC reads, replacing the static `analog()` value for that pin.
- A net no element and no active driver reaches is floating. Its voltage is `None` — high impedance — not 0 V. An LED is lit when its solved forward current is above a small threshold.
- A capacitor is an open circuit and an inductor a short at DC. In a transient an inductor is a real node connection from `t = 0` while a capacitor is one only as its voltage builds, which is why a net reached solely through a capacitor is floating in DC mode and solved in transient mode.
- The solver models NPN bipolar and N-channel MOSFET devices. A `bjt` with `polarity = "pnp"` or a `mosfet` with `channel = "p"` is skipped rather than solved with the wrong sign convention, so its pins stay digital endpoints.
- A part with no electrical model (buzzer, servo, display, bus device) is skipped; its pins remain digital endpoints.

The solve is deterministic. In DC mode it is a cached operating point, recomputed when a chip drive, a press, a control change or a configuration change invalidates it. In transient mode `Board::analog_transient(dt_ns)` steps one interval and `Board::analog_advance(to_ns)` steps to the chip's time; the run starts from a zero state, keeps its state across calls, and is discarded by the same invalidation rule. `Board::analog_solve` stays the DC solve; `analog_inputs()` and the node-voltage accessors read the transient solution while one is live.

**AC sweep.** `Netlist::ac_sweep` evaluates the small-signal response of the same netlist over an explicit list of frequencies (`AcRequest::Frequencies`) or a log-spaced sweep (`AcRequest::Decades { start_hz, stop_hz, points_per_decade }`). It first solves the DC operating point and linearises every nonlinear device there: a diode becomes its junction conductance `(Id + Is)/(n·Vt)`, a BJT and a MOSFET use the `gm`/`gds` derivatives the Newton solve already computed, and an op-amp becomes its finite gain — or zero gain when its DC output rests on a rail, because a saturated amplifier does not answer a small input change. Resistors are `1/R`, capacitors `jωC`, inductors `1/(jωL)` written as the branch constraint `Vp − Vq = jωL·I` (a short at 0 Hz). Frequency dependence lives only in those impedances: the device models carry no junction capacitance, no transistor charge storage and no op-amp gain-bandwidth product, and there are no transmission lines or parasitics. An `AcVoltageSource` is the stimulus (a phasor with amplitude in volts and phase in degrees; exactly 0 V at DC and in transient). A plain DC `VoltageSource` is a zero AC source, an AC source used in DC or transient is a 0 V source, and a `CurrentSource` drives its `amps` as a zero-phase AC current. Frequencies are in hertz, node voltages are complex phasors in volts, and branch currents are complex phasors in amperes. The sweep is deterministic: points are evaluated in request order, elements are assembled in insertion order, and repeated runs are bit-identical. `AcSweep::to_csv` writes a header `frequency_hz,node_1_db,node_1_phase_deg,...` followed by one row per frequency; magnitudes are `20·log10|V|` in dB relative to 1 V, phases are in degrees, every field has six decimals, and ground (node 0) has no column. The crate performs no file I/O.

## 7. Validation

Every diagnostic carries a stable code.

| Code | Severity | Meaning |
|---|---|---|
| `duplicate-id` | error | two parts share an id |
| `reserved-id` | error | a part id starts with `$` |
| `unknown-part` | error | `type` is not in the catalog |
| `unknown-part-ref` | error | a wire names a part that is not placed |
| `unknown-pin` | error | the part has no such pin |
| `pin-index` | error | `GND.3` on a part with two contacts |
| `pin-kind` | error | incompatible pin kinds |
| `rail-short` | error | two different fixed rails on one wire |
| `board-simulator` | error | a board names both `chip` and `[board.simulator]`, neither, or a module path that escapes its directory |
| `unknown-host-device` | error | `$foo` is not a host device or rail |
| `attr-type` | error | attribute value does not match its declared type |
| `unknown-attr` | warning | attribute is not declared by the catalog |
| `duplicate-wire` | warning | the same pair appears twice |

Errors block the run. Warnings do not; `--strict` turns warnings into failures, which is what a project should use in CI.

## 8. Versioning

`diagram.json` has an integer `version`. Readers accept a higher version only if every unknown key can be ignored, which is why unknown keys are preserved rather than rejected.

Catalog entries follow semver: patch for fixes, minor for additive changes (new pins, new attributes with defaults), major for anything that changes behaviour. An id may gain `aliases` in a new version; it may never be removed.

The lock file is the reproducibility boundary. A diagram plus a lock plus a firmware hash is a complete description of a run.

## 9. Security

A diagram can arrive from a pull request, so it is untrusted input.

- Catalogs and diagrams are data: parsing never executes code.
- `wasm` parts run in a sandbox with no filesystem, network, clock or randomness. They receive pin levels and time; everything else is denied.
- The runner meters fuel and memory per wasm part, so a malicious part cannot hang CI.
- Attribute values are size-limited and every string is length-checked before it reaches a model.

## 10. MCU simulator ABI

A board may name a WebAssembly component instead of a builtin chip. Two contracts are current: `litz:mcu@0.1.0` (`abi = "litz-mcu-1"`, the default) in `crates/litz-mcu-wit/wit/`, and `litz:mcu@0.2.0` (`abi = "litz-mcu-2"`) in `crates/litz-mcu-wit/wit-v2/`. Both use world `simulator`; a board manifest says which one its module was built against.

The exported `chip` interface is the same in both versions:

| Side | Item | Meaning |
|---|---|---|
| export `chip` | `info()` | name, `cpu-hz`, memory windows, flash layout |
| export `chip` | `load-image(kind, offset, bytes)` | place a ROM, bootloader, partition table or firmware |
| export `chip` | `reset(mac)`, `boot()` | start the firmware; `boot` returns its entry address |
| export `chip` | `set-inputs(pins)` / `take-outputs()` | pad levels in before a run, drives out after it |
| export `chip` | `run(budget-cycles)` | execute up to a budget, then return |
| export `chip` | `write-console(bytes)` | bytes from a host terminal into the chip's receive path |
| export `chip` | `read(addr, len)` | debug peek at the address space |

Version 0.1 imports only `host.console` and `host.fault`. Version 0.2 adds the imports a real peripheral block needs:

| Group | Host calls |
|---|---|
| analog | `analog-read(pin)` — the solved millivolts, or none |
| buses | `i2c-transfer`, `spi-transfer`, `uart-write`, `uart-read` — transactions on the board's controllers and ports |
| Wi-Fi | `wifi-scan`, `wifi-join`, `wifi-leave`, `wifi-resolve` |
| sockets | `tcp-connect`, `tcp-write`, `tcp-read`, `tcp-close`, `udp-open`, `udp-send`, `udp-read`, `udp-close` |
| BLE | `ble-scan`, `ble-advertise`, `ble-stop-advertising` |
| GATT | `ble-set-gatt`, `ble-connect`, `ble-disconnect`, `ble-discover`, `ble-read`, `ble-write`, `ble-subscribe`, `ble-notifications`, `ble-take-writes`, `ble-notify` |
| raw radio | `radio-configure`, `radio-send`, `radio-receive` — the shared nRF24/SX1276/AT86RF233 medium, with the guest supplying its own airtime |

A runtime advertisement is heard by every other 0.2 station on the board and by no one else; the native esp32sim peer list is fixed when its board boots, so a native station hears the circuit's beacon parts but not a later wasm advertisement. Making the air shared across backend families would need esp32sim to accept new peers at run time. The same limit applies to GATT: `ble-connect` reaches circuit beacons and the tables other 0.2 stations register, not an esp32sim peer. A GATT link is simulated state with no timing: connect, discover, read, write and notify answer immediately, and the peripheral side reads centrals' writes with `ble-take-writes`.

One invariant survives both versions: **one call runs a budget**. `run` is called once per few million instructions, not once per instruction, because a per-instruction boundary would cost more than the simulation. The 0.1 rule that only pins and console bytes cross is relaxed in 0.2 — a chip can start a bus transaction and join a network — but everything it reaches is still the circuit the diagram describes: bus devices are the parts the manifest attached, and every network answer comes from the deterministic router the circuit places.

The host loads the module with no WASI, no filesystem, no clock and no randomness, caps its memory, and meters it with fuel, so a chip module from a pull request cannot hang a run or reach outside its sandbox. A board whose guest carries its own program may omit `firmware` in `litz.toml`; the guest's `boot` starts whatever it has in flash.

`litz catalog new board <id>` writes a complete 0.2 guest, manifest, schematic and template README into `boards/<id>/`, and `boards/<id>/simulator/build.sh` builds the component. A guest built from that template is checked in as a test fixture, so every host call above is exercised by `cargo test -p litz-mcu-wasm`.

Nothing about the contract is Rust-specific. `wit-bindgen` generates the same guest bindings for C, C++, Go, MoonBit and Python from `wit-v2/litz-mcu.wit`; a toolchain that emits a component directly can skip the wrapper, and a core module built by any language can be wrapped with `litz-componentize` (or `wasm-tools component new`) as long as it carries the world's interface metadata. The manifest only names the `.wasm` file and the ABI it speaks.

## 11. Part simulator ABI

A `part.toml` with `backend = "wasm"` names a WebAssembly component instead of a builtin engine. The current contract is WIT package `litz:part@0.2.0`, world `component`, in `crates/litz-part-wit/wit/litz-part.wit`; `abi = "litz-part-2"` names it and is the default. `abi = "litz-part-1"` still selects the 0.1.0 package kept in `crates/litz-part-wit/wit-legacy`; the host detects the version from the component's exported interface, so a module built before 0.2 keeps running. A 0.1 module has no bus, attribute or framebuffer calls, and the host answers those with empty defaults.

Pins are indices into the part's own pin table, assigned in catalog order. The world imports two host calls and exports one `part` interface.

| Side | Item | Meaning |
|---|---|---|
| import `host` | `log(message)` | one diagnostic line, kept by the host for the run |
| import `host` | `rom()` | the image the host attached at load; empty when there is none |
| export `part` | `info()` | name, description and pin table; called once at load |
| export `part` | `set-pin(pin, level, at-ns)` | a net on this pin resolved a level at a simulated time |
| export `part` | `advance(to-ns)` / `next-event()` | the part's own timers; `next-event` returns the next tick it wants |
| export `part` | `drive(pin)` | what the pin drives now — level and strength |
| export `part` | `state()` | the `PartState` a UI or test reads back |
| export `part` | `press(pressed)` / `set-control(permille)` | host interaction, returning whether the part has that input |
| export `part` | `analog(pin)` | volts on a pin that sources one; `none` for a digital pin |
| export `part` | `attributes()` / `set-attribute(name, value)` | the part's own property table; values are `number(f64)`, `text(string)` or `flag(bool)` |
| export `part` | `i2c-connect(addr, read)` / `i2c-read()` / `i2c-write(byte)` / `i2c-stop()` | I2C target transactions |
| export `part` | `spi-select(selected)` / `spi-transfer(tx, rx-len)` | full-duplex SPI, bracketed by chip select |
| export `part` | `uart-receive(bytes, at-ns)` / `uart-poll()` | bytes from and to the chip on a UART |
| export `part` | `framebuffer()` / `draw(offset, pixels)` / `damage()` | RGBA8888 framebuffer geometry, uploads and pending damage |

Levels are `low`, `high` and `floating`; strength is `strong` or `weak`. There is no time base inside the module: `at-ns` and `to-ns` are the only clock it sees. The host loads it with no WASI, caps its memory and meters every call with fuel, so a part module from a pull request cannot hang a run or reach outside its sandbox.

**ROM.** A component gets read-only load-time contents only through `host.rom()`; the host supplies them with `WasmPart::from_bytes_with_rom`, before instantiation. The 24C02 guest reads them from `info()` and seeds its array. There is no separate `load-rom` call, so a guest cannot rewrite the image at run time.

**Buses.** When a manifest declares `[buses.i2c0]` (or `spi`/`uart`), a wasm part is built by `devices::build` rather than as a pin-level part; its power and ground pins remain passive endpoints. The I2C write/read split follows the host controller: `i2c-connect(addr, read)` opens a phase, every byte is one call, and `i2c-stop` ends the transaction, including before a repeated start. `i2c-write` returning `false` nacks the byte. `spi-transfer` must return exactly `rx-len` bytes; the host pads a short answer with `0xff`, the level a floating MISO line reads.

**Framebuffer.** `framebuffer()` returns `none` for anything that is not a display, otherwise `{ width, height }` in pixels. `draw(offset, pixels)` uploads RGBA8888 bytes at a byte offset. Each upload widens a pending damage rectangle; `damage()` returns that rectangle and clears it, so a front end can transfer only the changed region. A display that is also an I2C device is attached like any other target.

**Attributes.** `attributes()` enumerates `{ name, description, value }`; `set-attribute` writes one. The host does not invent names, and a part ignores a name it does not know, because the contract has no error channel for that.

The reference guests are `crates/litz-part-74hc595` (the original 0.1.0 shift register, which is also the compatibility proof), `crates/litz-part-eeprom` (`litz-24c02`, a 256-byte I2C EEPROM at 0x50) and `crates/litz-part-display` (`litz-rgb-panel`, an 8x8 RGBA framebuffer at 0x3c). Guest sources use `crates/litz-part-sdk`, which generates the bindings and re-exports `Guest`, `export!` and the host helpers. Each guest's `build.sh` compiles for `wasm32-unknown-unknown` and wraps the core module with `litz-componentize`; the resulting components are committed in `crates/litz-catalog/builtin/parts/`.

The reference MCU implementation is `crates/litz-chip-rv32`: RV32IMC plus enough of the ESP32-C3 memory map (GPIO, UART0, IRAM/DRAM) to run the firmware this repository generates. `crates/litz-chip-rv32/build.sh` rebuilds it. Custom chips run under the native runner and the CLI; the browser page is itself a wasm module and cannot load a second component, so `web/` keeps the builtin backend.
