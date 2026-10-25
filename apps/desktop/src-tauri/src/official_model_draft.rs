//! Model selector edits are limited to the supplied draft, never the live config.
use crate::error::{CodexxError, Result};
use toml_edit::{DocumentMut, Item, Value};

fn parse(config_text: &str) -> Result<DocumentMut> {
    if config_text.len() > 8 * 1024 * 1024 {
        return Err(CodexxError::Config("配置草稿过大，未修改模型。".into()));
    }
    config_text.parse::<DocumentMut>().map_err(|error| {
        let offset = error.span().map_or(0, |span| span.start);
        let prefix = config_text.get(..offset).unwrap_or_default();
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let column = prefix
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count()
            + 1;
        CodexxError::Config(format!(
            "config.toml 第 {line} 行、第 {column} 列格式无效，请修正后再选择模型。"
        ))
    })
}

fn model_from_document(doc: &DocumentMut) -> Result<Option<String>> {
    doc.get("model")
        .map(|item| {
            item.as_str().map(ToString::to_string).ok_or_else(|| {
                CodexxError::Config("config.toml 顶层 model 必须是字符串，未修改配置。".into())
            })
        })
        .transpose()
}

pub(crate) fn inspect(config_text: &str) -> Result<Option<String>> {
    model_from_document(&parse(config_text)?)
}

pub(crate) fn update(config_text: &str, model: &str) -> Result<String> {
    let model = model.trim();
    if model.len() > 256
        || model
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(CodexxError::Config(
            "模型 ID 过长或包含空白、控制字符。".into(),
        ));
    }
    let mut doc = parse(config_text)?;
    model_from_document(&doc)?;
    if model.is_empty() {
        doc.as_table_mut().remove("model");
    } else {
        let mut replacement = Value::from(model);
        if let Some(value) = doc.get("model").and_then(Item::as_value) {
            *replacement.decor_mut() = value.decor().clone();
        }
        doc["model"] = Item::Value(replacement);
    }
    Ok(doc.to_string())
}
