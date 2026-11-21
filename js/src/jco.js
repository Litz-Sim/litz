// Adapters from `@bytecodealliance/jco` output to the host interfaces this
// package drives.
//
// A component from the Litz catalog is a Component Model binary. Browsers
// cannot instantiate one, so it is transpiled once with
// `jco transpile --instantiation sync`, which emits a core module plus a JS
// module exporting `instantiate(getCoreModule, imports)`. That function
// returns the guest's interfaces; these adapters translate between their
// shapes and the plain objects `createSession` expects.
//
// Nothing here depends on jco at run time: the transpiled output is the input.

/** Build the `imports` object a transpiled `litz:part` component expects. */
export function partImports({ log = () => {}, rom = () => new Uint8Array(0) } = {}) {
  // jco reads the import object by package name without the version, which
  // is the key the generated glue actually indexes.
  return {
    "litz:part/host": { log, rom },
    "litz:part/types": {},
  };
}

/**
 * Build the `imports` object a transpiled `litz:mcu@0.1.0` or `0.2.0`
 * component expects.
 *
 * Every 0.2 call goes to the circuit through `host.call`, the bridge's one
 * JSON door into the module; there is no host-side logic here beyond turning
 * the contract's records into plain objects. A WIT `result<_, string>` error
 * has to be thrown with a `payload` property, which is how jco tells a
 * declared error from a trap.
 */
export function chipImports(host) {
  const call = (op, args) => host.call(op, args);
  const result = (op, args) => {
    try {
      return call(op, args);
    } catch (error) {
      const failed = new Error(error?.message ?? String(error));
      failed.payload = failed.message;
      throw failed;
    }
  };
  const list = (bytes) => Array.from(bytes ?? []);
  const bytes = (value) => Uint8Array.from(value ?? []);
  const optional = (value) => (value === null || value === undefined ? undefined : value);
  const services = (value) =>
    (value ?? []).map((service) => ({
      uuid: service.uuid,
      characteristics: (service.characteristics ?? []).map((characteristic) => ({
        uuid: characteristic.uuid,
        access: characteristic.access,
        value: bytes(characteristic.value),
      })),
    }));
  const notifications = (value) =>
    (value ?? []).map((notification) => ({
      service: notification.service,
      characteristic: notification.characteristic,
      value: bytes(notification.value),
    }));

  return {
    "litz:mcu/host": {
      console: (data) => host.console(data),
      fault: (detail) => host.fault(detail),
      analogRead: (pin) => optional(call("analogRead", { pin })),
      i2cTransfer: (controller, address, write, readLen) =>
        bytes(result("i2cTransfer", { controller, address, write: list(write), readLen })),
      spiTransfer: (controller, chipSelect, tx, readLen) =>
        bytes(result("spiTransfer", { controller, chipSelect, tx: list(tx), readLen })),
      uartWrite: (port, data) => call("uartWrite", { port, bytes: list(data) }),
      uartRead: (port) => bytes(call("uartRead", { port })),
      wifiScan: () => call("wifiScan", {}),
      wifiJoin: (ssid, psk) => {
        const lease = result("wifiJoin", { ssid, psk: optional(psk) ?? null });
        return {
          ip: bytes(lease.ip),
          gateway: bytes(lease.gateway),
          dns: bytes(lease.dns),
        };
      },
      wifiLeave: () => call("wifiLeave", {}),
      wifiResolve: (name) => bytes(result("wifiResolve", { name })),
      tcpConnect: (address, port) => result("tcpConnect", { host: list(address), port }),
      tcpWrite: (id, data) => result("tcpWrite", { id, bytes: list(data) }),
      tcpRead: (id, max) => bytes(result("tcpRead", { id, max })),
      tcpClose: (id) => call("tcpClose", { id }),
      udpOpen: (port) => result("udpOpen", { port }),
      udpSend: (id, address, port, data) =>
        result("udpSend", { id, host: list(address), port, bytes: list(data) }),
      udpRead: (id, max) => bytes(result("udpRead", { id, max })),
      udpClose: (id) => call("udpClose", { id }),
      bleScan: () =>
        call("bleScan", {}).map((beacon) => ({
          address: bytes(beacon.address),
          name: optional(beacon.name),
          rssi: beacon.rssi,
          data: bytes(beacon.data),
          scanResponse: bytes(beacon.scanResponse),
        })),
      bleAdvertise: (address, data, scanResponse) =>
        bytes(
          result("bleAdvertise", {
            address: list(address),
            data: list(data),
            scanResponse: list(scanResponse),
          }),
        ),
      bleStopAdvertising: (address) => call("bleStopAdvertising", { address: list(address) }),
      bleSetGatt: (table) =>
        result("bleSetGatt", {
          services: (table ?? []).map((service) => ({
            uuid: service.uuid,
            characteristics: (service.characteristics ?? []).map((characteristic) => ({
              uuid: characteristic.uuid,
              access: characteristic.access,
              value: list(characteristic.value),
            })),
          })),
        }),
      bleConnect: (address) => result("bleConnect", { address: list(address) }),
      bleDisconnect: (handle) => call("bleDisconnect", { handle }),
      bleDiscover: (handle) => services(result("bleDiscover", { handle })),
      bleRead: (handle, service, characteristic) =>
        bytes(result("bleRead", { handle, service, characteristic })),
      bleWrite: (handle, service, characteristic, value) =>
        result("bleWrite", { handle, service, characteristic, value: list(value) }),
      bleSubscribe: (handle, service, characteristic, enabled) =>
        result("bleSubscribe", { handle, service, characteristic, enabled }),
      bleNotifications: (handle) => notifications(call("bleNotifications", { handle })),
      bleTakeWrites: () =>
        call("bleTakeWrites", {}).map((write) => ({
          service: write.service,
          characteristic: write.characteristic,
          value: bytes(write.value),
        })),
      bleNotify: (service, characteristic, value) =>
        result("bleNotify", { service, characteristic, value: list(value) }),
      radioConfigure: (network, channel, address) =>
        result("radioConfigure", { network, channel, address: list(address) }),
      radioSend: (data, to, airtimeNs) =>
        result("radioSend", {
          bytes: list(data),
          to: to === undefined ? null : list(to),
          airtimeNs: Number(airtimeNs),
        }),
      radioReceive: () =>
        call("radioReceive", {}).map((frame) => ({
          sender: bytes(frame.sender),
          to: optional(frame.to) === undefined ? undefined : bytes(frame.to),
          bytes: bytes(frame.bytes),
          sentAt: BigInt(frame.sentAt),
          airtime: BigInt(frame.airtime),
        })),
    },
    "litz:mcu/types": {},
  };
}

