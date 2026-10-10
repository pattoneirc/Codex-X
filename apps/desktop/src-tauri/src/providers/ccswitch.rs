use super::{
    consolidate_legacy_provider_duplicates_on_connection, custom_provider_id,
    experimental_bearer_token_from_doc, list_saved_providers_on_connection,
    normalize_saved_provider, open_store, strip_provider_bearer_tokens,
    upsert_ccswitch_provider_on_connection, ProviderUpsertKind, SavedProvider,
};
use crate::ccswitch::{ccswitch_db_candidates, default_ccswitch_db_path};
use crate::error::{CodexxError, Result};
use crate::failover::protocol::UpstreamApi;
use crate::sqlite_utils::table_column_set;
use crate::string_value;
use crate::toml_utils::ensure_table;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use toml_edit::{value, DocumentMut, Item, Table};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportResult {
    imported: usize,
    added: usize,
    updated: usize,
    merged: usize,
    skipped: usize,
    warnings: Vec<String>,
    providers: Vec<SavedProvider>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialAuthCandidate {
    auth_json: String,
    config_text: Option<String>,
    model: Option<String>,
    source: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CcSwitchCodexRow {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) settings_config: String,
    pub(crate) category: Option<String>,
    pub(crate) meta: Option<String>,
}

pub(crate) fn is_official_ccswitch_row(row: &CcSwitchCodexRow) -> bool {
    row.id.trim().eq_ignore_ascii_case("codex-official")
        || row
            .category
            .as_deref()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("official"))
}

pub(crate) fn read_ccswitch_codex_rows(conn: &Connection) -> Result<Vec<CcSwitchCodexRow>> {
    let provider_columns = table_column_set(conn, "providers")?;
    let category_column = if provider_columns.contains("category") {
        "category"
    } else {
        "NULL"
    };
    let meta_column = if provider_columns.contains("meta") {
        "meta"
    } else {
        "NULL"
    };
    let provider_query = format!(
        "SELECT id, name, settings_config, {category_column}, {meta_column} FROM providers
         WHERE app_type = 'codex' ORDER BY sort_index ASC, created_at ASC"
    );
    let mut stmt = conn
        .prepare(&provider_query)
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(CcSwitchCodexRow {
                id: row.get::<_, String>(0)?,
                name: row.get::<_, String>(1)?,
                settings_config: row.get::<_, String>(2)?,
                category: row.get::<_, Option<String>>(3)?,
                meta: row.get::<_, Option<String>>(4)?,
            })
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| CodexxError::Database(e.to_string()))?);
    }
    Ok(result)
}

#[derive(Debug, Clone)]
pub(crate) struct CcSwitchCodexSection {
    pub(crate) id: String,
    pub(crate) name: Option<String>,
    pub(crate) base_url: String,
    pub(crate) model: Option<String>,
    pub(crate) wire_api: String,
    pub(crate) requires_openai_auth: bool,
    pub(crate) experimental_bearer_token: Option<String>,
    pub(crate) provider_table: Table,
}

