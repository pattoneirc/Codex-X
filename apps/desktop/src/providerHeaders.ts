export type ProviderHeaderSource = "static" | "env";

export type ProviderHeader = {
  name: string;
  value: string;
  source: ProviderHeaderSource;
};

export type ProviderHeaderError = { name?: string; value?: string; source?: string };
type Language = "zh" | "en";

// RFC 9110 field-name tokens. Header values may contain horizontal tabs and
// UTF-8 text, matching reqwest's HeaderValue validation on the backend.
const HEADER_NAME = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
const INVALID_HEADER_VALUE = /[\u0000-\u0008\u000a-\u001f\u007f]/;
const ENV_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

export function createProviderHeader(source: ProviderHeaderSource = "static"): ProviderHeader {
  return { name: "", value: "", source };
}

export function updateProviderHeader(rows: readonly ProviderHeader[], index: number, patch: Partial<ProviderHeader>): ProviderHeader[] {
  return rows.map((row, position) => position === index ? { ...row, ...patch } : row);
}

export function removeProviderHeader(rows: readonly ProviderHeader[], index: number): ProviderHeader[] {
  return rows.filter((_, position) => position !== index);
}

export function validateProviderHeaders(rows: readonly ProviderHeader[], lang: Language = "en") {
  const counts = new Map<string, number>();
  for (const row of rows) {
    const name = row.name.trim().toLowerCase();
    if (name) counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  const errors: ProviderHeaderError[] = rows.map((row) => {
    const error: ProviderHeaderError = {};
    const name = row.name.trim();
    if (!name) error.name = lang === "zh" ? "请填写 Header 名称" : "Enter a header name";
    else if (!HEADER_NAME.test(name)) error.name = lang === "zh" ? "名称只能使用英文字母、数字和 HTTP 允许的符号" : "Use letters, numbers and valid HTTP header symbols";
    else if ((counts.get(name.toLowerCase()) ?? 0) > 1) error.name = lang === "zh" ? "此名称重复（不区分大小写），请删除或重命名" : "Duplicate header name (case insensitive). Remove or rename it";
    if (row.source === "env") {
      if (!ENV_NAME.test(row.value)) error.value = lang === "zh" ? "请填写环境变量名，如 PROVIDER_PROJECT；仅允许字母、数字和下划线，且不能以数字开头" : "Enter an environment variable name, e.g. PROVIDER_PROJECT, using letters, numbers and underscores, without a leading digit";
    } else if (row.source === "static") {
      if (INVALID_HEADER_VALUE.test(row.value)) error.value = lang === "zh" ? "Header 值不能包含换行或控制字符" : "Header values cannot contain line breaks or control characters";
    } else {
      error.source = lang === "zh" ? "请选择固定值或环境变量" : "Choose a literal value or environment variable";
    }
    return error;
  });
  return { errors, valid: errors.every((error) => !error.name && !error.value && !error.source) };
}

/** Normalize only names; changing a value's whitespace can change its meaning. */
export function normalizeProviderHeaders(rows: readonly ProviderHeader[]): ProviderHeader[] {
  if (!validateProviderHeaders(rows).valid) throw new Error("Invalid provider headers");
  return rows.map((row) => ({ ...row, name: row.name.trim() }));
}

export function findProviderHeader(rows: readonly ProviderHeader[], name: string): ProviderHeader | undefined {
  const target = name.trim().toLowerCase();
  return rows.find((row) => row.name.trim().toLowerCase() === target);
}
