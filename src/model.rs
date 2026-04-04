use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Trait for types that carry an `id` field (used for list→map conversion)
// ---------------------------------------------------------------------------

pub trait HasId {
    fn take_id(&mut self) -> Option<String>;

    /// Construct an instance from a bare type string (e.g. `"File"`, `"int?"`).
    /// Used for CWL shorthand syntax like `inputs: { file1: File }`.
    /// Returns `None` if this type does not support shorthand construction.
    fn from_type_str(_type_str: &str) -> Option<Self>
    where
        Self: Sized,
    {
        None
    }
}

// ---------------------------------------------------------------------------
// Custom deserializer: map-or-list for inputs/outputs/steps
// ---------------------------------------------------------------------------

/// Deserialize a field that may be either:
/// - A YAML mapping `{key: value, ...}` → `HashMap<String, V>`
/// - A YAML sequence `[{id: key, ...}, ...]` → `HashMap<String, V>` keyed by `id`
pub fn deserialize_map_or_list<'de, D, V>(deserializer: D) -> Result<HashMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: serde::de::DeserializeOwned + HasId,
{
    use serde::de::{self, MapAccess, SeqAccess, Visitor};
    use std::fmt;
    use std::marker::PhantomData;

    struct MapOrListVisitor<V>(PhantomData<V>);

    impl<'de, V> Visitor<'de> for MapOrListVisitor<V>
    where
        V: serde::de::DeserializeOwned + HasId,
    {
        type Value = HashMap<String, V>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a map or a list of objects with 'id' fields")
        }

        fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            let mut result = HashMap::new();
            while let Some(key) = map.next_key::<String>()? {
                // Deserialize as intermediate Value first to detect bare strings
                let raw: serde_yaml::Value = map.next_value()?;
                let value = match &raw {
                    serde_yaml::Value::String(s) => {
                        // Try shorthand type syntax first (e.g. "File", "int?")
                        match V::from_type_str(s) {
                            Some(v) => v,
                            // Fall back to normal deserialization (e.g. StepInput
                            // has its own string handling)
                            None => serde_yaml::from_value(raw)
                                .map_err(de::Error::custom)?,
                        }
                    }
                    _ => {
                        // Full struct (mapping) — deserialize normally
                        serde_yaml::from_value(raw).map_err(de::Error::custom)?
                    }
                };
                result.insert(key, value);
            }
            Ok(result)
        }

        fn visit_seq<S>(self, mut seq: S) -> Result<Self::Value, S::Error>
        where
            S: SeqAccess<'de>,
        {
            let mut result = HashMap::new();
            while let Some(mut item) = seq.next_element::<V>()? {
                let id = item.take_id().ok_or_else(|| {
                    de::Error::custom("list-form entry missing required 'id' field")
                })?;
                result.insert(id, item);
            }
            Ok(result)
        }
    }

    deserializer.deserialize_any(MapOrListVisitor(PhantomData))
}

// ---------------------------------------------------------------------------
// Custom deserializer: list-or-map for requirements/hints
// ---------------------------------------------------------------------------

/// Deserialize requirements/hints that may be either:
/// - A YAML sequence `[{class: X, ...}, ...]` (current/standard form)
/// - A YAML mapping `{X: {...}, ...}` → converted to `[{class: X, ...}, ...]`
pub fn deserialize_requirements<'de, D>(
    deserializer: D,
) -> Result<Vec<serde_yaml::Value>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::{self, MapAccess, SeqAccess, Visitor};
    use std::fmt;

    struct ReqVisitor;

    impl<'de> Visitor<'de> for ReqVisitor {
        type Value = Vec<serde_yaml::Value>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a list or map of requirements")
        }

        fn visit_seq<S>(self, mut seq: S) -> Result<Self::Value, S::Error>
        where
            S: SeqAccess<'de>,
        {
            let mut result = Vec::new();
            while let Some(item) = seq.next_element::<serde_yaml::Value>()? {
                result.push(item);
            }
            Ok(result)
        }

        fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            let mut result = Vec::new();
            while let Some((key, value)) =
                map.next_entry::<String, serde_yaml::Value>()?
            {
                let mut mapping = match value {
                    serde_yaml::Value::Mapping(m) => m,
                    serde_yaml::Value::Null => serde_yaml::Mapping::new(),
                    other => {
                        return Err(de::Error::custom(format!(
                            "expected mapping for requirement '{}', got {:?}",
                            key, other
                        )));
                    }
                };
                mapping.insert(
                    serde_yaml::Value::String("class".to_string()),
                    serde_yaml::Value::String(key),
                );
                result.push(serde_yaml::Value::Mapping(mapping));
            }
            Ok(result)
        }
    }

    deserializer.deserialize_any(ReqVisitor)
}

// ---------------------------------------------------------------------------
// Top-level CWL document
// ---------------------------------------------------------------------------

/// A CWL document is either a CommandLineTool, ExpressionTool, or a Workflow.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "class")]
pub enum CwlDocument {
    CommandLineTool(CommandLineTool),
    ExpressionTool(ExpressionTool),
    Workflow(Workflow),
}

