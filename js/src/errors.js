// The one error type the library throws.

export class LitzError extends Error {
  constructor(message, options) {
    super(message, options);
    this.name = "LitzError";
  }
}