/**
 * Wrap the root object a transpiled part's `instantiate` returned.
 *
 * `root` is the whole return value, not just its `part` key, because the
 * interface is also exported under its versioned name.
 */
export function partFromJco(root) {
  const part = root.part ?? root["litz:part/part@0.2.0"] ?? root["litz:part/part@0.1.0"];
  if (!part) throw new TypeError("the transpiled module exports no litz:part interface");
  const at = (ns) => BigInt(Math.round(ns));
  return {
    info: () => part.info(),
    setPin: (pin, level, atNs) => part.setPin(pin, level, at(atNs)),
    advance: (toNs) => part.advance(at(toNs)),
    nextEvent: () => {
      const event = part.nextEvent();
      return event === undefined || event === null ? null : Number(event);
    },
    drive: (pin) => part.drive(pin),
    state: () => partState(part.state()),
    press: (pressed) => part.press(pressed),
    setControl: (permille) => part.setControl(permille),
    analog: (pin) => {
      const volts = part.analog?.(pin);
      return volts === undefined ? null : volts;
    },
    i2cConnect: (address, read) => part.i2cConnect?.(address, read),
    i2cRead: () => part.i2cRead?.() ?? 0xff,
    i2cWrite: (byte) => part.i2cWrite?.(byte) ?? false,
    i2cStop: () => part.i2cStop?.(),
    spiSelect: (selected) => part.spiSelect?.(selected),
    spiTransfer: (tx, rxLen) => part.spiTransfer?.(tx, rxLen) ?? new Uint8Array(rxLen),
    uartReceive: (bytes, atNs) => part.uartReceive?.(bytes, at(atNs)),
    uartPoll: () => part.uartPoll?.() ?? new Uint8Array(0),
  };
}

/**
 * Wrap the root object a transpiled chip's `instantiate` returned.
 *
 * The guest's `host` import is answered by the adapter given in `imports`;
 * this function only translates the chip interface.
 */
export function chipFromJco(root) {
  const chip = root.chip ?? root["litz:mcu/chip@0.2.0"] ?? root["litz:mcu/chip@0.1.0"];
  if (!chip) throw new TypeError("the transpiled module exports no litz:mcu interface");
  return {
    info: () => {
      const info = chip.info();
      return {
        name: info.name,
        description: info.description,
        cpuHz: Number(info.cpuHz),
        flash: {
          size: Number(info.flash.size),
          bootloader: Number(info.flash.bootloader),
          partitionTable: Number(info.flash.partitionTable),
          app: Number(info.flash.app),
        },
      };
    },
    loadImage: (kind, offset, bytes) => chip.loadImage(kind, BigInt(Math.round(offset)), bytes),
    boot: () => chip.boot(),
    setInputs: (inputs) => chip.setInputs(inputs),
    run: (budget) => {
      const result = chip.run(BigInt(Math.round(budget)));
      return { cycles: Number(result.cycles), stop: result.stop };
    },
    takeOutputs: () => chip.takeOutputs(),
    writeConsole: (bytes) => chip.writeConsole(bytes),
  };
}

/** `litz:part/types@0.2.0`'s tagged `part-state` as the bridge's kind/value. */
function partState(state) {
  switch (state?.tag) {
    case "led":
      return { kind: state.val ? "led-on" : "led-off" };
    case "button":
      return { kind: state.val ? "button-down" : "button-up" };
    case "switch":
      return { kind: state.val ? "switch-on" : "switch-off" };
    case "tone":
      return {
        kind: state.val.sounding ? "tone-sounding" : "tone-silent",
        value: state.val.hz,
      };
    case "segments":
      return { kind: "segments", value: state.val };
    case "servo":
      return { kind: "servo", value: state.val };
    case "knob":
      return { kind: "knob", value: state.val };
    default:
      return { kind: "none" };
  }
}
