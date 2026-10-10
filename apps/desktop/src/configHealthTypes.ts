export type ConfigHealthIssue = {
  code: string;
  title: string;
  description: string;
  repairable: boolean;
  path: string;
  line: number | null;
  column: number | null;
  key: string;
  suggestion: string;
};

export type ConfigHealthReport = {
  codexDir: string;
  fingerprint: string;
  status: "healthy" | "issues" | "missing" | "unavailable";
  issues: ConfigHealthIssue[];
  canRepair: boolean;
  repairSummary: string[];
  checkedAt: string;
};

export type ConfigHealthRepairResult = {
  report: ConfigHealthReport;
  backupId: string | null;
  changed: boolean;
};
