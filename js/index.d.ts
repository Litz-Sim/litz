/** Loaded Litz module, ready to open sessions. */
export declare class Litz {
  private constructor();
  /** The raw wasm export object. */
  readonly exports: WebAssembly.Exports;
  /** The instance this runtime was built from. */
  readonly instance: WebAssembly.Instance;
  /** Open a session on one board. */
  createSession(options: SessionOptions): Session;
  /** Release module memory. Sessions must be closed first. */
  close(): void;
}

export interface SessionOptions {
  /** The diagram.json text. The boards it names select the chips. */
  diagram: string;
  /** The application image for a single-board diagram. */
  app?: Uint8Array | ArrayBuffer | ArrayBufferView;
  /** One image per development board; replaces `app` in a multi-board diagram. */
  boardImages?: BoardImage[];
  /** Mask ROM, when the firmware needs one. */
  rom?: Uint8Array | ArrayBuffer | ArrayBufferView | null;
  /** Second-stage bootloader; requires `ptable`. */
  boot?: Uint8Array | ArrayBuffer | ArrayBufferView | null;
  /** Partition table; requires `boot`. */
  ptable?: Uint8Array | ArrayBuffer | ArrayBufferView | null;
  /** Flash size in bytes; the board manifest's, then 4 MiB. */
  flashSize?: number | null;
  /** The MAC the station reports, e.g. `24:0a:c4:12:34:56`. */
  mac?: string | null;
  /** Solve the analog network instead of treating every net as digital. */
  analog?: boolean;
  /** Catalog manifests as TOML text: custom parts and boards. */
  manifests?: string[];
  /** Wasm part modules: `{ "x.wasm": () => instance }`. */
  parts?: Record<string, PartFactory>;
  /** Wasm chip modules: `{ "x.wasm": (host) => instance }`. */
  boards?: Record<string, ChipFactory>;
}

/** One board's images in a multi-board session. */
export interface BoardImage {
  /** The `diagram.json` instance id; omitted entries match boards by position. */
  id?: string;
  app: Uint8Array | ArrayBuffer | ArrayBufferView;
  rom?: Uint8Array | ArrayBuffer | ArrayBufferView | null;
  boot?: Uint8Array | ArrayBuffer | ArrayBufferView | null;
  ptable?: Uint8Array | ArrayBuffer | ArrayBufferView | null;
  flashSize?: number | null;
  mac?: string | null;
}

/** One instantiated `litz:part` component, as the simulator drives it. */
export interface HostPart {
  setPin(pin: number, level: Level, atNs: number): void;
  advance(toNs: number): void;
  nextEvent(): number | null | undefined;
  drive(pin: number): { level: Level; strength?: "strong" | "weak" } | Level | null;
  state(): { kind: string; value?: number } | null;
  press?(pressed: boolean): boolean;
  setControl?(permille: number): boolean;
  analog?(pin: number): number | null;
  i2cConnect?(address: number, read: boolean): void;
  i2cRead?(): number;
  i2cWrite?(byte: number): boolean;
  i2cStop?(): void;
  spiSelect?(selected: boolean): void;
  spiTransfer?(tx: Uint8Array, rxLen: number): Uint8Array;
  uartReceive?(bytes: Uint8Array, atNs: number): void;
  uartPoll?(): Uint8Array;
}

/** One instantiated `litz:mcu` component. */
export interface HostChip {
  info(): {
    name: string;
    description?: string;
    cpuHz: number;
    flash: { size: number; bootloader: number; partitionTable: number; app: number };
  };
  loadImage(kind: "rom" | "bootloader" | "partition-table" | "firmware", offset: number, bytes: Uint8Array): void;
  boot(): number;
  setInputs(inputs: Array<{ pin: number; level: Level }>): void;
  run(budget: number): { cycles: number; stop: StopWord };
  takeOutputs(): Array<{ pin: number; level: Level; enabled: boolean }>;
  writeConsole(bytes: Uint8Array): void;
}

/** The guest's host imports. */
export interface ChipHost {
  console(bytes: string | Uint8Array): void;
  fault(detail: string): void;
  /**
   * Answer one `litz:mcu@0.2.0` host call against the circuit. `op` is the
   * contract's camelCase name and `args` its JSON argument object; the result
   * is the call's plain JSON value. A call the circuit refuses throws
   * `LitzError`.
   */
  call(op: string, args: Record<string, unknown>): unknown;
}

