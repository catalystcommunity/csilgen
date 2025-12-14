//! Abstract Syntax Tree definitions for CSIL

use crate::lexer::Position;
use serde::{Deserialize, Serialize};

/// Root AST node representing a complete CSIL interface definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CsilSpec {
    pub imports: Vec<ImportStatement>,
    pub options: Option<FileOptions>,
    pub rules: Vec<Rule>,
}

/// Import statements for including other CSIL files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ImportStatement {
    /// Simple include with optional alias: `include "path/file.csil" as alias`
    Include {
        path: String,
        alias: Option<String>,
        position: Position,
    },
    /// Selective import: `from "path/file.csil" include Type1, Type2`
    SelectiveImport {
        path: String,
        items: Vec<String>,
        position: Position,
    },
}

/// File-level options block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileOptions {
    pub entries: Vec<OptionEntry>,
    pub position: Position,
}

/// Individual entry in file options block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionEntry {
    pub key: String,
    pub value: LiteralValue,
    pub position: Position,
}

/// A CSIL rule definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    pub rule_type: RuleType,
    pub position: Position,
}

/// Types of CSIL rules
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuleType {
    /// Type definition rule (=)
    TypeDef(TypeExpression),
    /// Group definition rule (=)  
    GroupDef(GroupExpression),
    /// Type choice rule (/=)
    TypeChoice(Vec<TypeExpression>),
    /// Group choice rule (//=)
    GroupChoice(Vec<GroupExpression>),
    /// Service definition
    ServiceDef(ServiceDefinition),
}

/// CSIL type expressions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeExpression {
    /// Built-in types (int, text, bool, etc.)
    Builtin(String),
    /// User-defined type reference
    Reference(String),
    /// Array type with occurrence
    Array {
        element_type: Box<TypeExpression>,
        occurrence: Option<Occurrence>,
    },
    /// Map type
    Map {
        key: Box<TypeExpression>,
        value: Box<TypeExpression>,
        occurrence: Option<Occurrence>,
    },
    /// Group expression
    Group(GroupExpression),
    /// Choice between types (type1 / type2 / type3)
    Choice(Vec<TypeExpression>),
    /// Range expression
    Range {
        start: Option<i64>,
        end: Option<i64>,
        inclusive: bool,
    },
    /// Socket reference
    Socket(String),
    /// Plug reference
    Plug(String),
    /// Literal values
    Literal(LiteralValue),
    /// Type with CDDL control operators (constraints)
    Constrained {
        base_type: Box<TypeExpression>,
        constraints: Vec<ControlOperator>,
    },
}

/// CDDL control operators for type constraints
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlOperator {
    /// Size constraint: .size (min..max) or .size value
    Size(SizeConstraint),
    /// Regular expression constraint: .regex "pattern"  
    Regex(String),
    /// Default value constraint: .default value
    Default(LiteralValue),
    /// Greater than or equal constraint: .ge value
    GreaterEqual(LiteralValue),
    /// Less than or equal constraint: .le value
    LessEqual(LiteralValue),
    /// Greater than constraint: .gt value
    GreaterThan(LiteralValue),
    /// Less than constraint: .lt value
    LessThan(LiteralValue),
    /// Equal to constraint: .eq value
    Equal(LiteralValue),
    /// Not equal constraint: .ne value
    NotEqual(LiteralValue),
    /// Bit control constraint: .bits bits-expression
    Bits(String),
    /// Type intersection constraint: .and type-expression
    And(Box<TypeExpression>),
    /// Subset constraint: .within type-expression
    Within(Box<TypeExpression>),
    /// JSON encoding constraint: .json
    Json,
    /// CBOR encoding constraint: .cbor
    Cbor,
    /// CBOR sequence constraint: .cborseq
    Cborseq,
}

/// Size constraint specifications
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SizeConstraint {
    /// Exact size: .size 5
    Exact(u64),
    /// Range size: .size (1..10)
    Range { min: u64, max: u64 },
    /// Minimum size: .size (5..)
    Min(u64),
    /// Maximum size: .size (..10)
    Max(u64),
}

/// Literal values in CDDL
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LiteralValue {
    Integer(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Bool(bool),
    Null,
    Array(Vec<LiteralValue>),
}

/// CDDL occurrence indicators
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Occurrence {
    /// Optional (?)
    Optional,
    /// Zero or more (*)
    ZeroOrMore,
    /// One or more (+)
    OneOrMore,
    /// Exact count (5)
    Exact(u64),
    /// Range (1*5, *10, 1*)
    Range { min: Option<u64>, max: Option<u64> },
}

/// CSIL group expressions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupExpression {
    pub entries: Vec<GroupEntry>,
}

/// Individual entries in a group
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupEntry {
    pub key: Option<GroupKey>,
    pub value_type: TypeExpression,
    pub occurrence: Option<Occurrence>,
    pub metadata: Vec<FieldMetadata>,
}

/// Group key types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GroupKey {
    /// Bare key (identifier)
    Bare(String),
    /// Type key (type:)
    Type(TypeExpression),
    /// Literal key ("string": or 42:)
    Literal(LiteralValue),
}

/// CSIL service definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDefinition {
    pub operations: Vec<ServiceOperation>,
}

/// A service operation definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceOperation {
    pub name: String,
    pub input_type: TypeExpression,
    pub output_type: TypeExpression,
    pub direction: ServiceDirection,
    pub position: Position,
}

/// Direction of service operation
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServiceDirection {
    /// Unidirectional operation (input -> output)
    Unidirectional,
    /// Bidirectional operation (input <-> output)
    Bidirectional,
    /// Reverse operation (input <- output, rarely used)
    Reverse,
}

/// CSIL field metadata annotations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldMetadata {
    /// Field visibility metadata
    Visibility(FieldVisibility),
    /// Field dependency metadata
    DependsOn {
        field: String,
        value: Option<LiteralValue>,
    },
    /// Validation constraint metadata
    Constraint(ValidationConstraint),
    /// Documentation metadata
    Description(String),
    /// Custom generator hints
    Custom {
        name: String,
        parameters: Vec<MetadataParameter>,
    },
}

/// Field visibility annotations
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldVisibility {
    /// Field is only included in outgoing requests/messages
    SendOnly,
    /// Field is only included in incoming responses/messages
    ReceiveOnly,
    /// Field is included in both directions (default behavior)
    Bidirectional,
}

/// Validation constraint metadata
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ValidationConstraint {
    /// Minimum length for strings/arrays
    MinLength(u64),
    /// Maximum length for strings/arrays
    MaxLength(u64),
    /// Minimum number of items for arrays/maps
    MinItems(u64),
    /// Maximum number of items for arrays/maps
    MaxItems(u64),
    /// Minimum value for numeric types
    MinValue(LiteralValue),
    /// Maximum value for numeric types
    MaxValue(LiteralValue),
    /// Custom validation constraint
    Custom { name: String, value: LiteralValue },
}

/// Parameter for metadata annotations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataParameter {
    pub name: Option<String>,
    pub value: LiteralValue,
}
