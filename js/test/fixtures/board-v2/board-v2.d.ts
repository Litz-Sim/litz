// world root:component/root
import type * as LitzMcuHost from './interfaces/litz-mcu-host.js'; // litz:mcu/host@0.2.0
import type * as LitzMcuTypes from './interfaces/litz-mcu-types.js'; // litz:mcu/types@0.2.0
import type * as LitzMcuChipExports from './interfaces/litz-mcu-chip.js'; // litz:mcu/chip@0.2.0
export interface ImportObject {
  "litz:mcu/host@0.2.0": typeof LitzMcuHost,
  "litz:mcu/types@0.2.0": typeof LitzMcuTypes,
}
export interface Root {
  "litz:mcu/chip@0.2.0": typeof LitzMcuChipExports,
  chip: typeof LitzMcuChipExports,
  /** Permanently stop this instance and dispose guest-owned host resources. */
  readonly _util: { stop(): void; whenIdle(): Promise<void>;  /** Test whether a value is a declared WIT result error from this instance. */
  isComponentError(value: unknown): value is Error & { readonly payload: unknown }; };
}

/**
* Options accepted by `instantiate`.
*/
export interface InstantiateOptions {
  /**
  * Called with each trap of this instance as it happens, in whichever
  * task: including one a guest left running, which no call of the host's
  * would otherwise fail for. Not called for stopping the instance, which
  * it may do itself.
  */
  onTrap?: (error: WebAssembly.RuntimeError) => void;
  /** Runtime overrides for this instance. */
  shim?: {
    /** Platform globals used instead of those on `globalThis`. */
    globals?: {
      /**
      * The WebAssembly implementation used by this instance,
      * including by the default `instantiateCore`. Defaults to
      * `globalThis.WebAssembly`.
      */
      WebAssembly?: typeof WebAssembly;
    };
  };
}


/**
* Instantiates this component with the provided imports and
* returns a map of all the exports of the component.
*
* This function is intended to be similar to the
* `WebAssembly.Instantiate` constructor. The second `imports`
* argument is the "import object" for wasm, except here it
* uses component-model-layer types instead of core wasm
* integers/numbers/etc.
*
* The first argument to this function, `getCoreModule`, is
* used to compile core wasm modules within the component.
* Components are composed of core wasm modules and this callback
* will be invoked per core wasm module. The caller of this
* function is responsible for reading the core wasm module
* identified by `path` and returning its compiled
* `WebAssembly.Module` object. This would use the
* `WebAssembly.Module` constructor on the web, for example.
*
* The optional `options` argument configures instantiation; see
* `InstantiateOptions`.
*/
export function instantiate(
getCoreModule: (path: string) => WebAssembly.Module,
imports: ImportObject,
instantiateCore?: (module: WebAssembly.Module, imports: Record<string, any>) => WebAssembly.Instance,
options?: InstantiateOptions
): Root;