// ---------------------------------------------------------------------------
// ExpressionTool
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionTool {
    #[serde(default)]
    pub cwl_version: Option<String>,

    #[serde(default)]
    pub label: Option<String>,

    #[serde(default)]
    pub doc: Option<String>,

    #[serde(default, deserialize_with = "deserialize_requirements")]
    pub requirements: Vec<serde_yaml::Value>,

    #[serde(default, deserialize_with = "deserialize_map_or_list")]
    pub inputs: HashMap<String, ToolInput>,

    #[serde(default, deserialize_with = "deserialize_map_or_list")]
    pub outputs: HashMap<String, ToolOutput>,

    #[serde(default)]
    pub expression: Option<String>,
}

// ---------------------------------------------------------------------------
// CommandLineTool
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandLineTool {
    #[serde(default)]
    pub cwl_version: Option<String>,

    #[serde(default)]
    pub label: Option<String>,

    #[serde(default)]
    pub doc: Option<String>,

    #[serde(default)]
    pub base_command: BaseCommand,

    #[serde(default)]
    pub arguments: Vec<Argument>,

    #[serde(default, deserialize_with = "deserialize_map_or_list")]
    pub inputs: HashMap<String, ToolInput>,

    #[serde(default, deserialize_with = "deserialize_map_or_list")]
    pub outputs: HashMap<String, ToolOutput>,

    #[serde(default, deserialize_with = "deserialize_requirements")]
    pub requirements: Vec<serde_yaml::Value>,

    #[serde(default, deserialize_with = "deserialize_requirements")]
    pub hints: Vec<serde_yaml::Value>,

    #[serde(default)]
    pub stdout: Option<String>,

    #[serde(default)]
    pub stdin: Option<String>,

    #[serde(default)]
    pub stderr: Option<String>,
}

// ---------------------------------------------------------------------------
// BaseCommand — string, array of strings, or absent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
#[derive(Default)]
pub enum BaseCommand {
    Single(String),
    Array(Vec<String>),
    #[default]
    None,
}


// ---------------------------------------------------------------------------
// Argument — plain string or structured entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Argument {
    String(String),
    Structured(ArgumentEntry),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgumentEntry {
    #[serde(default)]
    pub prefix: Option<String>,

    #[serde(default)]
    pub value_from: Option<String>,

    #[serde(default)]
    pub position: Option<i32>,

    #[serde(default)]
    pub shell_quote: Option<bool>,
}

// ---------------------------------------------------------------------------
// Tool inputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInput {
    #[serde(default)]
    pub id: Option<String>,

    #[serde(rename = "type")]
    pub cwl_type: CwlType,

    #[serde(default)]
    pub input_binding: Option<InputBinding>,

    #[serde(default)]
    pub secondary_files: Vec<SecondaryFile>,

    #[serde(default)]
    pub doc: Option<String>,

    #[serde(default)]
    pub default: Option<serde_yaml::Value>,

    #[serde(default)]
    pub load_contents: Option<bool>,
}

impl HasId for ToolInput {
    fn take_id(&mut self) -> Option<String> {
        self.id.take()
    }

