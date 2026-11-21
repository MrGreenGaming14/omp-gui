//! Form cards: fields declared as a JSON Schema object, filled in, validated and answered as one
//! JSON object.

use serde_json::{Map, Number, Value as Json};

#[derive(Clone, Debug, PartialEq)]
pub enum FormError {
    Json(String),
    NotObject,
    Unsupported { field: String, ty: String },
}

impl std::fmt::Display for FormError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(e) => write!(f, "the field schema is not valid JSON: {e}"),
            Self::NotObject => write!(f, "the field schema is not an object schema with `properties`"),
            Self::Unsupported { field, ty } => write!(f, "field `{field}` has unsupported type `{ty}`"),
        }
    }
}

/// A field's type, which picks its input widget. Enum and multi `labels[i]` show `values[i]`;
/// multi is an array of an enum, answered as a JSON array in option order.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldType {
    String,
    Number,
    Integer,
    Boolean,
    Enum { labels: Vec<String>, values: Vec<Json> },
    Multi { labels: Vec<String>, values: Vec<Json> },
}

/// One property of the schema; `label` is its `title`, else its name.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub label: String,
    pub description: Option<String>,
    pub ty: FieldType,
    pub required: bool,
}

/// What the user entered: raw text of string and number fields, an enum index, multi ticks.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Text(String),
    Bool(bool),
    Choice(Option<usize>),
    Picks(Vec<bool>),
}

/// Why a field was refused on submit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldError {
    Required,
    NotNumber,
    NotInteger,
}

impl std::fmt::Display for FieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(["Required", "Enter a number", "Enter a whole number"][*self as usize]) }
}

/// A parsed schema with the values entered so far and the errors of the last submit.
#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    pub fields: Vec<Field>,
    pub values: Vec<FieldValue>,
    pub errors: Vec<Option<FieldError>>,
}

impl Form {
    /// Parses `properties`, `required` and per property `type`, `enum`, `default`, `title`,
    /// `description`, filling in the defaults.
    pub fn parse(schema: &str) -> Result<Self, FormError> {
        let schema: Json = serde_json::from_str(schema).map_err(|e| FormError::Json(e.to_string()))?;
        let properties = schema.get("properties").and_then(Json::as_object).ok_or(FormError::NotObject)?;
        let required: Vec<&str> = schema.get("required").and_then(Json::as_array).map(|r| r.iter().filter_map(Json::as_str).collect()).unwrap_or_default();
        let mut form = Self { fields: Vec::new(), values: Vec::new(), errors: vec![None; properties.len()] };
        for (name, prop) in properties {
            let ty = field_type(name, prop)?;
            form.values.push(initial_value(&ty, prop.get("default")));
            let text = |key| prop.get(key).and_then(Json::as_str).map(str::to_owned);
            form.fields.push(Field { name: name.clone(), label: text("title").unwrap_or_else(|| name.clone()), description: text("description"), ty, required: required.contains(&name.as_str()) });
        }
        Ok(form)
    }

    /// Replaces the value of field `index` and clears its error.
    pub fn set(&mut self, index: usize, value: FieldValue) {
        if let (Some(slot), Some(error)) = (self.values.get_mut(index), self.errors.get_mut(index)) {
            (*slot, *error) = (value, None);
        }
    }

    /// Ticks or unticks value `option` of multi field `index`.
    pub fn toggle(&mut self, index: usize, option: usize) {
        if let Some(FieldValue::Picks(picks)) = self.values.get(index) {
            let mut picks = picks.clone();
            if let Some(pick) = picks.get_mut(option) {
                *pick = !*pick;
                self.set(index, FieldValue::Picks(picks));
            }
        }
    }

    /// The answer when every field is valid: one JSON object of typed values, leaving out empty
    /// optional fields. Otherwise records the errors.
    pub fn submit(&mut self) -> Option<String> {
        let mut object = Map::new();
        for ((field, value), error) in self.fields.iter().zip(&self.values).zip(&mut self.errors) {
            match convert(field, value) {
                Ok(json) => {
                    *error = None;
                    object.extend(json.map(|j| (field.name.clone(), j)));
                }
                Err(e) => *error = Some(e),
            }
        }
        self.errors.iter().all(Option::is_none).then(|| Json::Object(object).to_string())
    }
}

fn unsupported(field: &str, ty: impl ToString) -> FormError { FormError::Unsupported { field: field.to_owned(), ty: ty.to_string() } }

fn field_type(name: &str, prop: &Json) -> Result<FieldType, FormError> {
    let enumeration = |p: &Json| p.get("enum").and_then(Json::as_array).map(|values| (values.iter().map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned)).collect(), values.clone()));
    if let Some((labels, values)) = enumeration(prop) {
        return Ok(FieldType::Enum { labels, values });
    }
    if prop.get("type").and_then(Json::as_str) == Some("array")
        && let Some((labels, values)) = prop.get("items").and_then(enumeration)
    {
        return Ok(FieldType::Multi { labels, values });
    }
    // `["string", "null"]` is a nullable string.
    let ty = match prop.get("type") {
        None => "string",
        Some(Json::String(t)) => t.as_str(),
        Some(Json::Array(types)) => match types.iter().filter_map(Json::as_str).filter(|t| *t != "null").collect::<Vec<_>>()[..] {
            [t] => t,
            _ => return Err(unsupported(name, Json::Array(types.clone()))),
        },
        Some(other) => return Err(unsupported(name, other)),
    };
    Ok(match ty {
        "string" => FieldType::String,
        "number" => FieldType::Number,
        "integer" => FieldType::Integer,
        "boolean" => FieldType::Boolean,
        other => return Err(unsupported(name, other)),
    })
}

fn initial_value(ty: &FieldType, default: Option<&Json>) -> FieldValue {
    match ty {
        FieldType::Boolean => FieldValue::Bool(default.and_then(Json::as_bool).unwrap_or(false)),
        FieldType::Enum { values, .. } => FieldValue::Choice(default.and_then(|d| values.iter().position(|v| v == d))),
        FieldType::Multi { values, .. } => FieldValue::Picks(values.iter().map(|v| default.and_then(Json::as_array).is_some_and(|d| d.contains(v))).collect()),
        _ => FieldValue::Text(match default {
            None | Some(Json::Null) => String::new(),
            Some(Json::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        }),
    }
}

fn convert(field: &Field, value: &FieldValue) -> Result<Option<Json>, FieldError> {
    let json = match (&field.ty, value) {
        (FieldType::Boolean, FieldValue::Bool(b)) => Some(Json::Bool(*b)),
        (FieldType::Enum { values, .. }, FieldValue::Choice(choice)) => choice.and_then(|i| values.get(i)).cloned(),
        (FieldType::Multi { values, .. }, FieldValue::Picks(picks)) => {
            let chosen: Vec<Json> = values.iter().zip(picks).filter(|(_, p)| **p).map(|(v, _)| v.clone()).collect();
            (!chosen.is_empty()).then_some(Json::Array(chosen))
        }
        (ty, FieldValue::Text(raw)) if !raw.trim().is_empty() => Some(match ty {
            FieldType::Number => Json::Number(raw.trim().parse::<f64>().ok().and_then(Number::from_f64).ok_or(FieldError::NotNumber)?),
            FieldType::Integer => Json::Number(raw.trim().parse::<i64>().map_err(|_| FieldError::NotInteger)?.into()),
            _ => Json::String(raw.clone()),
        }),
        // Empty text, or a value of the wrong variant, counts as empty.
        _ => None,
    };
    if json.is_none() && field.required { Err(FieldError::Required) } else { Ok(json) }
}