fn table_string(table: &Table, key: &str) -> Option<String> {
    table
        .get(key)
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

fn ccswitch_auth_api_key(settings: &Value) -> Option<String> {
    settings
        .get("auth")
        .and_then(|v| v.get("OPENAI_API_KEY"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

fn explicit_ccswitch_api_format(
    row: &CcSwitchCodexRow,
    object: &Value,
    prefix: &str,
) -> Result<Option<(String, String)>> {
    for key in ["apiFormat", "api_format"] {
        let Some(value) = object.get(key).filter(|value| !value.is_null()) else {
            continue;
        };
        let field = format!("{prefix}.{key}");
        let text = value.as_str().ok_or_else(|| {
            CodexxError::Config(format!(
                "cc-switch 供应商 {} 的 {field} 必须是协议名称文本",
                row.id
            ))
        })?;
        if !text.trim().is_empty() {
            return Ok(Some((text.trim().to_string(), field)));
        }
    }
    Ok(None)
}

fn ccswitch_upstream_api(
    row: &CcSwitchCodexRow,
    settings: &Value,
    wire_api: &str,
    provider_id: Option<&str>,
) -> Result<Option<String>> {
    let mut selected = None;
    if let Some(text) = row.meta.as_deref().filter(|text| !text.trim().is_empty()) {
        let meta: Value = serde_json::from_str(text).map_err(|_| {
            CodexxError::Config(format!("cc-switch 供应商 {} 的 meta 不是有效 JSON", row.id))
        })?;
        if !meta.is_null() && !meta.is_object() {
            return Err(CodexxError::Config(format!(
                "cc-switch 供应商 {} 的 meta 必须是 JSON 对象",
                row.id
            )));
        }
        selected = explicit_ccswitch_api_format(row, &meta, "meta")?;
    }
    if selected.is_none() {
        if let Some(meta) = settings.get("meta").filter(|meta| !meta.is_null()) {
            if !meta.is_object() {
                return Err(CodexxError::Config(format!(
                    "cc-switch 供应商 {} 的 settings.meta 必须是 JSON 对象",
                    row.id
                )));
            }
            selected = explicit_ccswitch_api_format(row, meta, "settings.meta")?;
        }
    }
    if selected.is_none() {
        selected = explicit_ccswitch_api_format(row, settings, "settings")?;
    }
    let explicit = selected.is_some();
    let (format, field) = selected.unwrap_or_else(|| {
        (
            wire_api.trim().to_string(),
            format!(
                "config.model_providers.{}.wire_api",
                provider_id.unwrap_or("<当前供应商>")
            ),
        )
    });
    let protocol = UpstreamApi::from_provider(Some(&format), "responses").map_err(|_| {
        CodexxError::Config(format!(
            "cc-switch 供应商 {} 的 {field} 上游协议不受支持；请选择 Responses、Chat Completions、Anthropic Messages 或 Gemini",
            row.id
        ))
    })?;
    // An old native template says nothing about a locally selected bridge.
    // Leave None so a refresh can retain Codex-X's independent metadata.
    if !explicit && protocol == UpstreamApi::Responses {
        return Ok(None);
    }
    Ok(Some(
        match protocol {
            UpstreamApi::Responses => "responses",
            UpstreamApi::ChatCompletions => "chat_completions",
            UpstreamApi::AnthropicMessages => "anthropic_messages",
            UpstreamApi::Gemini => "gemini",
        }
        .to_string(),
    ))
}

pub(super) fn codex_section_from_table(
    id: &str,
    table: &Table,
    model: Option<String>,
) -> Option<CcSwitchCodexSection> {
    let base_url = table_string(table, "base_url")?
        .trim_end_matches('/')
        .to_string();
    if base_url.is_empty() {
        return None;
    }
    Some(CcSwitchCodexSection {
        id: id.to_string(),
        name: table_string(table, "name"),
        base_url,
        model,
        wire_api: table_string(table, "wire_api").unwrap_or_else(|| "responses".to_string()),
        requires_openai_auth: table
            .get("requires_openai_auth")
            .and_then(|item| item.as_bool())
            .unwrap_or(false),
        experimental_bearer_token: table_string(table, "experimental_bearer_token"),
        provider_table: table.clone(),
    })
}

pub(crate) fn codex_sections_from_config(config_text: &str) -> Vec<CcSwitchCodexSection> {
    let Ok(doc) = config_text.parse::<DocumentMut>() else {
        return Vec::new();
    };
    let model = string_value(&doc, "model");
    let Some(providers) = doc.get("model_providers").and_then(|item| item.as_table()) else {
        return Vec::new();
    };
    providers
        .iter()
        .filter_map(|(id, item)| {
            item.as_table()
                .and_then(|table| codex_section_from_table(id, table, model.clone()))
        })
        .collect()
}

fn select_ccswitch_section_for_row(
    row: &CcSwitchCodexRow,
    settings: &Value,
    global_sections: &HashMap<String, CcSwitchCodexSection>,
) -> Option<CcSwitchCodexSection> {
    let provider_id = custom_provider_id(&row.id);
    let config_text = settings.get("config").and_then(Value::as_str).unwrap_or("");
    let doc = config_text.parse::<DocumentMut>().ok();

    if let Some(doc) = doc.as_ref() {
        let model = string_value(doc, "model");
        let active_provider = string_value(doc, "model_provider");
        let providers = doc.get("model_providers").and_then(|item| item.as_table());

        if let Some(providers) = providers {
            for exact_id in [provider_id.as_str(), row.id.trim()] {
                if let Some(section) = providers
                    .get(exact_id)
                    .and_then(|item| item.as_table())
                    .and_then(|table| codex_section_from_table(exact_id, table, model.clone()))
                {
                    return Some(section);
                }
            }

            if active_provider.as_deref() == Some(row.id.trim())
                || active_provider.as_deref() == Some(provider_id.as_str())
            {
                if let Some(active) = active_provider.as_deref() {
                    if let Some(section) = providers
                        .get(active)
                        .and_then(|item| item.as_table())
                        .and_then(|table| codex_section_from_table(active, table, model.clone()))
                    {
                        return Some(section);
                    }
                }
            }

            // Legacy cc-switch templates store each third-party provider under
            // `[model_providers.custom]` in that row's own complete config.
            if active_provider
                .as_deref()
                .is_none_or(|active| active == "custom")
            {
                if let Some(section) = providers
                    .get("custom")
                    .and_then(|item| item.as_table())
                    .and_then(|table| codex_section_from_table("custom", table, model.clone()))
                {
                    return Some(section);
                }
            }
        }
    }

    for exact_id in [provider_id.as_str(), row.id.trim()] {
        if let Some(section) = global_sections.get(exact_id) {
            return Some(section.clone());
        }
    }

    let doc = doc?;
    let active_provider = string_value(&doc, "model_provider");
    doc.get("base_url")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|base_url| {
            let base_url = base_url.trim_end_matches('/').to_string();
            let token = experimental_bearer_token_from_doc(&doc, active_provider.as_deref());
            let mut provider_table = Table::new();
            provider_table["base_url"] = value(base_url.clone());
            provider_table["wire_api"] = value("responses");
            provider_table["requires_openai_auth"] = value(false);
            if let Some(token) = token.as_deref() {
                provider_table["experimental_bearer_token"] = value(token);
            }
            CcSwitchCodexSection {
                id: provider_id,
                name: None,
                base_url,
                model: string_value(&doc, "model"),
                wire_api: "responses".to_string(),
                requires_openai_auth: false,
                experimental_bearer_token: token,
                provider_table,
            }
        })
}

fn ccswitch_provider_template(
    settings: &Value,
    section: &CcSwitchCodexSection,
    provider_name: &str,
    model: &str,
) -> Option<String> {
    let config_text = settings.get("config").and_then(Value::as_str).unwrap_or("");
    let mut doc = if config_text.trim().is_empty() {
        DocumentMut::new()
    } else {
        config_text.parse::<DocumentMut>().ok()?
    };
    let provider_id = section.id.trim();
    if provider_id.is_empty() {
        return None;
    }

    if string_value(&doc, "model_provider").as_deref() != Some(provider_id) {
        doc["model_provider"] = value(provider_id);
    }
    if string_value(&doc, "model").as_deref() != Some(model) {
        doc["model"] = value(model);
    }
    let providers = ensure_table(doc.as_table_mut(), "model_providers").ok()?;
    if providers
        .get(provider_id)
        .and_then(|item| item.as_table())
        .is_none()
    {
        let mut table = section.provider_table.clone();
        if table_string(&table, "name").is_none() && !provider_name.trim().is_empty() {
            table["name"] = value(provider_name.trim());
        }
        providers.insert(provider_id, Item::Table(table));
    }
    let table = providers
        .get_mut(provider_id)
        .and_then(Item::as_table_mut)?;
    if table.get("wire_api").and_then(Item::as_str) != Some("responses") {
        table["wire_api"] = value("responses");
    }
    strip_provider_bearer_tokens(&mut doc);
    Some(doc.to_string().trim_end().to_string())
}

pub(crate) fn build_ccswitch_codex_provider(
    row: &CcSwitchCodexRow,
    global_sections: &HashMap<String, CcSwitchCodexSection>,
) -> Result<Option<SavedProvider>> {
    let Ok(settings) = serde_json::from_str::<Value>(&row.settings_config) else {
        ccswitch_upstream_api(row, &Value::Null, "responses", None)?;
        return Ok(None);
    };
    let Some(section) = select_ccswitch_section_for_row(row, &settings, global_sections) else {
        // A configured but unsupported protocol should not be hidden behind
        // the generic incomplete-provider warning.
        ccswitch_upstream_api(row, &settings, "responses", None)?;
        return Ok(None);
    };
    let upstream_api = ccswitch_upstream_api(row, &settings, &section.wire_api, Some(&section.id))?;
    let api_key = ccswitch_auth_api_key(&settings).or(section.experimental_bearer_token.clone());
    let provider_name = section
        .name
        .clone()
        .or_else(|| {
            let name = row.name.trim();
            (!name.is_empty()).then(|| name.to_string())
        })
        .unwrap_or_else(|| row.id.clone());
    let Some(model) = section.model.clone() else {
        return Ok(None);
    };
    let Some(toml_config) = ccswitch_provider_template(&settings, &section, &provider_name, &model)
    else {
        return Ok(None);
    };
    Ok(Some(SavedProvider {
        id: custom_provider_id(&row.id),
        provider_name,
        base_url: section.base_url,
        model,
        api_key,
        toml_config: Some(toml_config),
        wire_api: "responses".to_string(),
        requires_openai_auth: section.requires_openai_auth,
        upstream_api,
        model_mappings: Vec::new(),
    }))
}

pub(crate) fn import_ccswitch_codex_providers_inner(path: Option<String>) -> Result<ImportResult> {
    let db = path
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or(default_ccswitch_db_path()?);

    if !db.exists() {
        let candidates = ccswitch_db_candidates()?
            .into_iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n- ");
        return Err(CodexxError::Config(format!(
            "cc-switch 数据库不存在: {}\n已检查候选路径:\n- {}",
            db.display(),
            candidates
        )));
    }

    let conn = Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| {
        CodexxError::Database(format!("打开 cc-switch 数据库失败 {}: {e}", db.display()))
    })?;

    let rows_vec = read_ccswitch_codex_rows(&conn)?;

    let mut global_sections: HashMap<String, CcSwitchCodexSection> = HashMap::new();
    for row in &rows_vec {
        if is_official_ccswitch_row(row) {
            continue;
        }
        let Ok(settings) = serde_json::from_str::<Value>(&row.settings_config) else {
            continue;
        };
        let Some(config_text) = settings.get("config").and_then(Value::as_str) else {
            continue;
        };
        for section in codex_sections_from_config(config_text) {
            if !global_sections.contains_key(&section.id) {
                global_sections.insert(section.id.clone(), section);
            }
        }
    }

    let mut imported = 0usize;
    let mut added = 0usize;
    let mut updated = 0usize;
    let mut merged = 0usize;
    let mut skipped = 0usize;
    let mut warnings = Vec::new();
    let mut local_conn = open_store()?;
    let transaction = local_conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    merged += consolidate_legacy_provider_duplicates_on_connection(&transaction)?;

    for row in rows_vec {
        if is_official_ccswitch_row(&row) {
            skipped += 1;
            warnings.push(format!(
                "跳过 {} ({})：官方认证不作为第三方供应商导入",
                row.name, row.id
            ));
            continue;
        }
        match build_ccswitch_codex_provider(&row, &global_sections)? {
            Some(provider) => {
                let provider = normalize_saved_provider(provider)?;
                let result =
                    upsert_ccswitch_provider_on_connection(&transaction, provider, row.id.trim())?;
                match result.kind {
                    ProviderUpsertKind::Added => added += 1,
                    ProviderUpsertKind::Updated => updated += 1,
                }
                imported += 1;
            }
            None => {
                skipped += 1;
                warnings.push(format!(
                    "跳过 {} ({})：未找到可用 config/base_url，可能是官方登录或空模板",
                    row.name, row.id
                ));
            }
        }
    }
    merged += consolidate_legacy_provider_duplicates_on_connection(&transaction)?;
    transaction
        .commit()
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let providers = list_saved_providers_on_connection(&local_conn)?;

    Ok(ImportResult {
        imported,
        added,
        updated,
        merged,
        skipped,
        warnings,
        providers,
    })
}

pub(crate) fn read_ccswitch_official_auth_inner(
    path: Option<String>,
) -> Result<Option<OfficialAuthCandidate>> {
    let db = path
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or(default_ccswitch_db_path()?);

    if !db.exists() {
        return Ok(None);
    }

    let conn = Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| {
        CodexxError::Database(format!("打开 cc-switch 数据库失败 {}: {e}", db.display()))
    })?;

    let provider_columns = table_column_set(&conn, "providers")?;
    let official_filter = if provider_columns.contains("category") {
        "id = 'codex-official' OR category = 'official'"
    } else {
        // Older cc-switch databases predate the category column. The stable
        // codex-official id is still enough to identify the official row.
        "id = 'codex-official'"
    };
    let query = format!(
        "SELECT id, name, settings_config FROM providers
         WHERE app_type = 'codex' AND ({official_filter})
         ORDER BY CASE WHEN id = 'codex-official' THEN 0 ELSE 1 END
         LIMIT 1"
    );
    let mut stmt = conn
        .prepare(&query)
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    let mut rows = stmt
        .query([])
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    let Some(row) = rows
        .next()
        .map_err(|e| CodexxError::Database(e.to_string()))?
    else {
        return Ok(None);
    };

    let id: String = row
        .get(0)
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let name: String = row
        .get(1)
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let settings_config: String = row
        .get(2)
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let settings: Value = serde_json::from_str(&settings_config).map_err(|e| {
        CodexxError::Database(format!("cc-switch official settings JSON 解析失败: {e}"))
    })?;

    let auth = settings
        .get("auth")
        .cloned()
        .filter(|value| value.is_object())
        .ok_or_else(|| {
            CodexxError::Database("cc-switch official provider 缺少 auth object".to_string())
        })?;

    let config_text = settings
        .get("config")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let model = config_text
        .as_deref()
        .and_then(|text| text.parse::<DocumentMut>().ok())
        .and_then(|doc| string_value(&doc, "model"));

    let auth_json = serde_json::to_string_pretty(&auth)
        .map_err(|e| CodexxError::Database(format!("官方 auth JSON 格式化失败: {e}")))?;

    Ok(Some(OfficialAuthCandidate {
        auth_json,
        config_text,
        model,
        source: format!("cc-switch:{name}:{id}"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn protocol_row(format: Option<&str>) -> CcSwitchCodexRow {
        CcSwitchCodexRow {
            id: "protocol-fixture".into(),
            name: "Protocol fixture".into(),
            settings_config: json!({
                "auth": {"OPENAI_API_KEY": "fixture-api-key"},
                "config": "# keep imported comment\nmodel_provider='custom'\nmodel='fixture-model'\n[model_providers.custom]\nname='Protocol fixture'\nbase_url='https://neutral-gateway.example.test/custom-api-prefix'\nwire_api='responses'\nrequires_openai_auth=false\nhttp_headers={X-Title='Fixture title'}\n[mcp_servers.keep]\ncommand='fixture-server'\n",
            }).to_string(),
            category: None,
            meta: format.map(|format| json!({"apiFormat":format}).to_string()),
        }
    }

    fn fixture_ccswitch_database(with_meta: bool) -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(dir.path().join("fixture-ccswitch.db")).unwrap();
        let optional = if with_meta {
            "category TEXT, meta TEXT,"
        } else {
            ""
        };
        conn.execute_batch(&format!(
            "CREATE TABLE providers (id TEXT, name TEXT, app_type TEXT, settings_config TEXT, {optional} sort_index INTEGER, created_at INTEGER);"
        )).unwrap();
        (dir, conn)
    }

    #[test]
    fn current_database_reads_metadata_and_builds_all_four_protocols_without_endpoint_guessing() {
        let (_dir, conn) = fixture_ccswitch_database(true);
        for (index, (format, canonical)) in [
            ("openai_responses", "responses"),
            ("openai_chat", "chat_completions"),
            ("anthropic", "anthropic_messages"),
            ("gemini", "gemini"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut row = protocol_row(Some(format));
            row.id = format!("protocol-{index}");
            conn.execute(
                "INSERT INTO providers (id,name,app_type,settings_config,meta,sort_index,created_at) VALUES (?1,?2,'codex',?3,?4,?5,1)",
                rusqlite::params![row.id,row.name,row.settings_config,row.meta,index],
            ).unwrap();
            let read = read_ccswitch_codex_rows(&conn).unwrap().pop().unwrap();
            let provider = build_ccswitch_codex_provider(&read, &HashMap::new())
                .unwrap()
                .unwrap();
            assert_eq!(provider.upstream_api.as_deref(), Some(canonical));
            assert_eq!(provider.wire_api, "responses");
            assert_eq!(
                provider.base_url,
                "https://neutral-gateway.example.test/custom-api-prefix"
            );
            let text = provider.toml_config.unwrap();
            assert!(text.contains("# keep imported comment"));
            let doc = text.parse::<DocumentMut>().unwrap();
            let table = doc["model_providers"]["custom"].as_table().unwrap();
            assert_eq!(table["wire_api"].as_str(), Some("responses"));
            assert_eq!(
                table["http_headers"]["X-Title"].as_str(),
                Some("Fixture title")
            );
            assert!(table.get("upstream_api").is_none());
            assert!(table.get("apiFormat").is_none());
            assert_eq!(
                doc["mcp_servers"]["keep"]["command"].as_str(),
                Some("fixture-server")
            );
        }
    }

    #[test]
    fn old_database_without_metadata_columns_preserves_absence_and_uses_settings_formats() {
        let (_dir, conn) = fixture_ccswitch_database(false);
        let mut row = protocol_row(None);
        let mut settings: Value = serde_json::from_str(&row.settings_config).unwrap();
        settings["api_format"] = json!("anthropic_messages");
        row.settings_config = settings.to_string();
        conn.execute(
            "INSERT INTO providers (id,name,app_type,settings_config,sort_index,created_at) VALUES (?1,?2,'codex',?3,0,1)",
            rusqlite::params![row.id,row.name,row.settings_config],
        ).unwrap();
        let read = read_ccswitch_codex_rows(&conn).unwrap().pop().unwrap();
        assert!(read.category.is_none());
        assert!(read.meta.is_none());
        let provider = build_ccswitch_codex_provider(&read, &HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(provider.upstream_api.as_deref(), Some("anthropic_messages"));
        let native = build_ccswitch_codex_provider(&protocol_row(None), &HashMap::new())
            .unwrap()
            .unwrap();
        assert!(native.upstream_api.is_none());
    }

    #[test]
    fn protocol_metadata_priority_is_database_meta_then_nested_settings_then_settings_format() {
        let mut row = protocol_row(Some("openai_chat"));
        let mut settings: Value = serde_json::from_str(&row.settings_config).unwrap();
        settings["meta"] = json!({"apiFormat":"anthropic"});
        settings["apiFormat"] = json!("gemini");
        settings["api_format"] = json!("responses");
        row.settings_config = settings.to_string();
        let db = build_ccswitch_codex_provider(&row, &HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(db.upstream_api.as_deref(), Some("chat_completions"));
        row.meta = None;
        let nested = build_ccswitch_codex_provider(&row, &HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(nested.upstream_api.as_deref(), Some("anthropic_messages"));
        settings.as_object_mut().unwrap().remove("meta");
        row.settings_config = settings.to_string();
        let camel = build_ccswitch_codex_provider(&row, &HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(camel.upstream_api.as_deref(), Some("gemini"));
        settings.as_object_mut().unwrap().remove("apiFormat");
        row.settings_config = settings.to_string();
        let snake = build_ccswitch_codex_provider(&row, &HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(snake.upstream_api.as_deref(), Some("responses"));
    }

    #[test]
    fn explicit_metadata_overrides_legacy_wire_format_and_legacy_chat_migrates_native_toml() {
        let mut row = protocol_row(None);
        row.settings_config = row
            .settings_config
            .replace("wire_api='responses'", "wire_api='chat'");
        let legacy = build_ccswitch_codex_provider(&row, &HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(legacy.upstream_api.as_deref(), Some("chat_completions"));
        assert_eq!(legacy.wire_api, "responses");
        assert_eq!(
            legacy.toml_config.unwrap().parse::<DocumentMut>().unwrap()["model_providers"]
                ["custom"]["wire_api"]
                .as_str(),
            Some("responses")
        );
        row.meta = Some(json!({"apiFormat":"openai_responses"}).to_string());
        let explicit = build_ccswitch_codex_provider(&row, &HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(explicit.upstream_api.as_deref(), Some("responses"));
        row.settings_config = row
            .settings_config
            .replace("wire_api='chat'", "wire_api='unknown-wire'");
        let overridden = build_ccswitch_codex_provider(&row, &HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(overridden.upstream_api.as_deref(), Some("responses"));
    }

    #[test]
    fn unknown_protocol_metadata_reports_its_provider_and_field_without_echoing_credentials() {
        for (meta, settings_format, key) in [
            (Some("future_protocol"), None, "meta.apiFormat"),
            (None, Some("future_protocol"), "settings.api_format"),
        ] {
            let mut row = protocol_row(meta);
            if let Some(format) = settings_format {
                let mut settings: Value = serde_json::from_str(&row.settings_config).unwrap();
                settings["api_format"] = json!(format);
                row.settings_config = settings.to_string();
            }
            let error = build_ccswitch_codex_provider(&row, &HashMap::new())
                .unwrap_err()
                .to_string();
            assert!(error.contains(&row.id));
            assert!(error.contains(key));
            assert!(error.contains("不受支持"));
            assert!(!error.contains("fixture-api-key"));
        }
        let mut row = protocol_row(None);
        row.settings_config = row
            .settings_config
            .replace("wire_api='responses'", "wire_api='future_wire'");
        let error = build_ccswitch_codex_provider(&row, &HashMap::new())
            .unwrap_err()
            .to_string();
        assert!(error.contains("config.model_providers.custom.wire_api"));
        row.meta = Some(json!({"apiFormat":"future_protocol"}).to_string());
        row.settings_config = json!({"apiFormat":"responses","config":""}).to_string();
        assert!(build_ccswitch_codex_provider(&row, &HashMap::new()).is_err());
    }

    #[test]
    fn malformed_metadata_and_non_text_protocol_fields_are_rejected_without_raw_json() {
        for meta in ["{fixture-private-meta", "[]", "{\"apiFormat\":42}"] {
            let mut row = protocol_row(None);
            row.meta = Some(meta.to_string());
            let error = build_ccswitch_codex_provider(&row, &HashMap::new())
                .unwrap_err()
                .to_string();
            assert!(error.contains("meta"));
            assert!(!error.contains("fixture-private-meta"));
            assert!(!error.contains("fixture-api-key"));
        }
    }

    #[test]
    fn refreshing_old_native_input_keeps_the_local_protocol_but_an_explicit_choice_replaces_it() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE providers (id TEXT PRIMARY KEY,provider_name TEXT NOT NULL,base_url TEXT NOT NULL,model TEXT NOT NULL,api_key TEXT,toml_config TEXT,wire_api TEXT NOT NULL,requires_openai_auth INTEGER NOT NULL,model_mappings_json TEXT NOT NULL,upstream_api TEXT,source TEXT NOT NULL,source_id TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);").unwrap();
        let original = normalize_saved_provider(
            build_ccswitch_codex_provider(&protocol_row(Some("openai_chat")), &HashMap::new())
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        upsert_ccswitch_provider_on_connection(&conn, original, "protocol-fixture").unwrap();
        let old_input = build_ccswitch_codex_provider(&protocol_row(None), &HashMap::new())
            .unwrap()
            .unwrap();
        assert!(old_input.upstream_api.is_none());
        let retained = upsert_ccswitch_provider_on_connection(
            &conn,
            normalize_saved_provider(old_input).unwrap(),
            "protocol-fixture",
        )
        .unwrap()
        .provider;
        assert_eq!(retained.upstream_api.as_deref(), Some("chat_completions"));
        let explicit =
            build_ccswitch_codex_provider(&protocol_row(Some("openai_responses")), &HashMap::new())
                .unwrap()
                .unwrap();
        let replaced = upsert_ccswitch_provider_on_connection(
            &conn,
            normalize_saved_provider(explicit).unwrap(),
            "protocol-fixture",
        )
        .unwrap()
        .provider;
        assert_eq!(replaced.upstream_api.as_deref(), Some("responses"));
    }

    #[test]
    fn provider_import_round_trips_complete_config_without_bearer_tokens() {
        let settings_config = json!({
            "auth": {"OPENAI_API_KEY": "sk-from-auth"},
            "config": r#"# keep-imported-comment
model_provider = "custom"
model = "gpt-5.6-sol"
model_reasoning_effort = "xhigh"
service_tier = "priority"
experimental_bearer_token = "sk-top-level"
notify = ["C:\\Users\\Thy\\codex-computer-use.exe", "turn-ended"]

[model_providers.custom]
name = "Sky2api"
base_url = "https://proxy.example.com/v1/"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-from-config"
request_max_retries = 7
http_headers = { "HTTP-Referer" = "https://header.example.test", "User-Agent" = "Fixture agent" }
env_http_headers = { "X-Project" = "PROJECT_ID" }

[model_providers.other]
name = "Other provider"
base_url = "https://other.example.com/v1"
experimental_bearer_token = "sk-other"

[projects."/work/project"]
trust_level = "trusted"

[desktop]
followUpQueueMode = "queue"
localeOverride = "zh-CN"

[windows]
sandbox = "elevated"
shell_path = 'D:\Program Files\PowerShell\7\pwsh.exe'

[plugins."browser@openai-bundled"]
enabled = true

[features]
js_repl = false

[shell_environment_policy.set]
CODEX_HOME = 'C:\Users\Thy\.codex'

[mcp_servers.docs]
command = "docs-server"
"#,
        })
        .to_string();
        let row = CcSwitchCodexRow {
            id: "magicai-123".to_string(),
            name: "  Sky2_free  ".to_string(),
            settings_config,
            category: None,
            meta: None,
        };

        let provider = build_ccswitch_codex_provider(&row, &HashMap::new())
            .expect("valid cc-switch provider")
            .expect("build cc-switch provider");
        assert_eq!(provider.id, "magicai-123");
        assert_eq!(provider.provider_name, "Sky2api");
        assert_eq!(provider.base_url, "https://proxy.example.com/v1");
        assert_eq!(provider.model, "gpt-5.6-sol");
        assert_eq!(provider.api_key.as_deref(), Some("sk-from-auth"));
        assert_eq!(provider.wire_api, "responses");
        assert!(!provider.requires_openai_auth);

        let text = provider.toml_config.expect("complete provider TOML");
        let doc = text.parse::<DocumentMut>().expect("parse provider TOML");
        assert!(text.contains("# keep-imported-comment"));
        assert_eq!(doc["model_provider"].as_str(), Some("custom"));
        assert_eq!(doc["model_reasoning_effort"].as_str(), Some("xhigh"));
        assert_eq!(doc["service_tier"].as_str(), Some("priority"));
        assert_eq!(doc["notify"].as_array().map(|values| values.len()), Some(2));
        assert_eq!(
            doc["model_providers"]["custom"]["name"].as_str(),
            Some("Sky2api")
        );
        assert_eq!(
            doc["model_providers"]["custom"]["base_url"].as_str(),
            Some("https://proxy.example.com/v1/")
        );
        assert_eq!(
            doc["model_providers"]["custom"]["request_max_retries"].as_integer(),
            Some(7)
        );
        assert_eq!(
            doc["model_providers"]["custom"]["http_headers"]["HTTP-Referer"].as_str(),
            Some("https://header.example.test")
        );
        assert_eq!(
            doc["model_providers"]["custom"]["http_headers"]["User-Agent"].as_str(),
            Some("Fixture agent")
        );
        assert_eq!(
            doc["model_providers"]["custom"]["env_http_headers"]["X-Project"].as_str(),
            Some("PROJECT_ID")
        );
        assert_eq!(
            doc["model_providers"]["other"]["base_url"].as_str(),
            Some("https://other.example.com/v1")
        );
        assert_eq!(
            doc["projects"]["/work/project"]["trust_level"].as_str(),
            Some("trusted")
        );
        assert_eq!(doc["desktop"]["followUpQueueMode"].as_str(), Some("queue"));
        assert_eq!(doc["desktop"]["localeOverride"].as_str(), Some("zh-CN"));
        assert_eq!(doc["windows"]["sandbox"].as_str(), Some("elevated"));
        assert_eq!(
            doc["windows"]["shell_path"].as_str(),
            Some(r"D:\Program Files\PowerShell\7\pwsh.exe")
        );
        assert_eq!(
            doc["plugins"]["browser@openai-bundled"]["enabled"].as_bool(),
            Some(true)
        );
        assert_eq!(doc["features"]["js_repl"].as_bool(), Some(false));
        assert_eq!(
            doc["shell_environment_policy"]["set"]["CODEX_HOME"].as_str(),
            Some(r"C:\Users\Thy\.codex")
        );
        assert_eq!(
            doc["mcp_servers"]["docs"]["command"].as_str(),
            Some("docs-server")
        );
        assert!(doc.get("experimental_bearer_token").is_none());
        assert!(doc["model_providers"]
            .as_table()
            .expect("model providers table")
            .iter()
            .all(|(_, item)| item
                .as_table()
                .is_none_or(|table| table.get("experimental_bearer_token").is_none())));
        assert!(!text.contains("sk-from-auth"));
        assert!(!text.contains("experimental_bearer_token"));
    }

    #[test]
    fn complete_row_without_a_model_is_not_silently_downgraded() {
        let row = CcSwitchCodexRow {
            id: "missing-model".to_string(),
            name: "Database label".to_string(),
            settings_config: json!({
                "auth": {"OPENAI_API_KEY": "sk-test"},
                "config": r#"model_provider = "custom"

[model_providers.custom]
name = "TOML label"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
"#,
            })
            .to_string(),
            category: None,
            meta: None,
        };

        assert!(build_ccswitch_codex_provider(&row, &HashMap::new())
            .unwrap()
            .is_none());
    }

    #[test]
    fn malformed_row_config_does_not_create_a_sparse_provider() {
        let section = codex_sections_from_config(
            r#"model_provider = "broken-row"
model = "gpt-5.5"

[model_providers.broken-row]
name = "Recovered only from another row"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
"#,
        )
        .into_iter()
        .next()
        .expect("global provider section");
        let mut global_sections = HashMap::new();
        global_sections.insert(section.id.clone(), section);
        let row = CcSwitchCodexRow {
            id: "broken-row".to_string(),
            name: "Broken row".to_string(),
            settings_config: json!({
                "auth": {"OPENAI_API_KEY": "sk-must-not-import"},
                "config": "model = ["
            })
            .to_string(),
            category: None,
            meta: None,
        };

        assert!(build_ccswitch_codex_provider(&row, &global_sections)
            .unwrap()
            .is_none());
    }
}
