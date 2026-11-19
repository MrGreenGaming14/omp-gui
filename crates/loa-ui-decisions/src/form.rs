//! Form cards: fields declared by a harness or MCP server as a JSON Schema
//! object, filled in, validated and answered as one JSON object.

use serde_json::{Map, Number, Value as Json};

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum FormError {
    #[error("the field schema is not valid JSON: {0}")]
    Json(String),
    #[error("the field schema is not an object schema with `properties`")]
    NotObject,
    #[error("field `{field}` has unsupported type `{ty}`")]
    Unsupported { field: String, ty: String },
}

pub type Result<T, E = FormError> = std::result::Result<T, E>;

/// The type of one field, which picks its input widget.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldType {
    String,
    Number,
    Integer,
    Boolean,
    /// Closed set of JSON values. `labels[i]` shows `values[i]`.
    Enum {
        labels: Vec<String>,
        values: Vec<Json>,
    },
    /// `"type": "array"` whose `items` is an enum: any number of the values,
    /// answered as a JSON array in option order.
    Multi {
        labels: Vec<String>,
        values: Vec<Json>,
    },
}

/// One property of the schema.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    /// `title`, else the property name.
    pub label: String,
    pub description: Option<String>,
    pub ty: FieldType,
    pub required: bool,
}

/// What the user has entered in one field.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    /// String, number and integer fields keep the raw text.
    Text(String),
    Bool(bool),
    /// Index into the enum values.
    Choice(Option<usize>),
    /// Multi fields: which values are ticked, one slot per value.
    Picks(Vec<bool>),
}

/// Why a field was refused on submit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldError {
    Required,
    NotNumber,
    NotInteger,
}

impl std::fmt::Display for FieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Required => "Required",
            Self::NotNumber => "Enter a number",
            Self::NotInteger => "Enter a whole number",
        })
    }
}

/// A parsed schema with the values entered so far.
#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    pub fields: Vec<Field>,
    pub values: Vec<FieldValue>,
    /// Errors of the last submit, one slot per field.
    pub errors: Vec<Option<FieldError>>,
}

impl Form {
    /// Parses a JSON Schema object (`properties`, `required`, per property
    /// `type`, `enum`, `default`, `title`, `description`) and fills in the
    /// defaults.
    pub fn parse(schema: &str) -> Result<Self> {
        let schema: Json =
            serde_json::from_str(schema).map_err(|e| FormError::Json(e.to_string()))?;
        let properties = schema
            .get("properties")
            .and_then(Json::as_object)
            .ok_or(FormError::NotObject)?;
        let required: Vec<&str> = schema
            .get("required")
            .and_then(Json::as_array)
            .map(|r| r.iter().filter_map(Json::as_str).collect())
            .unwrap_or_default();
        let mut fields = Vec::with_capacity(properties.len());
        let mut values = Vec::with_capacity(properties.len());
        for (name, prop) in properties {
            let ty = field_type(name, prop)?;
            let default = prop.get("default");
            values.push(initial_value(&ty, default));
            fields.push(Field {
                name: name.clone(),
                label: prop
                    .get("title")
                    .and_then(Json::as_str)
                    .unwrap_or(name)
                    .to_owned(),
                description: prop
                    .get("description")
                    .and_then(Json::as_str)
                    .map(str::to_owned),
                ty,
                required: required.contains(&name.as_str()),
            });
        }
        let errors = vec![None; fields.len()];
        Ok(Self {
            fields,
            values,
            errors,
        })
    }

    /// Replaces the value of field `index`. Clears that field's error.
    pub fn set(&mut self, index: usize, value: FieldValue) {
        if let (Some(slot), Some(error)) = (self.values.get_mut(index), self.errors.get_mut(index))
        {
            *slot = value;
            *error = None;
        }
    }

    /// Ticks or unticks value `option` of the multi field `index`. Clears
    /// that field's error.
    pub fn toggle(&mut self, index: usize, option: usize) {
        if let (Some(FieldValue::Picks(picks)), Some(error)) =
            (self.values.get_mut(index), self.errors.get_mut(index))
            && let Some(pick) = picks.get_mut(option)
        {
            *pick = !*pick;
            *error = None;
        }
    }