    fn from_type_str(type_str: &str) -> Option<Self> {
        Some(ToolInput {
            id: None,
            cwl_type: CwlType::Single(type_str.to_string()),
            input_binding: None,
            secondary_files: Vec::new(),
            doc: None,
            default: None,
            load_contents: None,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputBinding {
    #[serde(default)]
    pub position: Option<i32>,

    #[serde(default)]
    pub prefix: Option<String>,

    #[serde(default)]
    pub separate: Option<bool>,

    #[serde(default)]
    pub shell_quote: Option<bool>,

    #[serde(default)]
    pub value_from: Option<String>,

    #[serde(default)]
    pub item_separator: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool outputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutput {
    #[serde(default)]
    pub id: Option<String>,

    #[serde(rename = "type")]
    pub cwl_type: CwlType,

    #[serde(default)]
    pub output_binding: Option<OutputBinding>,

    #[serde(default)]
    pub secondary_files: Vec<SecondaryFile>,

    #[serde(default)]
    pub doc: Option<String>,

    #[serde(default)]
    pub format: Option<String>,
}

impl HasId for ToolOutput {
    fn take_id(&mut self) -> Option<String> {
        self.id.take()
    }

    fn from_type_str(type_str: &str) -> Option<Self> {
        Some(ToolOutput {
            id: None,
            cwl_type: CwlType::Single(type_str.to_string()),
            output_binding: None,
            secondary_files: Vec::new(),
            doc: None,
            format: None,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputBinding {
    #[serde(default)]
    pub glob: GlobPattern,

    #[serde(default)]
    pub load_contents: Option<bool>,

    #[serde(default)]
    pub output_eval: Option<String>,
}

// ---------------------------------------------------------------------------
// GlobPattern — single string, array, or absent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
#[derive(Default)]
pub enum GlobPattern {
    Single(String),
    Array(Vec<String>),
    #[default]
    None,
}


// ---------------------------------------------------------------------------
// CwlType — single string or array of strings (for union types)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub enum CwlType {
    /// A simple type string: "File", "string", "int", "File?", "File[]"
    Single(String),
    /// A union type: ["null", "File"]
    Union(Vec<CwlType>),
    /// A structured array type: {type: array, items: File, inputBinding?: ...}
    ArrayType {
        items: Box<CwlType>,
        /// Optional per-element inputBinding from the type definition.
        inner_binding: Option<InputBinding>,
    },
}

impl<'de> Deserialize<'de> for CwlType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, SeqAccess, Visitor};
        use std::fmt;

        struct CwlTypeVisitor;

        impl<'de> Visitor<'de> for CwlTypeVisitor {
            type Value = CwlType;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a CWL type (string, array, or {type: array, items: ...})")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<CwlType, E> {
                Ok(CwlType::Single(v.to_string()))
            }

            fn visit_seq<S>(self, mut seq: S) -> Result<CwlType, S::Error>
            where
                S: SeqAccess<'de>,
            {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element::<CwlType>()? {
                    items.push(item);
                }
                Ok(CwlType::Union(items))
            }

            fn visit_map<M>(self, mut map: M) -> Result<CwlType, M::Error>
            where
                M: MapAccess<'de>,
            {
                // Parse {type: "array", items: <CwlType>, inputBinding?: ...}
                let mut type_field: Option<String> = None;
                let mut items_field: Option<CwlType> = None;
                let mut inner_binding: Option<InputBinding> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "type" => {
                            type_field = Some(map.next_value()?);
                        }
                        "items" => {
                            items_field = Some(map.next_value()?);
                        }
                        "inputBinding" => {
                            inner_binding = Some(map.next_value()?);
                        }
                        _ => {
                            let _: serde_yaml::Value = map.next_value()?;
                        }
                    }
                }

                match (type_field.as_deref(), items_field) {
                    (Some("array"), Some(items)) => {
                        Ok(CwlType::ArrayType {
                            items: Box::new(items),
                            inner_binding,
                        })
                    }
                    (Some(t), _) => {
                        // Other structured types - just return as single
                        Ok(CwlType::Single(t.to_string()))
                    }
                    _ => Err(de::Error::custom("structured type missing 'type' field")),
                }
            }
        }

        deserializer.deserialize_any(CwlTypeVisitor)
    }
}

impl CwlType {
    /// Return the base type name, stripping optional "?" suffix and "[]" array
    /// suffix. For union arrays like `["null", "File"]`, return the first
    /// non-"null" element. For structured array types, return the items type.
    pub fn base_type(&self) -> &str {
        match self {
            CwlType::Single(s) => {
                let s = s.trim_end_matches('?');
                let s = s.trim_end_matches("[]");
                s
            }
            CwlType::Union(v) => {
                for item in v {
                    match item {
                        CwlType::Single(s) if s == "null" => continue,
                        other => return other.base_type(),
                    }
                }
                "null"
            }
            CwlType::ArrayType { items, .. } => items.base_type(),
        }
    }

    /// Returns true if the type is optional (nullable).
    /// - `"File?"` -> true
    /// - `["null", "File"]` -> true
    /// - `"File"` -> false
    pub fn is_optional(&self) -> bool {
        match self {
            CwlType::Single(s) => s.ends_with('?'),
            CwlType::Union(v) => v.iter().any(|item| {
                matches!(item, CwlType::Single(s) if s == "null")
            }),
            CwlType::ArrayType { .. } => false,
        }
    }

    /// Returns true if the type represents an array type (e.g. `"File[]"` or `{type: array, items: File}`).
    pub fn is_array(&self) -> bool {
        match self {
            CwlType::Single(s) => s.ends_with("[]"),
            CwlType::Union(_) => false,
            CwlType::ArrayType { .. } => true,
        }
    }
}

// ---------------------------------------------------------------------------
// SecondaryFile — plain pattern string or structured entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SecondaryFile {
    Pattern(String),
    Structured(SecondaryFileEntry),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecondaryFileEntry {
    pub pattern: String,

    #[serde(default)]
    pub required: Option<bool>,
}

// ---------------------------------------------------------------------------
// Workflow
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    #[serde(default)]
    pub cwl_version: Option<String>,

    #[serde(default)]
    pub label: Option<String>,

    #[serde(default)]
    pub doc: Option<String>,

    #[serde(default, deserialize_with = "deserialize_map_or_list")]
    pub inputs: HashMap<String, WorkflowInput>,

    #[serde(default, deserialize_with = "deserialize_map_or_list")]
    pub outputs: HashMap<String, WorkflowOutput>,

    #[serde(default, deserialize_with = "deserialize_map_or_list")]
    pub steps: HashMap<String, WorkflowStep>,

    #[serde(default, deserialize_with = "deserialize_requirements")]
    pub requirements: Vec<serde_yaml::Value>,
}

// ---------------------------------------------------------------------------
// Workflow inputs / outputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowInput {
    #[serde(default)]
    pub id: Option<String>,

