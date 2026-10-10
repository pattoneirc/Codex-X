import type { ProviderHeader } from "./providerHeaders";

export type ProviderHeadersControlState = {
  busy: boolean;
  reading: boolean;
  valid: boolean;
  error: "read" | "write" | null;
};

type Options = {
  read: (configText: string) => Promise<ProviderHeader[]>;
  update: (configText: string, headers: ProviderHeader[]) => Promise<string>;
  getConfigText: () => string;
  isValid: (rows: readonly ProviderHeader[]) => boolean;
  normalize: (rows: readonly ProviderHeader[]) => ProviderHeader[];
  onRowsChange: (rows: ProviderHeader[]) => void;
  onConfigChange: (configText: string) => void;
  onStateChange: (state: ProviderHeadersControlState) => void;
};

/** Keep local drafts and asynchronous TOML patches within the current form. */
export function createProviderHeadersControl(options: Options) {
  let disposed = false;
  let request = 0;
  let revision = 0;
  let source: string | null = null;
  let rows: ProviderHeader[] = [];
  let dirty = false;
  let published: { text: string; revision: number } | null = null;

  const emit = (state: ProviderHeadersControlState) => {
    if (!disposed) options.onStateChange(state);
  };
  const current = (id: number, text: string) => !disposed && request === id && options.getConfigText() === text;
  const idle = (valid: boolean) => emit({ busy: false, reading: false, valid, error: null });

  async function read(text: string) {
    const id = ++request;
    emit({ busy: true, reading: true, valid: false, error: null });
    try {
      const result = await options.read(text);
      if (!current(id, text)) return;
      rows = result.map((row) => ({ ...row }));
      options.onRowsChange(rows);
      idle(options.isValid(rows));
    } catch {
      // IPC errors can contain configuration snippets. Only expose the failed
      // operation, never the raw exception or a resolved environment value.
      if (current(id, text)) emit({ busy: false, reading: false, valid: false, error: "read" });
    }
  }

  async function write(text: string) {
    const id = ++request;
    const editedRevision = revision;
    const headers = options.normalize(rows);
    emit({ busy: true, reading: false, valid: true, error: null });
    try {
      const result = await options.update(text, headers);
      if (!current(id, text)) return;
      dirty = false;
      if (result !== text) {
        published = { text: result, revision: editedRevision };
        options.onConfigChange(result);
      }
      idle(true);
    } catch {
      if (current(id, text)) emit({ busy: false, reading: false, valid: false, error: "write" });
    }
  }

  async function load(configText: string) {
    if (disposed || source === configText) return;
    source = configText;
    ++request;
    const echo = published;
    published = null;
    if (echo?.text === configText && echo.revision === revision && !dirty) {
      idle(options.isValid(rows));
      return;
    }
    if (dirty) {
      // Reapply only edited header fields to the latest source, preserving an
      // unrelated model, endpoint or TOML change. Invalid rows stay editable.
      if (options.isValid(rows)) await write(configText);
      else idle(false);
      return;
    }
    if (!configText.trim()) {
      emit({ busy: false, reading: true, valid: false, error: null });
      return;
    }
    await read(configText);
  }

  async function changeRows(next: readonly ProviderHeader[]) {
    if (disposed) return;
    ++request;
    ++revision;
    rows = next.map((row) => ({ ...row }));
    dirty = true;
    options.onRowsChange(rows);
    if (!options.isValid(rows)) {
      idle(false);
      return;
    }
    await write(options.getConfigText());
  }

  async function retry() {
    if (disposed) return;
    const text = options.getConfigText();
    if (dirty && options.isValid(rows)) await write(text);
    else if (!dirty) await read(text);
  }

  return {
    load,
    changeRows,
    retry,
    dispose() { disposed = true; ++request; },
  };
}