export type PartFactory = () => HostPart;
export type ChipFactory = (host: ChipHost) => HostChip;

export interface ChipFlash {
  readonly size: number;
  readonly bootloader: number;
  readonly partitionTable: number;
  readonly app: number;
}

export interface ChipInfo {
  /** `esp32c3`, `esp32s3`, `atmega328p`, `rp2040`, ... */
  readonly name: string;
  readonly cpuHz: number;
  readonly flash: ChipFlash;
}

/** How the last `run` ended. */
export type StopWord = "budget" | "idle" | "halted" | "trap" | "reset" | "breakpoint";

/** A pin level as the board sees it. */
export type Level = "low" | "high" | "high-z";

export interface LoadOptions {
  /** Bytes, a URL, a `Response`, or an already compiled module. */
  wasm?: Uint8Array | ArrayBuffer | ArrayBufferView | string | URL | Response | WebAssembly.Module | null;
  /** Extra imports, merged alongside this package's `litz_host` bridge. */
  imports?: WebAssembly.Imports;
}

/** Where the shipped wasm file sits. */
export declare const wasmUrl: string;

/** Scheduling steps per call when the host does not choose. */
export declare const DEFAULT_STEPS: number;

/** Load the module. Without `wasm`, the file next to the module is used. */
export declare function loadLitz(options?: LoadOptions): Promise<Litz>;

export declare class LitzError extends Error {}

/** Wrap a jco-transpiled `litz:part` root object. */
export declare function partFromJco(root: unknown): HostPart;
/** Wrap a jco-transpiled `litz:mcu` root object. */
export declare function chipFromJco(root: unknown): HostChip;
/** The imports a transpiled `litz:part@0.2.0` module expects. */
export declare function partImports(options?: {
  log?: (message: string) => void;
  rom?: () => Uint8Array;
}): Record<string, unknown>;
/** The imports a transpiled `litz:mcu` (0.1 or 0.2) module expects. */
export declare function chipImports(host: ChipHost): Record<string, unknown>;

export type PartStateName =
  | "none"
  | "off"
  | "on"
  | "up"
  | "down"
  | "silent"
  | "sounding"
  | "position"
  | "mask"
  | "state"
  | "unknown";

export interface PartState {
  /** The id the diagram gave the part. */
  id: string;
  /** The ABI's state code: 0 none, 1 off, 2 on, 3 up, 4 down. */
  code: number;
  /** What the part is: `led`, `button`, `switch`, `servo`, `radio`, ... */
  kind: string;
  /** `code` as a word; `null` for a part that reports nothing (a board). */
  state: PartStateName | null;
  /** The number behind the state: servo degrees, a segment mask, a tone rate. */
  value: number;
}

export interface Display {
  readonly width: number;
  readonly height: number;
  /** Grows when the pixels change, so a host can skip an unchanged frame. */
  readonly version: number;
  /** The frame as RGBA8888, `width * height * 4` bytes. */
  pixels(): Uint8Array;
}

export interface Serial {
  /** Drain what the boards printed since the last call, merged in board order. */
  take(): string;
  /** Drain one board's console, by list index or board id. */
  takeBoard(board: number | string): string;
  /** Deliver bytes to the first board's UART0 receiver. */
  write(data: string | Uint8Array | ArrayBuffer | ArrayBufferView): void;
  /** Deliver bytes to one board's UART0 receiver. */
  writeBoard(
    board: number | string,
    data: string | Uint8Array | ArrayBuffer | ArrayBufferView,
  ): void;
}

/** One development board the session booted. */
export interface BoardInfo extends ChipInfo {
  readonly index: number;
  readonly id: string;
}

/** The browser-facing debugger over one RISC-V board. */
export interface Debug {
  available(board?: number | string): boolean;
  pc(board?: number | string): number | null;
  registers(board?: number | string): { values: number[]; pc: number } | null;
  memory(board: number | string, addr: number, length: number): Uint8Array;
  disassemble(board: number | string, addr: number, count?: number): string;
  setBreakpoints(board: number | string, pcs: ArrayLike<number>): void;
  step(board?: number | string): StopWord | "unsupported";
  run(board?: number | string, steps?: number): StopWord | "unsupported";
}

