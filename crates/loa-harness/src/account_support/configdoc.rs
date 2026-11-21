//! Shared string-setting edits for harness configuration documents.

use std::str::FromStr;

use loa_util::fs::Snapshot;
use serde_json::{Map, Value};
use toml_edit::{DocumentMut, Item, Table, value};

use loa_settings::models::error::{Error, Result, save_snapshot};

pub trait ConfigDoc: Sized {
    fn parse(text: &str, what: &str) -> Result<Self>;
    fn get(&self, path: &[&str]) -> Option<String>;
    fn set(&mut self, path: &[&str], value: Option<&str>) -> Result<()>;
    fn render(&self) -> String;
}

pub(crate) fn edit_file<D: ConfigDoc>(snapshot: &mut Snapshot, text: &str, edit: impl FnOnce(&mut D) -> Result<()>) -> Result<bool> {
    let mut doc = D::parse(text, &snapshot.path().display().to_string())?;
    let before = doc.render();
    edit(&mut doc)?;
    let after = doc.render();
    if before == after { return Ok(false); }
    // Check the rendered document before committing it.
    D::parse(&after, &snapshot.path().display().to_string())?;
    save_snapshot(snapshot, &after)?;
    Ok(true)
}

pub(crate) struct JsonDoc {
    pub(crate) root: Map<String, Value>,
    what: String,
}

impl ConfigDoc for JsonDoc {
    fn parse(text: &str, what: &str) -> Result<Self> {
        let value: Value = if text.trim().is_empty() { Value::Object(Map::new()) } else {
            serde_json::from_str(text).map_err(|e| Error::Parse { what: what.into(), message: e.to_string() })?
        };
        let Value::Object(root) = value else {
            return Err(Error::Unsupported { what: what.into(), reason: "its root is not an object".into() });
        };
        Ok(Self { root, what: what.into() })
    }
    fn get(&self, path: &[&str]) -> Option<String> {
        let (first, rest) = path.split_first()?;
        let mut item = self.root.get(*first)?;
        for key in rest { item = item.as_object()?.get(*key)?; }
        item.as_str().map(str::to_owned)
    }
    fn set(&mut self, path: &[&str], value: Option<&str>) -> Result<()> {
        let (key, parents) = path.split_last().ok_or_else(|| Error::invalid("path", "empty key path"))?;
        let mut map = &mut self.root;
        for parent in parents {
            if value.is_none() && !map.contains_key(*parent) { return Ok(()); }
            let item = map.entry((*parent).to_owned()).or_insert_with(|| Value::Object(Map::new()));
            if value.is_none() && !item.is_object() { return Ok(()); }
            map = item.as_object_mut().ok_or_else(|| Error::Unsupported { what: self.what.clone(), reason: format!("{parent:?} is not an object") })?;
        }
        match value {
            Some(v) => { map.insert((*key).to_owned(), Value::String(v.to_owned())); }
            None => { map.shift_remove(*key); }
        }
        Ok(())
    }
    fn render(&self) -> String {
        let mut text = serde_json::to_string_pretty(&self.root).expect("JSON values always serialize");
        text.push('\n');
        text
    }
}

pub(crate) struct TomlDoc {
    doc: DocumentMut,
    what: String,
}

impl ConfigDoc for TomlDoc {
    fn parse(text: &str, what: &str) -> Result<Self> {
        let doc = DocumentMut::from_str(text).map_err(|e| Error::Parse { what: what.into(), message: e.to_string() })?;
        Ok(Self { doc, what: what.into() })
    }
    fn get(&self, path: &[&str]) -> Option<String> {
        let (first, rest) = path.split_first()?;
        let mut item = self.doc.get(first)?;
        for key in rest { item = item.as_table_like()?.get(key)?; }
        item.as_str().map(str::to_owned)
    }
    fn set(&mut self, path: &[&str], setting: Option<&str>) -> Result<()> {
        let (key, parents) = path.split_last().ok_or_else(|| Error::invalid("path", "empty key path"))?;
        let mut table: &mut dyn toml_edit::TableLike = self.doc.as_table_mut();
        for parent in parents {
            if setting.is_none() && !table.contains_key(parent) { return Ok(()); }
            if !table.contains_key(parent) { table.insert(parent, Item::Table(Table::new())); }
            let item = table.get_mut(parent).expect("inserted table");
            if setting.is_none() && !item.is_table_like() { return Ok(()); }
            table = item.as_table_like_mut().ok_or_else(|| Error::Unsupported { what: self.what.clone(), reason: format!("{parent:?} is not a table") })?;
        }
        match setting {
            Some(text) => {
                if let Some(existing) = table.get_mut(key).and_then(Item::as_value_mut) {
                    let decor = existing.decor().clone();
                    *existing = toml_edit::Value::from(text);
                    *existing.decor_mut() = decor;
                } else { table.insert(key, value(text)); }
            }
            None => { table.remove(key); }
        }
        Ok(())
    }
    fn render(&self) -> String { self.doc.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_edits_preserve_key_order_and_reject_non_object_parents() {
        let mut doc = JsonDoc::parse(r#"{"z":1,"nested":{"b":2,"a":"old"},"a":3}"#, "settings").unwrap();
        doc.set(&["nested", "a"], Some("new")).unwrap();
        doc.set(&["nested", "b"], None).unwrap();
        doc.set(&["nested", "c"], Some("last")).unwrap();
        assert_eq!(doc.render(), "{\n  \"z\": 1,\n  \"nested\": {\n    \"a\": \"new\",\n    \"c\": \"last\"\n  },\n  \"a\": 3\n}\n");
        assert!(matches!(doc.set(&["z", "child"], Some("x")), Err(Error::Unsupported { .. })));
        assert_eq!(doc.get(&["nested", "a"]).as_deref(), Some("new"));
    }

    #[test]
    fn toml_edits_keep_comments_and_support_nested_paths() {
        let mut doc = TomlDoc::parse("# header\n[model]\nname = 'old' # note\n", "config").unwrap();
        doc.set(&["model", "name"], Some("new")).unwrap();
        assert_eq!(doc.get(&["model", "name"]).as_deref(), Some("new"));
        assert_eq!(doc.render(), "# header\n[model]\nname = \"new\" # note\n");
    }
}
