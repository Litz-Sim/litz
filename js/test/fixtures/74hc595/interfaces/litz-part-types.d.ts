/** @module Interface litz:part/types@0.1.0 **/
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
/**
 * # Variants
 * 
 * ## `"strong"`
 * 
 * ## `"weak"`
 */
export type Strength = "strong" | "weak";
export interface Drive {
  level: Level,
  strength: Strength,
}
export interface PartInfo {
  name: string,
  description: string,
  pins: Array<string>,
}
export interface Tone {
  sounding: boolean,
  hz: number,
}
export type PartState = PartStateLed | PartStateButton | PartStateSwitch | PartStateSegments | PartStateTone | PartStateServo | PartStateKnob | PartStateNone;
export interface PartStateLed {
  tag: 'led',
  val: boolean,
}
export interface PartStateButton {
  tag: 'button',
  val: boolean,
}
export interface PartStateSwitch {
  tag: 'switch',
  val: boolean,
}
export interface PartStateSegments {
  tag: 'segments',
  val: number,
}
export interface PartStateTone {
  tag: 'tone',
  val: Tone,
}
export interface PartStateServo {
  tag: 'servo',
  val: number,
}
export interface PartStateKnob {
  tag: 'knob',
  val: number,
}
export interface PartStateNone {
  tag: 'none',
}