    /// Validates every field. On success returns the answer: one JSON object
    /// with typed values, leaving out optional fields left empty. On failure
    /// records an error per bad field and returns `None`.
    pub fn submit(&mut self) -> Option<String> {
        let mut object = Map::with_capacity(self.fields.len());
        let mut ok = true;
        for ((field, value), error) in self.fields.iter().zip(&self.values).zip(&mut self.errors) {
            *error = None;
            match convert(field, value) {
                Ok(Some(json)) => {
                    object.insert(field.name.clone(), json);
                }
                Ok(None) => {}
                Err(e) => {
                    *error = Some(e);
                    ok = false;
                }
            }
        }
        ok.then(|| Json::Object(object).to_string())
    }
}

fn field_type(name: &str, prop: &Json) -> Result<FieldType> {
    let enumeration = |p: &Json| {
        p.get("enum").and_then(Json::as_array).map(|values| {
            let labels = values
                .iter()
                .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                .collect();
            (labels, values.clone())
        })
    };
    if let Some((labels, values)) = enumeration(prop) {
        return Ok(FieldType::Enum { labels, values });
    }
    if prop.get("type").and_then(Json::as_str) == Some("array")
        && let Some((labels, values)) = prop.get("items").and_then(enumeration)
    {
        return Ok(FieldType::Multi { labels, values });
    }
    // `"type": ["string", "null"]` is a nullable string.
    let ty = match prop.get("type") {
        None => "string",
        Some(Json::String(t)) => t.as_str(),
        Some(Json::Array(types)) => {
            let mut named = types
                .iter()
                .filter_map(Json::as_str)
                .filter(|t| *t != "null");
            match (named.next(), named.next()) {
                (Some(t), None) => t,
                _ => {
                    return Err(FormError::Unsupported {
                        field: name.to_owned(),
                        ty: Json::Array(types.clone()).to_string(),
                    });
                }
            }
        }
        Some(other) => {
            return Err(FormError::Unsupported {
                field: name.to_owned(),
                ty: other.to_string(),
            });
        }
    };
    match ty {
        "string" => Ok(FieldType::String),
        "number" => Ok(FieldType::Number),
        "integer" => Ok(FieldType::Integer),
        "boolean" => Ok(FieldType::Boolean),
        other => Err(FormError::Unsupported {
            field: name.to_owned(),
            ty: other.to_owned(),
        }),
    }
}

fn initial_value(ty: &FieldType, default: Option<&Json>) -> FieldValue {
    match ty {
        FieldType::Boolean => FieldValue::Bool(default.and_then(Json::as_bool).unwrap_or(false)),
        FieldType::Enum { values, .. } => {
            FieldValue::Choice(default.and_then(|d| values.iter().position(|v| v == d)))
        }
        FieldType::Multi { values, .. } => {
            let defaults = default.and_then(Json::as_array);
            FieldValue::Picks(
                values
                    .iter()
                    .map(|v| defaults.is_some_and(|d| d.contains(v)))
                    .collect(),
            )
        }
        FieldType::String | FieldType::Number | FieldType::Integer => {
            FieldValue::Text(match default {
                None | Some(Json::Null) => String::new(),
                Some(Json::String(s)) => s.clone(),
                Some(other) => other.to_string(),
            })
        }
    }
}

