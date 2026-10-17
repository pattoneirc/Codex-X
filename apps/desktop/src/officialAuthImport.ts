export const MAX_AUTH_IMPORT_BYTES = 1024 * 1024;
type ImportErrorCode = "too_large" | "invalid_text" | "invalid_json" | "unsupported_shape";

export class OfficialAuthImportError extends Error {
  readonly code: ImportErrorCode;
  constructor(code: ImportErrorCode) {
    super(code);
    this.name = "OfficialAuthImportError";
    this.code = code;
  }
}

export function parseOfficialAuthImport(bytes: Uint8Array): string {
  if (bytes.byteLength > MAX_AUTH_IMPORT_BYTES) throw new OfficialAuthImportError("too_large");
  let text: string;
  try { text = new TextDecoder("utf-8", { fatal: true }).decode(bytes).replace(/^\uFEFF/, "").trim(); }
  catch { throw new OfficialAuthImportError("invalid_text"); }
  let value: unknown;
  try { value = JSON.parse(text); }
  catch { throw new OfficialAuthImportError("invalid_json"); }
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new OfficialAuthImportError("unsupported_shape");
  const auth = value as Record<string, unknown>;
  const tokens = auth.tokens && typeof auth.tokens === "object" && !Array.isArray(auth.tokens)
    ? auth.tokens as Record<string, unknown> : null;
  const nonempty = (value: unknown) => typeof value === "string" && Boolean(value.trim());
  if (["base_url", "baseUrl", "api_base", "endpoint"].some(key => Object.prototype.hasOwnProperty.call(auth, key))
    || !(nonempty(auth.OPENAI_API_KEY) || (tokens && [tokens.access_token, tokens.id_token, tokens.refresh_token].some(nonempty)))) {
    throw new OfficialAuthImportError("unsupported_shape");
  }
  // Only load the editor draft. The existing save command validates credentials.
  return text;
}

export async function readOfficialAuthImport(file: Pick<File, "size" | "arrayBuffer">): Promise<string> {
  if (file.size > MAX_AUTH_IMPORT_BYTES) throw new OfficialAuthImportError("too_large");
  return parseOfficialAuthImport(new Uint8Array(await file.arrayBuffer()));
}