export interface Trace {
  /** Begin recording net transitions. */
  start(): void;
  /** Recorded transitions so far. */
  readonly transitions: number;
  /** The recording as a VCD document. */
  vcd(): string;
}

export interface Probes {
  /**
   * Watch a point: `<board>:<GPIO>` (`esp:GPIO8`) or `<part>:<pin>`
   * (`led1:A`). Samples appear in `session.scope()`.
   */
  add(name: string, point: string): void;
}

export interface Decoders {
  /** `uart:rx=<probe>[,baud=N]`, `i2c:sda=<probe>,scl=<probe>`,
   * `spi:sck=<probe>,mosi=<probe>,cs=<probe>[,miso=<probe>][,mode=N]`. */
  add(spec: string): void;
}

export interface ProbeRecord {
  name: string;
  point: string;
  /** Net the probe resolves to on the flattened board. */
  net: number;
  /** Level transitions as `[tick_ns, level]`, level 0 low, 1 high, 2 high-Z. */
  digital: Array<[number, number]>;
  /** Voltage samples as `[tick_ns, volts]`. */
  volts: Array<[number, number]>;
  edges: number;
  high_ns: number;
  low_ns: number;
  frequency_hz: number | null;
  duty: number | null;
  min_volts: number | null;
  max_volts: number | null;
}

export interface DecodeRecord {
  spec: string;
  kind: string;
  name: string;
  frames: Array<{ at: number; end: number; text: string; error: boolean | string }>;
}

export interface ScopeDocument {
  /** Simulated ms at the time of the call. */
  ms: number;
  probes: ProbeRecord[];
  decodes: DecodeRecord[];
}

export interface AdvanceOptions {
  /** Steps per inner `run` call. */
  chunkSteps?: number;
  /** Give up after this many steps. */
  maxSteps?: number;
}

/** One running board. */
export declare class Session {
  private constructor();
  /** The handle inside the module. */
  readonly handle: number;
  /** Advance by at most `steps` scheduling steps; returns the simulated ms. */
  run(steps?: number): number;
  /** Advance until `ms` of simulated time have passed. */
  advance(ms: number, options?: AdvanceOptions): number;
  /** Simulated milliseconds since reset. */
  readonly ms: number;
  /** The backend the diagram booted, its clock and its flash layout. */
  chip(): ChipInfo;
  /** Every development board the session booted, in diagram order. */
  boards(): BoardInfo[];
  /** One board by list index or `diagram.json` id. */
  board(board?: number | string): BoardInfo;
  /** How the last `run` ended. */
  readonly stop: StopWord;
  /** The backend's explanation for a trap, or `null`. */
  readonly fault: string | null;
  /** The firmware's own free heap in bytes, or `null` when it cannot be asked. */
  heap(): number | null;
  readonly serial: Serial;
  readonly trace: Trace;
  readonly probe: Probes;
  readonly decode: Decoders;
  /** Electrical conflicts, oldest first. */
  conflicts(): string[];
  /** Every part the diagram declares. */
  parts(): PartState[];
  /** One part by id, including board-expanded ids such as `esp/led_pwr`. */
  part(id: string): PartState | null;
  /** Press or release a pushbutton wired to a GPIO. */
  press(id: string, pressed?: boolean): void;
  /** Move a control: 0 to 1000 permille of its travel. */
  setControl(id: string, permille: number): void;
  /** Set a named control such as a sensor's `temperature` or `lux`. */
  setNamedControl(id: string, name: string, value: string | number): void;
  /** The level on one of a part's pins, or `null` when it does not exist. */
  partPin(id: string, pin?: string): Level | null;
  /** The level the chip sees on its own pad, by GPIO number. */
  gpio(pin: number): Level | null;
  /** The level one board sees on one of its own pads. */
  gpioOn(board: number | string, pin: number): Level | null;
  /** The panel, or `null` when the circuit has no display. */
  display(): Display | null;
  /** One board's panel, by list index or board id. */
  displayOn(board?: number | string): Display | null;
  /** Deliver a touch at panel pixels. False without a panel. */
  touch(x: number, y: number, down?: boolean): boolean;
  /** Probes' samples and decoders' frames. */
  scope(): ScopeDocument;
  /** Registers, memory, breakpoints and single-step. */
  readonly debug: Debug;
  /** Drop the session. */
  close(): void;
}