    #[serde(rename = "type")]
    pub cwl_type: CwlType,

    #[serde(default)]
    pub secondary_files: Vec<SecondaryFile>,

    #[serde(default)]
    pub doc: Option<String>,

    #[serde(default)]
    pub default: Option<serde_yaml::Value>,
}

impl HasId for WorkflowInput {
    fn take_id(&mut self) -> Option<String> {
        self.id.take()
    }

    fn from_type_str(type_str: &str) -> Option<Self> {
        Some(WorkflowInput {
            id: None,
            cwl_type: CwlType::Single(type_str.to_string()),
            secondary_files: Vec::new(),
            doc: None,
            default: None,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowOutput {
    #[serde(default)]
    pub id: Option<String>,

    #[serde(rename = "type")]
    pub cwl_type: CwlType,

    #[serde(default)]
    pub output_source: Option<String>,

    #[serde(default)]
    pub doc: Option<String>,
}

impl HasId for WorkflowOutput {
    fn take_id(&mut self) -> Option<String> {
        self.id.take()
    }

    fn from_type_str(type_str: &str) -> Option<Self> {
        Some(WorkflowOutput {
            id: None,
            cwl_type: CwlType::Single(type_str.to_string()),
            output_source: None,
            doc: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Workflow steps
// ---------------------------------------------------------------------------

/// `run` field of a workflow step: either a path string or an inline tool definition.
#[derive(Debug, Clone, Serialize)]
pub enum StepRun {
    Path(String),
    Inline(Box<CwlDocument>),
}

impl PartialEq<&str> for StepRun {
    fn eq(&self, other: &&str) -> bool {
        match self {
            StepRun::Path(p) => p.as_str() == *other,
            StepRun::Inline(_) => false,
        }
    }
}

impl<'de> Deserialize<'de> for StepRun {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        match &value {
            serde_yaml::Value::String(s) => Ok(StepRun::Path(s.clone())),
            serde_yaml::Value::Mapping(_) => {
                let doc: CwlDocument =
                    serde_yaml::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(StepRun::Inline(Box::new(doc)))
            }
            _ => Err(serde::de::Error::custom(format!(
                "expected string or mapping for step 'run', got {:?}",
                value
            ))),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStep {
    #[serde(default)]
    pub id: Option<String>,

    pub run: StepRun,

    #[serde(rename = "in", default, deserialize_with = "deserialize_map_or_list")]
    pub inputs: HashMap<String, StepInput>,

    #[serde(default)]
    pub out: StepOutputList,

    #[serde(default)]
    pub scatter: Option<ScatterField>,

    #[serde(default)]
    pub scatter_method: Option<String>,
}

impl HasId for WorkflowStep {
    fn take_id(&mut self) -> Option<String> {
        self.id.take()
    }
}

/// Step `out` can be a list of plain strings or a list of `{id: ...}` objects.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct StepOutputList(pub Vec<String>);

impl std::ops::Deref for StepOutputList {
    type Target = Vec<String>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'de> Deserialize<'de> for StepOutputList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::{SeqAccess, Visitor};
        use std::fmt;

        struct StepOutVisitor;

        impl<'de> Visitor<'de> for StepOutVisitor {
            type Value = StepOutputList;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a list of strings or objects with 'id' fields")
            }

            fn visit_seq<S>(self, mut seq: S) -> Result<Self::Value, S::Error>
            where
                S: SeqAccess<'de>,
            {
                let mut result = Vec::new();
                while let Some(item) = seq.next_element::<serde_yaml::Value>()? {
                    match item {
                        serde_yaml::Value::String(s) => result.push(s),
                        serde_yaml::Value::Mapping(m) => {
                            if let Some(serde_yaml::Value::String(id)) =
                                m.get(serde_yaml::Value::String("id".to_string()))
                            {
                                result.push(id.clone());
                            }
                        }
                        _ => {}
                    }
                }
                Ok(StepOutputList(result))
            }
        }

        deserializer.deserialize_seq(StepOutVisitor)
    }
}

/// A step input can be a simple source string or a structured entry.
#[derive(Debug, Clone, Serialize)]
pub enum StepInput {
    Source(String),
    Structured(StepInputEntry),
}

impl<'de> Deserialize<'de> for StepInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        match &value {
            serde_yaml::Value::String(s) => Ok(StepInput::Source(s.clone())),
            serde_yaml::Value::Mapping(_) => {
                let entry: StepInputEntry =
                    serde_yaml::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(StepInput::Structured(entry))
            }
            serde_yaml::Value::Sequence(seq) => {
                // Shorthand: `input_name: [source1, source2]`
                // is equivalent to `input_name: { source: [source1, source2] }`
                let mut sources = Vec::new();
                for item in seq {
                    if let serde_yaml::Value::String(s) = item {
                        sources.push(s.clone());
                    } else {
                        return Err(serde::de::Error::custom(format!(
                            "expected string in source array, got {:?}",
                            item
                        )));
                    }
                }
                Ok(StepInput::Structured(StepInputEntry {
                    id: None,
                    source: Some(SourceField::Multiple(sources)),
                    value_from: None,
                    default: None,
                    link_merge: None,
                }))
            }
            _ => Err(serde::de::Error::custom(format!(
                "expected string, mapping, or array for step input, got {:?}",
                value
            ))),
        }
    }
}

impl HasId for StepInput {
    fn take_id(&mut self) -> Option<String> {
        match self {
            StepInput::Structured(entry) => entry.id.take(),
            StepInput::Source(_) => None,
        }
    }
}

/// The `source` field on a step input: can be a single string or array of strings.
#[derive(Debug, Clone, Serialize)]
pub enum SourceField {
    Single(String),
    Multiple(Vec<String>),
}

impl<'de> Deserialize<'de> for SourceField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        match &value {
            serde_yaml::Value::String(s) => Ok(SourceField::Single(s.clone())),
            serde_yaml::Value::Sequence(seq) => {
                let mut sources = Vec::new();
                for item in seq {
                    if let serde_yaml::Value::String(s) = item {
                        sources.push(s.clone());
                    } else {
                        return Err(serde::de::Error::custom(format!(
                            "expected string in source array, got {:?}",
                            item
                        )));
                    }
                }
                Ok(SourceField::Multiple(sources))
            }
            _ => Err(serde::de::Error::custom(format!(
                "expected string or array for source, got {:?}",
                value
            ))),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepInputEntry {
    #[serde(default)]
    pub id: Option<String>,

    #[serde(default)]
    pub source: Option<SourceField>,

    #[serde(default)]
    pub value_from: Option<String>,

    #[serde(default)]
    pub default: Option<serde_yaml::Value>,

    #[serde(default)]
    pub link_merge: Option<String>,
}

/// Scatter can target a single input or multiple inputs.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ScatterField {
    Single(String),
    Multiple(Vec<String>),
}

// ---------------------------------------------------------------------------
// Runtime / resolved value types
// ---------------------------------------------------------------------------

/// A fully resolved input/output value at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResolvedValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    File(FileValue),
    Directory(FileValue),
    Array(Vec<ResolvedValue>),
    Null,
}

/// Represents a CWL File or Directory value with computed fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct FileValue {
    pub path: String,
    pub basename: String,
    pub nameroot: String,
    pub nameext: String,
    pub size: u64,
    pub checksum: Option<String>,
    pub secondary_files: Vec<FileValue>,
    /// File contents loaded when `loadContents: true` (limited to 64 KiB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contents: Option<String>,
    /// Format URI (from output format field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

// Allow constructing FileValue in tests without specifying format

impl FileValue {
    /// Build a `FileValue` from a filesystem path. The file does not need to
    /// exist (size will be 0 if it cannot be read).
    pub fn from_path(p: &str) -> Self {
        use sha1::{Sha1, Digest};

        let path = Path::new(p);
        let basename = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let nameext = path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let nameroot = if nameext.is_empty() {
            basename.clone()
        } else {
            basename
                .strip_suffix(&nameext)
                .unwrap_or(&basename)
                .to_string()
        };
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let checksum = if path.exists() && path.is_file() {
            if let Ok(mut file) = std::fs::File::open(path) {
                let mut hasher = Sha1::new();
                let mut buf = [0u8; 65536];
                loop {
                    let n = std::io::Read::read(&mut file, &mut buf).unwrap_or(0);
                    if n == 0 { break; }
                    hasher.update(&buf[..n]);
                }
                Some(format!("sha1${:x}", hasher.finalize()))
            } else {
                None
            }
        } else {
            None
        };
        FileValue {
            path: p.to_string(),
            basename,
            nameroot,
            nameext,
            size,
            checksum,
            secondary_files: Vec::new(),
            contents: None,
            format: None,
        }
    }
}

/// Runtime resource context passed to expressions and the command builder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeContext {
    pub cores: u32,
    pub ram: u64,
    pub outdir: String,
    pub tmpdir: String,
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- CwlType::base_type() -----------------------------------------------

    #[test]
    fn base_type_plain() {
        let t = CwlType::Single("File".to_string());
        assert_eq!(t.base_type(), "File");
    }

    #[test]
    fn base_type_optional() {
        let t = CwlType::Single("File?".to_string());
        assert_eq!(t.base_type(), "File");
    }

    #[test]
    fn base_type_array_suffix() {
        let t = CwlType::Single("string[]".to_string());
        assert_eq!(t.base_type(), "string");
    }

    #[test]
    fn base_type_union_array() {
        let t = CwlType::Union(vec![
            CwlType::Single("null".to_string()),
            CwlType::Single("File".to_string()),
        ]);
        assert_eq!(t.base_type(), "File");
    }

    // -- CwlType::is_optional() ---------------------------------------------

    #[test]
    fn is_optional_plain() {
        let t = CwlType::Single("File".to_string());
        assert!(!t.is_optional());
    }

    #[test]
    fn is_optional_question_mark() {
        let t = CwlType::Single("File?".to_string());
        assert!(t.is_optional());
    }

    #[test]
    fn is_optional_union_with_null() {
        let t = CwlType::Union(vec![
            CwlType::Single("null".to_string()),
            CwlType::Single("File".to_string()),
        ]);
        assert!(t.is_optional());
    }

    // -- CwlType::is_array() ------------------------------------------------

    #[test]
    fn is_array_plain() {
        let t = CwlType::Single("File".to_string());
        assert!(!t.is_array());
    }

    #[test]
    fn is_array_bracket_suffix() {
        let t = CwlType::Single("File[]".to_string());
        assert!(t.is_array());
    }

    // -- FileValue::from_path() ---------------------------------------------

    #[test]
    fn file_value_from_path() {
        let fv = FileValue::from_path("/data/reads.fastq.gz");
        assert_eq!(fv.basename, "reads.fastq.gz");
        assert_eq!(fv.nameext, ".gz");
        assert_eq!(fv.nameroot, "reads.fastq");
        assert_eq!(fv.path, "/data/reads.fastq.gz");
        // File doesn't exist, so size should be 0
        assert_eq!(fv.size, 0);
    }

    #[test]
    fn file_value_no_extension() {
        let fv = FileValue::from_path("/usr/bin/bash");
        assert_eq!(fv.basename, "bash");
        assert_eq!(fv.nameext, "");
        assert_eq!(fv.nameroot, "bash");
    }

    // -- Serde round-trip for CommandLineTool --------------------------------

    #[test]
    fn deserialize_command_line_tool() {
        let yaml = r#"
class: CommandLineTool
cwlVersion: v1.2
baseCommand: echo
inputs:
  message:
    type: string
    inputBinding:
      position: 1
outputs:
  out:
    type: stdout
stdout: output.txt
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                assert!(matches!(tool.base_command, BaseCommand::Single(ref s) if s == "echo"));
                assert!(tool.inputs.contains_key("message"));
                assert_eq!(tool.stdout, Some("output.txt".to_string()));
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    // -- Serde round-trip for ExpressionTool ----------------------------------

    #[test]
    fn deserialize_expression_tool() {
        let yaml = r#"
class: ExpressionTool
cwlVersion: v1.2
requirements:
  - class: InlineJavascriptRequirement
inputs:
  file1:
    type: File
    inputBinding:
      loadContents: true
outputs:
  output:
    type: int
expression: "${return {\"output\": parseInt(inputs.file1.contents)};}"
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::ExpressionTool(et) => {
                assert_eq!(et.cwl_version, Some("v1.2".to_string()));
                assert!(et.inputs.contains_key("file1"));
                assert!(et.outputs.contains_key("output"));
                assert!(et.expression.is_some());
                assert!(et.expression.as_ref().unwrap().contains("parseInt"));
                assert_eq!(et.requirements.len(), 1);
            }
            _ => panic!("Expected ExpressionTool"),
        }
    }

    #[test]
    fn deserialize_expression_tool_minimal() {
        let yaml = r#"
class: ExpressionTool
inputs: {}
outputs: {}
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::ExpressionTool(et) => {
                assert!(et.inputs.is_empty());
                assert!(et.outputs.is_empty());
                assert!(et.expression.is_none());
            }
            _ => panic!("Expected ExpressionTool"),
        }
    }

    // -- Serde round-trip for Workflow ---------------------------------------

    #[test]
    fn deserialize_workflow() {
        let yaml = r#"
class: Workflow
cwlVersion: v1.2
inputs:
  infile:
    type: File
outputs:
  outfile:
    type: File
    outputSource: step1/result
steps:
  step1:
    run: tool.cwl
    in:
      input1: infile
    out: [result]
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::Workflow(wf) => {
                assert!(wf.inputs.contains_key("infile"));
                assert!(wf.outputs.contains_key("outfile"));
                assert!(wf.steps.contains_key("step1"));
                let step = &wf.steps["step1"];
                assert_eq!(step.run, "tool.cwl");
                assert_eq!(*step.out, vec!["result"]);
            }
            _ => panic!("Expected Workflow"),
        }
    }

    // -- List-form inputs/outputs (Bug 1) -----------------------------------

    #[test]
    fn deserialize_list_form_inputs_outputs() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs:
  - id: reference
    type: File
    inputBinding: { position: 1 }
  - id: message
    type: string
outputs:
  - id: sam
    type: File
    outputBinding: { glob: output.sam }
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                assert!(tool.inputs.contains_key("reference"));
                assert!(tool.inputs.contains_key("message"));
                assert_eq!(tool.inputs["reference"].cwl_type.base_type(), "File");
                assert_eq!(tool.inputs["message"].cwl_type.base_type(), "string");
                assert!(tool.outputs.contains_key("sam"));
                assert_eq!(tool.outputs["sam"].cwl_type.base_type(), "File");
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    // -- Structured array type {type: array, items: File} (Bug 1) -----------

    #[test]
    fn deserialize_structured_array_type() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs:
  - id: reads
    type:
      type: array
      items: File
    inputBinding: { position: 1 }
outputs: {}
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                let reads = tool.inputs.get("reads").expect("missing input 'reads'");
                assert_eq!(reads.cwl_type.base_type(), "File");
                assert!(reads.cwl_type.is_array());
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    // -- Inline structured array type {type: array, items: int} -------------

    #[test]
    fn deserialize_inline_structured_array_type() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs:
  - id: counts
    type: { type: array, items: int }
outputs: {}
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                let counts = tool.inputs.get("counts").expect("missing input 'counts'");
                assert_eq!(counts.cwl_type.base_type(), "int");
                assert!(counts.cwl_type.is_array());
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    // -- Union type with structured array: ["null", {type: array, items: string}]

    #[test]
    fn deserialize_union_with_structured_array() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs:
  - id: tags
    type:
      - "null"
      - type: array
        items: string
outputs: {}
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                let tags = tool.inputs.get("tags").expect("missing input 'tags'");
                assert!(tags.cwl_type.is_optional());
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    // -- Map-form requirements (Bug 2) --------------------------------------

    #[test]
    fn deserialize_map_form_requirements() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs: {}
outputs: {}
requirements:
  ResourceRequirement:
    coresMin: 4
  ShellCommandRequirement: {}
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                assert_eq!(tool.requirements.len(), 2);
                // Check that the class field was injected
                let classes: Vec<String> = tool.requirements.iter().filter_map(|r| {
                    r.get("class").and_then(|v| v.as_str()).map(|s| s.to_string())
                }).collect();
                assert!(classes.contains(&"ResourceRequirement".to_string()));
                assert!(classes.contains(&"ShellCommandRequirement".to_string()));
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    // -- Map-form hints (Bug 2) ---------------------------------------------

    #[test]
    fn deserialize_map_form_hints() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs: {}
outputs: {}
hints:
  DockerRequirement:
    dockerPull: ubuntu:22.04
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                assert_eq!(tool.hints.len(), 1);
                let class = tool.hints[0].get("class").and_then(|v| v.as_str());
                assert_eq!(class, Some("DockerRequirement"));
                let pull = tool.hints[0].get("dockerPull").and_then(|v| v.as_str());
                assert_eq!(pull, Some("ubuntu:22.04"));
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    // -- Union output type: ["null", File] ----------------------------------

    #[test]
    fn deserialize_union_output_type() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs: {}
outputs:
  - id: sam
    type: ["null", File]
    outputBinding: { glob: output.sam }
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                let sam = tool.outputs.get("sam").expect("missing output 'sam'");
                assert!(sam.cwl_type.is_optional());
                assert_eq!(sam.cwl_type.base_type(), "File");
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    // -- bwa-mem-tool.cwl style (full conformance test format) ---------------

    #[test]
    fn deserialize_bwa_mem_tool_style() {
        let yaml = r#"
class: CommandLineTool
cwlVersion: v1.2
hints:
  - class: ResourceRequirement
    coresMin: 2
  - class: DockerRequirement
    dockerPull: docker.io/python:3-slim
inputs:
  - id: reference
    type: File
    inputBinding: { position: 2 }
  - id: reads
    type:
      type: array
      items: File
    inputBinding: { position: 3 }
  - id: minimum_seed_length
    type: int
    inputBinding: { position: 1, prefix: -m }
  - id: min_std_max_min
    type: { type: array, items: int }
    inputBinding:
      position: 1
      prefix: -I
      itemSeparator: ","
outputs:
  - id: sam
    type: ["null", File]
    outputBinding: { glob: output.sam }
  - id: args
    type:
      type: array
      items: string
baseCommand: python
arguments:
  - bwa
  - mem
stdout: output.sam
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                assert_eq!(tool.inputs.len(), 4);
                assert_eq!(tool.outputs.len(), 2);
                assert!(tool.inputs.contains_key("reference"));
                assert!(tool.inputs.contains_key("reads"));
                assert!(tool.inputs.contains_key("minimum_seed_length"));
                assert!(tool.inputs.contains_key("min_std_max_min"));
                // Check reads is array type
                assert!(tool.inputs["reads"].cwl_type.is_array());
                assert_eq!(tool.inputs["reads"].cwl_type.base_type(), "File");
                // Check min_std_max_min has itemSeparator
                let binding = tool.inputs["min_std_max_min"].input_binding.as_ref().unwrap();
                assert_eq!(binding.item_separator, Some(",".to_string()));
                // Check output union type
                assert!(tool.outputs["sam"].cwl_type.is_optional());
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    // -- FileValue checksum ---------------------------------------------------

    #[test]
    fn file_value_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello").unwrap();
        let fv = FileValue::from_path(path.to_str().unwrap());
        assert!(fv.checksum.is_some());
        assert!(fv.checksum.as_ref().unwrap().starts_with("sha1$"));
    }

    #[test]
    fn file_value_checksum_missing_file() {
        let fv = FileValue::from_path("/nonexistent/file.txt");
        assert!(fv.checksum.is_none());
    }

    // -- Shorthand type syntax in map-form inputs/outputs ---------------------

    #[test]
    fn parse_shorthand_input_types() {
        let yaml = r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: echo
inputs:
  file1: File
  count: int
  flag: boolean
  opt: File?
outputs:
  result: stdout
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                assert_eq!(tool.inputs.len(), 4);
                assert_eq!(tool.inputs["file1"].cwl_type.base_type(), "File");
                assert_eq!(tool.inputs["count"].cwl_type.base_type(), "int");
                assert_eq!(tool.inputs["flag"].cwl_type.base_type(), "boolean");
                assert!(tool.inputs["opt"].cwl_type.is_optional());
                assert_eq!(tool.outputs.len(), 1);
                assert_eq!(tool.outputs["result"].cwl_type.base_type(), "stdout");
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_shorthand_workflow_types() {
        let yaml = r#"
cwlVersion: v1.2
class: Workflow
inputs:
  message: string
  ref_file: File
steps: {}
outputs: {}
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::Workflow(wf) => {
                assert_eq!(wf.inputs["message"].cwl_type.base_type(), "string");
                assert_eq!(wf.inputs["ref_file"].cwl_type.base_type(), "File");
            }
            _ => panic!("Expected Workflow"),
        }
    }

    #[test]
    fn parse_shorthand_array_type() {
        let yaml = r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: echo
inputs:
  reads: File[]
outputs: {}
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                assert!(tool.inputs["reads"].cwl_type.is_array());
                assert_eq!(tool.inputs["reads"].cwl_type.base_type(), "File");
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    // -- Step input parsing: sequence shorthand for source ---------------------

    #[test]
    fn step_input_sequence_source() {
        let yaml = r#"
class: Workflow
cwlVersion: v1.2
inputs:
  file1: File
  file2: File
outputs:
  count_output:
    type: int
    outputSource: step1/output
steps:
  step1:
    run: wc.cwl
    in:
      file1: [file1, file2]
    out: [output]
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::Workflow(wf) => {
                let step = &wf.steps["step1"];
                match &step.inputs["file1"] {
                    StepInput::Structured(entry) => match &entry.source {
                        Some(SourceField::Multiple(sources)) => {
                            assert_eq!(sources.len(), 2);
                            assert_eq!(sources[0], "file1");
                            assert_eq!(sources[1], "file2");
                        }
                        other => panic!("expected Multiple source, got {:?}", other),
                    },
                    other => panic!("expected Structured step input, got {:?}", other),
                }
            }
            _ => panic!("Expected Workflow"),
        }
    }

    // -- Step input parsing: structured source array ---------------------------

    #[test]
    fn step_input_structured_source_array() {
        let yaml = r#"
class: Workflow
cwlVersion: v1.2
inputs:
  file1: File
  file2: File
outputs: []
steps:
  step1:
    run: wc.cwl
    in:
      file1:
        source: [file1, file2]
        linkMerge: merge_nested
    out: [output]
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::Workflow(wf) => {
                let step = &wf.steps["step1"];
                match &step.inputs["file1"] {
                    StepInput::Structured(entry) => {
                        match &entry.source {
                            Some(SourceField::Multiple(sources)) => {
                                assert_eq!(sources.len(), 2);
                            }
                            other => panic!("expected Multiple source, got {:?}", other),
                        }
                        assert_eq!(entry.link_merge.as_deref(), Some("merge_nested"));
                    }
                    other => panic!("expected Structured step input, got {:?}", other),
                }
            }
            _ => panic!("Expected Workflow"),
        }
    }

    // -- Inline tool definition parsing ----------------------------------------

    #[test]
    fn step_run_inline_tool() {
        let yaml = r#"
class: Workflow
cwlVersion: v1.2
inputs:
  file1: File
outputs:
  out:
    type: File
    outputSource: step1/output
steps:
  step1:
    run:
      class: CommandLineTool
      baseCommand: echo
      inputs:
        file1:
          type: File
          inputBinding: {}
      outputs:
        output:
          type: stdout
      stdout: output.txt
    in:
      file1: file1
    out: [output]
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::Workflow(wf) => {
                let step = &wf.steps["step1"];
                match &step.run {
                    StepRun::Inline(doc) => match doc.as_ref() {
                        CwlDocument::CommandLineTool(tool) => {
                            assert!(matches!(tool.base_command, BaseCommand::Single(ref s) if s == "echo"));
                        }
                        _ => panic!("expected CommandLineTool"),
                    },
                    StepRun::Path(_) => panic!("expected Inline, got Path"),
                }
            }
            _ => panic!("Expected Workflow"),
        }
    }
}
