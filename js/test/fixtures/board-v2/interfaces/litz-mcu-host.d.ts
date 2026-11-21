/** @module Interface litz:mcu/host@0.2.0 **/
export function console(bytes: Uint8Array): void;
export function analogRead(pin: number): number | undefined;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function i2cTransfer(controller: number, address: number, write: Uint8Array, readLen: number): Uint8Array;
export function uartWrite(port: number, bytes: Uint8Array): boolean;
export function wifiScan(): Array<WifiNetwork>;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function wifiJoin(ssid: string, psk: string | undefined): WifiLease;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function tcpConnect(host: Uint8Array, port: number): number;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function tcpWrite(id: number, bytes: Uint8Array): number;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function tcpRead(id: number, max: number): Uint8Array;
export function tcpClose(id: number): void;
export function bleScan(): Array<BleAdvertisement>;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function bleAdvertise(address: Uint8Array, data: Uint8Array, scanResponse: Uint8Array): Uint8Array;
export function bleStopAdvertising(address: Uint8Array): void;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function bleSetGatt(services: Array<GattService>): void;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function bleConnect(address: Uint8Array): number;
export function bleDisconnect(handle: number): void;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function bleDiscover(handle: number): Array<GattService>;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function bleRead(handle: number, service: number, characteristic: number): Uint8Array;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function bleWrite(handle: number, service: number, characteristic: number, value: Uint8Array): void;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function bleSubscribe(handle: number, service: number, characteristic: number, enabled: boolean): void;
export function bleNotifications(handle: number): Array<GattNotification>;
export function bleTakeWrites(): Array<GattWrite>;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function bleNotify(service: number, characteristic: number, value: Uint8Array): void;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function radioConfigure(network: string, channel: number, address: Uint8Array): void;
/**
 * @throws {string} To return the error of the result, throw it, or an `Error` that has it as its `payload`.
 */
export function radioSend(bytes: Uint8Array, to: Uint8Array | undefined, airtimeNs: bigint): void;
export function radioReceive(): Array<RadioFrame>;
export type GattService = import('./litz-mcu-types.js').GattService;
export type RadioFrame = import('./litz-mcu-types.js').RadioFrame;
export type GattWrite = import('./litz-mcu-types.js').GattWrite;
export type GattNotification = import('./litz-mcu-types.js').GattNotification;
export type BleAdvertisement = import('./litz-mcu-types.js').BleAdvertisement;
export type WifiLease = import('./litz-mcu-types.js').WifiLease;
export type WifiNetwork = import('./litz-mcu-types.js').WifiNetwork;
