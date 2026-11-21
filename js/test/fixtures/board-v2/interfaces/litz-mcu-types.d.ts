/** @module Interface litz:mcu/types@0.2.0 **/
export interface GattCharacteristic {
  uuid: number,
  access: number,
  value: Uint8Array,
}
export interface GattService {
  uuid: number,
  characteristics: Array<GattCharacteristic>,
}
export interface RadioFrame {
  sender: Uint8Array,
  to?: Uint8Array,
  bytes: Uint8Array,
  sentAt: bigint,
  airtime: bigint,
}
export interface GattWrite {
  service: number,
  characteristic: number,
  value: Uint8Array,
}
export interface GattNotification {
  service: number,
  characteristic: number,
  value: Uint8Array,
}
export interface BleAdvertisement {
  address: Uint8Array,
  name?: string,
  rssi: number,
  data: Uint8Array,
  scanResponse: Uint8Array,
}
export interface WifiLease {
  ip: Uint8Array,
  gateway: Uint8Array,
  dns: Uint8Array,
}
export interface WifiNetwork {
  ssid: string,
  channel: number,
  rssi: number,
  secure: boolean,
  joined: boolean,
}
export interface MemoryRegion {
  name: string,
  start: number,
  len: number,
}
export interface FlashLayout {
  size: bigint,
  bootloader: bigint,
  partitionTable: bigint,
  app: bigint,
}
export interface ChipInfo {
  name: string,
  description: string,
  cpuHz: bigint,
  memory: Array<MemoryRegion>,
  flash: FlashLayout,
}
/**
 * # Variants
 * 
 * ## `"rom"`
 * 
 * ## `"bootloader"`
 * 
 * ## `"partition-table"`
 * 
 * ## `"firmware"`
 */
export type Image = "rom" | "bootloader" | "partition-table" | "firmware";
/**
 * # Variants
 * 
 * ## `"low"`
 * 
 * ## `"high"`
 * 
 * ## `"floating"`
 */
export type Level = "low" | "high" | "floating";
export interface PinInput {
  pin: number,
  level: Level,
}
export interface PinOutput {
  pin: number,
  level: Level,
  enabled: boolean,
}
/**
 * # Variants
 * 
 * ## `"budget"`
 * 
 * ## `"idle"`
 * 
 * ## `"halted"`
 * 
 * ## `"trap"`
 * 
 * ## `"reset"`
 */
export type Stop = "budget" | "idle" | "halted" | "trap" | "reset";
export interface RunResult {
  cycles: bigint,
  stop: Stop,
}