fn convert(field: &Field, value: &FieldValue) -> std::result::Result<Option<Json>, FieldError> {
    match (&field.ty, value) {
        (FieldType::Boolean, FieldValue::Bool(b)) => Ok(Some(Json::Bool(*b))),
        (FieldType::Enum { values, .. }, FieldValue::Choice(choice)) => {
            match choice.and_then(|i| values.get(i)) {
                Some(v) => Ok(Some(v.clone())),
                None if field.required => Err(FieldError::Required),
                None => Ok(None),
            }
        }
        (FieldType::Multi { values, .. }, FieldValue::Picks(picks)) => {
            let chosen: Vec<Json> = values
                .iter()
                .zip(picks)
                .filter(|(_, picked)| **picked)
                .map(|(v, _)| v.clone())
                .collect();
            match chosen.is_empty() {
                false => Ok(Some(Json::Array(chosen))),
                true if field.required => Err(FieldError::Required),
                true => Ok(None),
            }
        }
        (ty, FieldValue::Text(raw)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return if field.required {
                    Err(FieldError::Required)
                } else {
                    Ok(None)
                };
            }
            match ty {
                FieldType::Number => trimmed
                    .parse::<f64>()
                    .ok()
                    .and_then(Number::from_f64)
                    .map(|n| Some(Json::Number(n)))
                    .ok_or(FieldError::NotNumber),
                FieldType::Integer => trimmed
                    .parse::<i64>()
                    .map(|n| Some(Json::Number(n.into())))
                    .map_err(|_| FieldError::NotInteger),
                _ => Ok(Some(Json::String(raw.clone()))),
            }
        }
        // `Form::set` callers pass the variant the field type takes; a
        // mismatched value counts as empty.
        _ if field.required => Err(FieldError::Required),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r#"{
        "type": "object",
        "properties": {
            "branch": {"type": "string", "title": "Branch name", "description": "Where to push", "default": "main"},
            "retries": {"type": "integer", "default": 3},
            "ratio": {"type": "number"},
            "force": {"type": "boolean"},
            "env": {"enum": ["dev", "prod"], "default": "dev"},
            "note": {"type": ["string", "null"]}
        },
        "required": ["branch", "ratio", "env"]
    }"#;

    #[test]
    fn schema_fields_keep_order_types_labels_and_defaults() {
        let form = Form::parse(SCHEMA).unwrap();
        let names: Vec<&str> = form.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["branch", "retries", "ratio", "force", "env", "note"]
        );
        assert_eq!(form.fields[0].label, "Branch name");
        assert_eq!(form.fields[0].description.as_deref(), Some("Where to push"));
        assert!(form.fields[0].required && !form.fields[1].required);
        assert_eq!(form.fields[1].ty, FieldType::Integer);
        assert_eq!(form.fields[5].ty, FieldType::String);
        assert_eq!(
            form.values,
            [
                FieldValue::Text("main".into()),
                FieldValue::Text("3".into()),
                FieldValue::Text(String::new()),
                FieldValue::Bool(false),
                FieldValue::Choice(Some(0)),
                FieldValue::Text(String::new()),
            ]
        );
    }

    #[test]
    fn submit_refuses_missing_and_malformed_values() {
        let mut form = Form::parse(SCHEMA).unwrap();
        form.set(0, FieldValue::Text("  ".into()));
        form.set(1, FieldValue::Text("2.5".into()));
        assert_eq!(form.submit(), None);
        assert_eq!(form.errors[0], Some(FieldError::Required));
        assert_eq!(form.errors[1], Some(FieldError::NotInteger));
        assert_eq!(form.errors[2], Some(FieldError::Required));

        form.set(2, FieldValue::Text("abc".into()));
        assert_eq!(form.submit(), None);
        assert_eq!(form.errors[2], Some(FieldError::NotNumber));
    }

    #[test]
    fn submit_answers_one_typed_json_object() {
        let mut form = Form::parse(SCHEMA).unwrap();
        form.set(1, FieldValue::Text("5".into()));
        form.set(2, FieldValue::Text("0.5".into()));
        form.set(3, FieldValue::Bool(true));
        form.set(4, FieldValue::Choice(Some(1)));
        let answer = form.submit().unwrap();
        assert_eq!(
            answer,
            r#"{"branch":"main","retries":5,"ratio":0.5,"force":true,"env":"prod"}"#
        );
        assert!(form.errors.iter().all(Option::is_none));
    }

    #[test]
    fn array_of_enum_is_a_multi_select_answered_as_an_array() {
        let mut form = Form::parse(
            r#"{"properties":{"tags":{"type":"array","items":{"enum":["a","b","c"]},"default":["c"]}},"required":["tags"]}"#,
        )
        .unwrap();
        assert_eq!(
            form.fields[0].ty,
            FieldType::Multi {
                labels: vec!["a".into(), "b".into(), "c".into()],
                values: vec!["a".into(), "b".into(), "c".into()],
            }
        );
        assert_eq!(form.values[0], FieldValue::Picks(vec![false, false, true]));
        form.toggle(0, 2);
        assert_eq!(form.submit(), None);
        assert_eq!(form.errors[0], Some(FieldError::Required));
        form.toggle(0, 1);
        form.toggle(0, 0);
        form.toggle(0, 9);
        assert_eq!(form.submit().as_deref(), Some(r#"{"tags":["a","b"]}"#));
    }

    #[test]
    fn unsupported_schemas_are_reported() {
        assert!(matches!(Form::parse("not json"), Err(FormError::Json(_))));
        assert_eq!(
            Form::parse(r#"{"type":"string"}"#),
            Err(FormError::NotObject)
        );
        assert_eq!(
            Form::parse(r#"{"properties":{"tags":{"type":"array"}}}"#),
            Err(FormError::Unsupported {
                field: "tags".into(),
                ty: "array".into()
            })
        );
    }
}
