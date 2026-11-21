/** @module Interface litz:part/part@0.1.0 **/
export function info(): PartInfo;
export function setPin(pin: number, level: Level, atNs: bigint): void;
export function advance(toNs: bigint): void;
export function nextEvent(): bigint | undefined;
export function drive(pin: number): DriveResult;
export function state(): PartState;
export function press(pressed: boolean): boolean;
export function setControl(permille: number): boolean;
export type DriveResult = import('./litz-part-types.js').Drive;
export type Level = import('./litz-part-types.js').Level;
export type PartInfo = import('./litz-part-types.js').PartInfo;
export type PartState = import('./litz-part-types.js').PartState;
