// Values the wrapper and the module agree on.

export const DEFAULT_STEPS = 400_000;

export const REQUIRED_EXPORTS = [
  "memory",
  "litz_alloc",
  "litz_free",
  "litz_new",
  "litz_new_json",
  "litz_drop",
  "litz_last_error",
  "litz_run",
  "litz_ms",
  "litz_conflict_count",
  "litz_conflict",
  "litz_serial_take",
  "litz_serial_write",
  "litz_display_info",
  "litz_display_version",
  "litz_display_rgba",
  "litz_part_display_info",
  "litz_part_display_version",
  "litz_part_display_rgba",
  "litz_storage_files_len",
  "litz_storage_files_copy",
  "litz_flash_len",
  "litz_flash_copy",
  "litz_touch",
  "litz_trace_start",
  "litz_trace_len",
  "litz_trace_vcd",
  "litz_probe_add",
  "litz_scope_len",
  "litz_scope_json",
  "litz_decode_add",
  "litz_part_state",
  "litz_part_value",
  "litz_press",
  "litz_set_control",
  "litz_part_count",
  "litz_part_id",
  "litz_chip_info",
  "litz_stop",
  "litz_fault",
  "litz_part_pin_level",
  "litz_gpio_level",
  "litz_set_named_control",
  "litz_heap",
  "litz_guest_console",
  "litz_guest_fault",
  "litz_guest_call",
  "litz_board_count",
  "litz_board_id",
  "litz_board_chip_info",
  "litz_board_gpio_level",
  "litz_serial_take_board",
  "litz_serial_write_board",
  "litz_board_display_info",
  "litz_board_display_version",
  "litz_board_display_rgba",
  "litz_debug_available",
  "litz_debug_pc",
  "litz_debug_registers",
  "litz_debug_memory",
  "litz_debug_disassemble",
  "litz_debug_breakpoints",
  "litz_debug_step",
  "litz_debug_run",
];

// State codes from `litz_part_state`; `-1` means the diagram has no such id.
// The code carries both what the part is and what it is doing, which is why
// the wrapper reports a kind as well as a word.
export const STATE_CODES = [
  { kind: "none", state: "none" },
  { kind: "led", state: "off" },
  { kind: "led", state: "on" },
  { kind: "button", state: "up" },
  { kind: "button", state: "down" },
  { kind: "switch", state: "off" },
  { kind: "switch", state: "on" },
  { kind: "tone", state: "silent" },
  { kind: "tone", state: "sounding" },
  { kind: "servo", state: "position" },
  { kind: "segments", state: "mask" },
  { kind: "knob", state: "position" },
  { kind: "radio", state: "state" },
];

// How a run ended, from `litz_stop`.
export const STOPS = ["budget", "idle", "halted", "trap", "reset", "breakpoint"];

// A pin's level, from `litz_part_pin_level` and `litz_gpio_level`.
export const LEVELS = ["low", "high", "high-z"];

export const ENCODER = new TextEncoder();
export const DECODER = new TextDecoder();

// State codes the part bridge shares with the module: the same numbering the
// rest of the ABI uses for a part's state.
export const PART_STATE_KINDS = {
  none: 0,
  "led-off": 1,
  "led-on": 2,
  "button-up": 3,
  "button-down": 4,
  "switch-off": 5,
  "switch-on": 6,
  "tone-silent": 7,
  "tone-sounding": 8,
  servo: 9,
  segments: 10,
  knob: 11,
};


export const LEVEL_WORDS = ["low", "high", "floating"];

// `litz:image` and `litz:stop`, in contract order.
export const IMAGE_KINDS = ["rom", "bootloader", "partition-table", "firmware"];
export const STOP_CODES = { budget: 0, idle: 1, halted: 2, trap: 3, reset: 4 };
