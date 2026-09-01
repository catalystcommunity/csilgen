use ciborium::value::Value;
use csilgen_core::{
    ControlOperator, CsilSpec, DependsCompareOp, DependsCondition, FieldMetadata, FieldVisibility,
    GroupEntry, GroupExpression, GroupKey, LiteralValue, Occurrence, RuleType, ServiceDirection,
    SizeConstraint, TypeExpression, ValidationConstraint,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use std::fmt;
use std::io::Cursor;
use thiserror::Error;

pub const FORMAT_NAME: &str = "csil-schema";
pub const FORMAT_VERSION: &str = "v1alpha1";
pub const MEDIA_TYPE: &str = "application/csil-schema+cbor";

/// A CBOR byte string. This wrapper prevents Serde from encoding bytes as an array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteString(pub Vec<u8>);

impl Serialize for ByteString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&self.0)
    }
}

struct ByteStringVisitor;

impl<'de> de::Visitor<'de> for ByteStringVisitor {
    type Value = ByteString;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a CBOR byte string")
    }

    fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(ByteString(value.to_vec()))
    }

    fn visit_byte_buf<E>(self, value: Vec<u8>) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(ByteString(value))
    }
}

impl<'de> Deserialize<'de> for ByteString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_byte_buf(ByteStringVisitor)
    }
}

