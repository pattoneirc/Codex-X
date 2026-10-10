import type { ConfigHealthIssue, ConfigHealthReport } from "./configHealthTypes";

// Fall back for older backend/fixture reports while retaining exact locations
// from the diagnostic command. Missing definitions do not have a source line.
export function configHealthIssueLocation(issue: ConfigHealthIssue, report: ConfigHealthReport, lang: "zh" | "en") {
  const zh = lang === "zh";
  const path = issue.path || `${report.codexDir.replace(/[\\/]$/, "")}/config.toml`;
  const position = issue.line != null && issue.line > 0
    ? (zh ? `第 ${issue.line} 行${issue.column != null ? `，第 ${issue.column} 列` : ""}`
      : `line ${issue.line}${issue.column != null ? `, column ${issue.column}` : ""}`)
    : !issue.key
      ? (zh ? "旧版报告未提供行列，请重新检查" : "location not supplied; check again")
    : issue.key.startsWith('"<')
      ? (zh ? "文件层面" : "file level")
      : (zh ? "未定义，尚无对应行" : "not defined; no source line");
  return { path, position, key: issue.key || (zh ? "配置文件" : "configuration file") };
}

export function primaryConfigHealthIssue(report: ConfigHealthReport) {
  return (report.canRepair ? report.issues.find((issue) => issue.repairable) : undefined) ?? report.issues[0];
}

export function configHealthRepairUnavailableReason(report: ConfigHealthReport, lang: "zh" | "en") {
  return (report.issues.find((issue) => issue.code === "config-linked-file")
    ?? report.issues.find((issue) => !issue.repairable))?.suggestion
    || (lang === "zh" ? "现有配置无法可靠确定，请按问题中的建议修改后重新检查。" : "The correct settings cannot be determined safely. Follow the issue's suggestion and check again.");
}
