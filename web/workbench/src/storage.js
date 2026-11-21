// Save and open through the File System Access API when the browser has it,
// and through a download and a file input otherwise. Nothing here reads a
// path out of a document: the only paths are the ones the user picks.

const JSON_ACCEPT = ".litz,.json,application/json";

export function supportsFileSystemAccess() {
  return (
    typeof window !== "undefined" &&
    typeof window.showSaveFilePicker === "function" &&
    typeof window.showOpenFilePicker === "function"
  );
}

/** A link download; the fallback for every browser without the picker API. */
export function downloadText(filename, text, mime = "application/json") {
  const blob = new Blob([text], { type: mime });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.rel = "noopener";
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}

async function writeHandle(handle, text) {
  const writable = await handle.createWritable();
  try {
    await writable.write(text);
  } finally {
    await writable.close();
  }
}

/**
 * Save `text`. Reuses `handle` when the caller already has one; otherwise
 * asks for a location, or downloads the file.
 *
 * Returns `{cancelled, method, handle, name}`. `method` is `"file"` or
 * `"download"`.
 */
export async function saveText({
  suggestedName,
  text,
  handle = null,
  types = [{ description: "JSON", accept: { "application/json": [".json"] } }],
  mime = "application/json",
}) {
  if (handle) {
    await writeHandle(handle, text);
    return { cancelled: false, method: "file", handle, name: handle.name ?? suggestedName };
  }
  if (supportsFileSystemAccess()) {
    try {
      const picked = await window.showSaveFilePicker({ suggestedName, types });
      await writeHandle(picked, text);
      return { cancelled: false, method: "file", handle: picked, name: picked.name ?? suggestedName };
    } catch (error) {
      if (error?.name === "AbortError") return { cancelled: true, method: "file", handle: null };
      throw error;
    }
  }
  downloadText(suggestedName, text, mime);
  return { cancelled: false, method: "download", handle: null, name: suggestedName };
}

async function pickWithInput(accept) {
  return new Promise((resolve, reject) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    input.hidden = true;
    const finish = (value) => {
      input.remove();
      resolve(value);
    };
    input.addEventListener(
      "change",
      async () => {
        const file = input.files?.[0];
        if (!file) {
          finish(null);
          return;
        }
        try {
          finish({ name: file.name, text: await file.text(), handle: null });
        } catch (error) {
          input.remove();
          reject(error);
        }
      },
      { once: true },
    );
    input.addEventListener("cancel", () => finish(null), { once: true });
    document.body.append(input);
    input.click();
  });
}

/**
 * Ask for a JSON file. Returns `{name,text,handle}` or `null` when the user
 * cancels. The File System Access API preserves a handle so Save can write
 * back to the same file.
 */
export async function openText({
  accept = JSON_ACCEPT,
  types = [{ description: "JSON", accept: { "application/json": [".litz", ".json"] } }],
} = {}) {
  if (supportsFileSystemAccess()) {
    let handles;
    try {
      handles = await window.showOpenFilePicker({ multiple: false, types });
    } catch (error) {
      if (error?.name === "AbortError") return null;
      throw error;
    }
    const handle = handles?.[0];
    if (!handle) return null;
    const file = await handle.getFile();
    return { name: file.name, text: await file.text(), handle };
  }
  return pickWithInput(accept);
}