/// `v1alpha1` descriptor. The digest is SHA-256 over the deterministic encoding
/// of [`SchemaBody`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaDescriptor {
    pub format: String,
    pub version: String,
    pub digest: ByteString,
    pub body: SchemaBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaBody {
    pub root: String,
    pub rules: Vec<SchemaRule>,
    pub services: Vec<SchemaService>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaRule {
    pub name: String,
    pub definition: SchemaRuleDefinition,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SchemaRuleDefinition {
    Type(SchemaType),
    Group(SchemaGroup),
    GroupChoice(Vec<SchemaGroup>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SchemaType {
    Builtin(String),
    Reference(String),
    Array {
        element: Box<SchemaType>,
        occurrence: Option<SchemaOccurrence>,
    },
    Tuple(SchemaGroup),
    Map {
        key: Box<SchemaType>,
        value: Box<SchemaType>,
        occurrence: Option<SchemaOccurrence>,
    },
    Group(SchemaGroup),
    Choice(Vec<SchemaType>),
    Range {
        start: Option<i64>,
        end: Option<i64>,
        inclusive: bool,
    },
    Socket(String),
    Plug(String),
    Literal(SchemaLiteral),
    Constrained {
        base: Box<SchemaType>,
        constraints: Vec<SchemaControl>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaGroup {
    pub entries: Vec<SchemaGroupEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaGroupEntry {
    pub key: Option<SchemaGroupKey>,
    pub value: SchemaType,
    pub occurrence: Option<SchemaOccurrence>,
    pub metadata: Vec<SchemaFieldMetadata>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SchemaGroupKey {
    Bare(String),
    Type(SchemaType),
    Literal(SchemaLiteral),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SchemaOccurrence {
    Optional,
    ZeroOrMore,
    OneOrMore,
    Exact(u64),
    Range { min: Option<u64>, max: Option<u64> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SchemaLiteral {
    Integer(i64),
    Float(f64),
    Text(String),
    Bytes(ByteString),
    Bool(bool),
    Null,
    Array(Vec<SchemaLiteral>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SchemaControl {
    Size(SchemaSize),
    Regex(String),
    Default(SchemaLiteral),
    GreaterEqual(SchemaLiteral),
    LessEqual(SchemaLiteral),
    GreaterThan(SchemaLiteral),
    LessThan(SchemaLiteral),
    Equal(SchemaLiteral),
    NotEqual(SchemaLiteral),
    Bits(String),
    And(Box<SchemaType>),
    Within(Box<SchemaType>),
    Json,
    Cbor,
    Cborseq,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SchemaSize {
    Exact(u64),
    Range { min: u64, max: u64 },
    Min(u64),
    Max(u64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SchemaFieldMetadata {
    Visibility(SchemaVisibility),
    DependsOn {
        field: String,
        value: Option<SchemaLiteral>,
    },
    DependsOnExpr(SchemaDependsCondition),
    Constraint(SchemaValidationConstraint),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SchemaVisibility {
    SendOnly,
    ReceiveOnly,
    Bidirectional,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SchemaDependsCondition {
    Compare {
        field: String,
        op: Option<SchemaDependsCompareOp>,
        value: Option<SchemaLiteral>,
    },
    All(Vec<SchemaDependsCondition>),
    Any(Vec<SchemaDependsCondition>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemaDependsCompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SchemaValidationConstraint {
    MinLength(u64),
    MaxLength(u64),
    MinItems(u64),
    MaxItems(u64),
    MinValue(SchemaLiteral),
    MaxValue(SchemaLiteral),
    Custom { name: String, value: SchemaLiteral },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaService {
    pub name: String,
    pub wire_id: Option<u64>,
    pub operations: Vec<SchemaOperation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaOperation {
    pub name: String,
    pub wire_id: Option<u64>,
    pub input: SchemaType,
    pub output: SchemaType,
    pub direction: SchemaServiceDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemaServiceDirection {
    Unidirectional,
    Bidirectional,
    Reverse,
}

#[derive(Debug, Error)]
pub enum DescriptorError {
    #[error("cannot encode the schema descriptor: {0}")]
    Encode(String),
    #[error("cannot decode the schema descriptor: {0}")]
    Decode(String),
    #[error("unsupported schema descriptor format '{format}' version {version}")]
    UnsupportedVersion { format: String, version: String },
    #[error("schema descriptor digest must contain 32 bytes, found {0}")]
    InvalidDigestLength(usize),
    #[error("schema descriptor digest does not match its body")]
    DigestMismatch,
}

impl SchemaDescriptor {
    pub fn from_spec(root: impl Into<String>, spec: &CsilSpec) -> Result<Self, DescriptorError> {
        let mut rules = Vec::new();
        let mut services = Vec::new();

        for rule in &spec.rules {
            match &rule.rule_type {
                RuleType::TypeDef(value) => rules.push(SchemaRule {
                    name: rule.name.clone(),
                    definition: SchemaRuleDefinition::Type(value.into()),
                }),
                RuleType::GroupDef(value) => rules.push(SchemaRule {
                    name: rule.name.clone(),
                    definition: SchemaRuleDefinition::Group(value.into()),
                }),
                RuleType::TypeChoice(values) => rules.push(SchemaRule {
                    name: rule.name.clone(),
                    definition: SchemaRuleDefinition::Type(SchemaType::Choice(
                        values.iter().map(Into::into).collect(),
                    )),
                }),
                RuleType::GroupChoice(values) => rules.push(SchemaRule {
                    name: rule.name.clone(),
                    definition: SchemaRuleDefinition::GroupChoice(
                        values.iter().map(Into::into).collect(),
                    ),
                }),
                RuleType::ServiceDef(service) => services.push(SchemaService {
                    name: rule.name.clone(),
                    wire_id: service.wire_id(),
                    operations: service
                        .operations
                        .iter()
                        .map(|operation| SchemaOperation {
                            name: operation.name.clone(),
                            wire_id: operation.wire_id(),
                            input: (&operation.input_type).into(),
                            output: (&operation.output_type).into(),
                            direction: (&operation.direction).into(),
                        })
                        .collect(),
                }),
            }
        }

        let body = SchemaBody {
            root: root.into(),
            rules,
            services,
        };
        let digest = body_digest(&body)?;
        Ok(Self {
            format: FORMAT_NAME.to_string(),
            version: FORMAT_VERSION.to_string(),
            digest: ByteString(digest.to_vec()),
            body,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, DescriptorError> {
        canonical_encode(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DescriptorError> {
        let mut cursor = Cursor::new(bytes);
        let descriptor: Self = ciborium::de::from_reader_with_recursion_limit(&mut cursor, 64)
            .map_err(|error| DescriptorError::Decode(error.to_string()))?;
        if cursor.position() as usize != bytes.len() {
            return Err(DescriptorError::Decode(
                "trailing bytes after the descriptor".to_string(),
            ));
        }
        descriptor.verify()?;
        Ok(descriptor)
    }

    pub fn verify(&self) -> Result<(), DescriptorError> {
        if self.format != FORMAT_NAME || self.version != FORMAT_VERSION {
            return Err(DescriptorError::UnsupportedVersion {
                format: self.format.clone(),
                version: self.version.clone(),
            });
        }
        if self.digest.0.len() != 32 {
            return Err(DescriptorError::InvalidDigestLength(self.digest.0.len()));
        }
        if self.digest.0.as_slice() != body_digest(&self.body)?.as_slice() {
            return Err(DescriptorError::DigestMismatch);
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<[u8; 32], DescriptorError> {
        if self.digest.0.len() != 32 {
            return Err(DescriptorError::InvalidDigestLength(self.digest.0.len()));
        }
        let mut digest = [0_u8; 32];
        digest.copy_from_slice(&self.digest.0);
        Ok(digest)
    }
}

pub fn body_digest(body: &SchemaBody) -> Result<[u8; 32], DescriptorError> {
    let bytes = canonical_encode(body)?;
    Ok(Sha256::digest(bytes).into())
}

/// Encode with recursively sorted map keys. Keys are ordered by their encoded
/// bytes. This rule is part of descriptor version `v1alpha1`.
pub fn canonical_encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, DescriptorError> {
    let mut value =
        Value::serialized(value).map_err(|error| DescriptorError::Encode(error.to_string()))?;
    canonicalize(&mut value)?;
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&value, &mut bytes)
        .map_err(|error| DescriptorError::Encode(error.to_string()))?;
    Ok(bytes)
}

fn canonicalize(value: &mut Value) -> Result<(), DescriptorError> {
    match value {
        Value::Array(values) => {
            for value in values {
                canonicalize(value)?;
            }
        }
        Value::Map(entries) => {
            for (key, value) in entries.iter_mut() {
                canonicalize(key)?;
                canonicalize(value)?;
            }
            let mut encoded = Vec::with_capacity(entries.len());
            for (key, value) in std::mem::take(entries) {
                let mut bytes = Vec::new();
                ciborium::ser::into_writer(&key, &mut bytes)
                    .map_err(|error| DescriptorError::Encode(error.to_string()))?;
                encoded.push((bytes, key, value));
            }
            encoded.sort_by(|left, right| left.0.cmp(&right.0));
            *entries = encoded
                .into_iter()
                .map(|(_, key, value)| (key, value))
                .collect();
        }
        Value::Tag(_, value) => canonicalize(value)?,
        _ => {}
    }
    Ok(())
}

impl From<&TypeExpression> for SchemaType {
    fn from(value: &TypeExpression) -> Self {
        match value {
            TypeExpression::Builtin(name) => Self::Builtin(name.clone()),
            TypeExpression::Reference(name) => Self::Reference(name.clone()),
            TypeExpression::Array {
                element_type,
                occurrence,
            } => Self::Array {
                element: Box::new(element_type.as_ref().into()),
                occurrence: occurrence.as_ref().map(Into::into),
            },
            TypeExpression::Tuple(group) => Self::Tuple(group.into()),
            TypeExpression::Map {
                key,
                value,
                occurrence,
            } => Self::Map {
                key: Box::new(key.as_ref().into()),
                value: Box::new(value.as_ref().into()),
                occurrence: occurrence.as_ref().map(Into::into),
            },
            TypeExpression::Group(group) => Self::Group(group.into()),
            TypeExpression::Choice(arms) => Self::Choice(arms.iter().map(Into::into).collect()),
            TypeExpression::Range {
                start,
                end,
                inclusive,
            } => Self::Range {
                start: *start,
                end: *end,
                inclusive: *inclusive,
            },
            TypeExpression::Socket(name) => Self::Socket(name.clone()),
            TypeExpression::Plug(name) => Self::Plug(name.clone()),
            TypeExpression::Literal(literal) => Self::Literal(literal.into()),
            TypeExpression::Constrained {
                base_type,
                constraints,
            } => Self::Constrained {
                base: Box::new(base_type.as_ref().into()),
                constraints: constraints.iter().map(Into::into).collect(),
            },
        }
    }
}

impl From<&GroupExpression> for SchemaGroup {
    fn from(value: &GroupExpression) -> Self {
        Self {
            entries: value.entries.iter().map(Into::into).collect(),
        }
    }
}

impl From<&GroupEntry> for SchemaGroupEntry {
    fn from(value: &GroupEntry) -> Self {
        Self {
            key: value.key.as_ref().map(Into::into),
            value: (&value.value_type).into(),
            occurrence: value.occurrence.as_ref().map(Into::into),
            metadata: value.metadata.iter().filter_map(convert_metadata).collect(),
        }
    }
}

impl From<&GroupKey> for SchemaGroupKey {
    fn from(value: &GroupKey) -> Self {
        match value {
            GroupKey::Bare(name) => Self::Bare(name.clone()),
            GroupKey::Type(value) => Self::Type(value.into()),
            GroupKey::Literal(value) => Self::Literal(value.into()),
        }
    }
}

impl From<&Occurrence> for SchemaOccurrence {
    fn from(value: &Occurrence) -> Self {
        match value {
            Occurrence::Optional => Self::Optional,
            Occurrence::ZeroOrMore => Self::ZeroOrMore,
            Occurrence::OneOrMore => Self::OneOrMore,
            Occurrence::Exact(value) => Self::Exact(*value),
            Occurrence::Range { min, max } => Self::Range {
                min: *min,
                max: *max,
            },
        }
    }
}

impl From<&LiteralValue> for SchemaLiteral {
    fn from(value: &LiteralValue) -> Self {
        match value {
            LiteralValue::Integer(value) => Self::Integer(*value),
            LiteralValue::Float(value) => Self::Float(*value),
            LiteralValue::Text(value) => Self::Text(value.clone()),
            LiteralValue::Bytes(value) => Self::Bytes(ByteString(value.clone())),
            LiteralValue::Bool(value) => Self::Bool(*value),
            LiteralValue::Null => Self::Null,
            LiteralValue::Array(values) => Self::Array(values.iter().map(Into::into).collect()),
        }
    }
}

impl From<&ControlOperator> for SchemaControl {
    fn from(value: &ControlOperator) -> Self {
        match value {
            ControlOperator::Size(value) => Self::Size(value.into()),
            ControlOperator::Regex(value) => Self::Regex(value.clone()),
            ControlOperator::Default(value) => Self::Default(value.into()),
            ControlOperator::GreaterEqual(value) => Self::GreaterEqual(value.into()),
            ControlOperator::LessEqual(value) => Self::LessEqual(value.into()),
            ControlOperator::GreaterThan(value) => Self::GreaterThan(value.into()),
            ControlOperator::LessThan(value) => Self::LessThan(value.into()),
            ControlOperator::Equal(value) => Self::Equal(value.into()),
            ControlOperator::NotEqual(value) => Self::NotEqual(value.into()),
            ControlOperator::Bits(value) => Self::Bits(value.clone()),
            ControlOperator::And(value) => Self::And(Box::new(value.as_ref().into())),
            ControlOperator::Within(value) => Self::Within(Box::new(value.as_ref().into())),
            ControlOperator::Json => Self::Json,
            ControlOperator::Cbor => Self::Cbor,
            ControlOperator::Cborseq => Self::Cborseq,
        }
    }
}

impl From<&SizeConstraint> for SchemaSize {
    fn from(value: &SizeConstraint) -> Self {
        match value {
            SizeConstraint::Exact(value) => Self::Exact(*value),
            SizeConstraint::Range { min, max } => Self::Range {
                min: *min,
                max: *max,
            },
            SizeConstraint::Min(value) => Self::Min(*value),
            SizeConstraint::Max(value) => Self::Max(*value),
        }
    }
}

fn convert_metadata(value: &FieldMetadata) -> Option<SchemaFieldMetadata> {
    match value {
        FieldMetadata::Visibility(value) => Some(SchemaFieldMetadata::Visibility(value.into())),
        FieldMetadata::DependsOn { field, value } => Some(SchemaFieldMetadata::DependsOn {
            field: field.clone(),
            value: value.as_ref().map(Into::into),
        }),
        FieldMetadata::DependsOnExpr(value) => {
            Some(SchemaFieldMetadata::DependsOnExpr(value.into()))
        }
        FieldMetadata::Constraint(value) => Some(SchemaFieldMetadata::Constraint(value.into())),
        FieldMetadata::Description(_) | FieldMetadata::Custom { .. } => None,
    }
}

impl From<&FieldVisibility> for SchemaVisibility {
    fn from(value: &FieldVisibility) -> Self {
        match value {
            FieldVisibility::SendOnly => Self::SendOnly,
            FieldVisibility::ReceiveOnly => Self::ReceiveOnly,
            FieldVisibility::Bidirectional => Self::Bidirectional,
        }
    }
}

impl From<&DependsCondition> for SchemaDependsCondition {
    fn from(value: &DependsCondition) -> Self {
        match value {
            DependsCondition::Compare { field, op, value } => Self::Compare {
                field: field.clone(),
                op: op.as_ref().map(Into::into),
                value: value.as_ref().map(Into::into),
            },
            DependsCondition::All(values) => Self::All(values.iter().map(Into::into).collect()),
            DependsCondition::Any(values) => Self::Any(values.iter().map(Into::into).collect()),
        }
    }
}

impl From<&DependsCompareOp> for SchemaDependsCompareOp {
    fn from(value: &DependsCompareOp) -> Self {
        match value {
            DependsCompareOp::Eq => Self::Eq,
            DependsCompareOp::Ne => Self::Ne,
            DependsCompareOp::Lt => Self::Lt,
            DependsCompareOp::Le => Self::Le,
            DependsCompareOp::Gt => Self::Gt,
            DependsCompareOp::Ge => Self::Ge,
        }
    }
}

impl From<&ValidationConstraint> for SchemaValidationConstraint {
    fn from(value: &ValidationConstraint) -> Self {
        match value {
            ValidationConstraint::MinLength(value) => Self::MinLength(*value),
            ValidationConstraint::MaxLength(value) => Self::MaxLength(*value),
            ValidationConstraint::MinItems(value) => Self::MinItems(*value),
            ValidationConstraint::MaxItems(value) => Self::MaxItems(*value),
            ValidationConstraint::MinValue(value) => Self::MinValue(value.into()),
            ValidationConstraint::MaxValue(value) => Self::MaxValue(value.into()),
            ValidationConstraint::Custom { name, value } => Self::Custom {
                name: name.clone(),
                value: value.into(),
            },
        }
    }
}

impl From<&ServiceDirection> for SchemaServiceDirection {
    fn from(value: &ServiceDirection) -> Self {
        match value {
            ServiceDirection::Unidirectional => Self::Unidirectional,
            ServiceDirection::Bidirectional => Self::Bidirectional,
            ServiceDirection::Reverse => Self::Reverse,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_core::{ImportResolver, parse_csil, parse_csil_file, validate_spec_optimized};
    use std::path::PathBuf;

    #[test]
    fn descriptor_is_stable_and_verifies() {
        let spec = parse_csil(
            r#"
            Bytes = bstr .size (1..10)
            Reply = "ok" / "error"
            @wire-id(7)
            service Example {
                @wire-id(2)
                call: { data: Bytes, ? note: text @send-only } -> Reply
            }
            "#,
        )
        .unwrap();
        let descriptor = SchemaDescriptor::from_spec("example", &spec).unwrap();
        assert_eq!(descriptor.version, "v1alpha1");
        let first = descriptor.encode().unwrap();
        let second = descriptor.encode().unwrap();
        assert_eq!(first, second);
        assert_eq!(SchemaDescriptor::decode(&first).unwrap(), descriptor);
        assert_eq!(
            descriptor.digest().unwrap(),
            body_digest(&descriptor.body).unwrap()
        );
    }

    #[test]
    fn descriptor_excludes_documentation_and_generator_hints() {
        let spec = parse_csil(
            r#"
            ;;; private source documentation
            Item = {
                ;;; field documentation
                value: bstr @description("display only") @language-name("Value")
            }
            "#,
        )
        .unwrap();
        let bytes = SchemaDescriptor::from_spec("item", &spec)
            .unwrap()
            .encode()
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("private source documentation"));
        assert!(!text.contains("field documentation"));
        assert!(!text.contains("display only"));
        assert!(!text.contains("language-name"));
    }

    #[test]
    fn descriptor_rejects_digest_mismatch_and_unknown_version() {
        let spec = parse_csil("Value = any").unwrap();
        let mut descriptor = SchemaDescriptor::from_spec("value", &spec).unwrap();
        descriptor.digest.0[0] ^= 0xff;
        assert!(matches!(
            descriptor.verify(),
            Err(DescriptorError::DigestMismatch)
        ));

        let mut descriptor = SchemaDescriptor::from_spec("value", &spec).unwrap();
        descriptor.version = "v1alpha2".to_string();
        assert!(matches!(
            descriptor.verify(),
            Err(DescriptorError::UnsupportedVersion { version, .. })
                if version == "v1alpha2"
        ));

        assert!(matches!(
            SchemaDescriptor::decode(&[0xff]),
            Err(DescriptorError::Decode(_))
        ));
    }

    #[test]
    fn comprehensive_resolved_fixture_encodes() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/schema/descriptor-all-types.csil");
        let mut spec = parse_csil_file(&path).unwrap();
        ImportResolver::new()
            .resolve_imports(&mut spec, &path)
            .unwrap();
        validate_spec_optimized(&spec).unwrap();

        let descriptor = SchemaDescriptor::from_spec("descriptor-all-types", &spec).unwrap();
        let bytes = descriptor.encode().unwrap();
        let decoded = SchemaDescriptor::decode(&bytes).unwrap();

        assert_eq!(decoded.body.rules.len(), 32);
        assert_eq!(decoded.body.services.len(), 1);
        assert_eq!(decoded.body.services[0].operations.len(), 3);
    }
}
