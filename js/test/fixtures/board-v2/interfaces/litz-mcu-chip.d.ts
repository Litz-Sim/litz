/** @module Interface litz:mcu/chip@0.2.0 **/
export function info(): ChipInfo;
/**
 * @throws {Error & { payload: string }} The error of the result, as the `payload` of the thrown error.
 */
export function loadImage(kind: Image, offset: bigint, bytes: Uint8Array): void;
export function reset(mac: Uint8Array): void;
/**
 * @throws {Error & { payload: string }} The error of the result, as the `payload` of the thrown error.
 */
export function boot(): number;
export function setInputs(inputs: Array<PinInput>): void;
export function run(budgetCycles: bigint): RunResult;
export function takeOutputs(): Array<PinOutput>;
export function writeConsole(bytes: Uint8Array): void;
/**
 * @throws {Error & { payload: string }} The error of the result, as the `payload` of the thrown error.
 */
export function read(addr: number, len: number): Uint8Array;
export type ChipInfo = import('./litz-mcu-types.js').ChipInfo;
export type Image = import('./litz-mcu-types.js').Image;
export type PinInput = import('./litz-mcu-types.js').PinInput;
export type PinOutput = import('./litz-mcu-types.js').PinOutput;
export type RunResult = import('./litz-mcu-types.js').RunResult;
