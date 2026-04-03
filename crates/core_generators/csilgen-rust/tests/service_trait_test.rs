use csilgen_common::GeneratorConfig;
use csilgen_core::ast::*;
use csilgen_core::lexer::Position;
use csilgen_rust::generate_rust_code;
use std::collections::HashMap;

fn default_position() -> Position {
    Position {
        line: 1,
        column: 1,
        offset: 0,
    }
}

fn default_config() -> GeneratorConfig {
    GeneratorConfig {
        target: "rust".to_string(),
        output_dir: "/tmp/output".to_string(),
        options: HashMap::new(),
    }
}

fn make_spec(rules: Vec<Rule>) -> CsilSpec {
    CsilSpec {
        imports: vec![],
        options: None,
        rules,
    }
}

fn make_service_rule(name: &str, operations: Vec<ServiceOperation>) -> Rule {
    Rule {
        name: name.to_string(),
        rule_type: RuleType::ServiceDef(ServiceDefinition { operations }),
        position: default_position(),
    }
}

fn make_operation(name: &str, input: &str, output: &str) -> ServiceOperation {
    ServiceOperation {
        name: name.to_string(),
        input_type: TypeExpression::Reference(input.to_string()),
        output_type: TypeExpression::Reference(output.to_string()),
        direction: ServiceDirection::Unidirectional,
        position: default_position(),
    }
}

#[test]
fn service_trait_has_context_associated_type() {
    let spec = make_spec(vec![make_service_rule(
        "DomainKeys",
        vec![make_operation(
            "get-domain-keys",
            "EmptyRequest",
            "GetDomainKeysResponse",
        )],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        code.contains("type Context;"),
        "trait should have Context associated type, got:\n{code}"
    );
}

#[test]
fn service_trait_methods_take_context_parameter() {
    let spec = make_spec(vec![make_service_rule(
        "DomainKeys",
        vec![make_operation(
            "get-domain-keys",
            "EmptyRequest",
            "GetDomainKeysResponse",
        )],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        code.contains("ctx: &Self::Context"),
        "methods should take ctx: &Self::Context, got:\n{code}"
    );
    assert!(
        code.contains("fn get_domain_keys(&self, ctx: &Self::Context, input: EmptyRequest)"),
        "ctx should come after &self and before input, got:\n{code}"
    );
}

#[test]
fn service_trait_methods_return_result_with_service_error() {
    let spec = make_spec(vec![make_service_rule(
        "DomainKeys",
        vec![make_operation(
            "get-domain-keys",
            "EmptyRequest",
            "GetDomainKeysResponse",
        )],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        code.contains("-> Result<GetDomainKeysResponse, ServiceError>"),
        "methods should return Result<T, ServiceError>, got:\n{code}"
    );
}

#[test]
fn service_error_struct_is_generated() {
    let spec = make_spec(vec![make_service_rule(
        "DomainKeys",
        vec![make_operation(
            "get-domain-keys",
            "EmptyRequest",
            "GetDomainKeysResponse",
        )],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        code.contains("pub struct ServiceError {"),
        "should generate ServiceError struct, got:\n{code}"
    );
    assert!(
        code.contains("pub code: i32"),
        "should have code field, got:\n{code}"
    );
    assert!(
        code.contains("pub message: String"),
        "should have message field, got:\n{code}"
    );
}

#[test]
fn service_error_implements_display_and_error() {
    let spec = make_spec(vec![make_service_rule(
        "DomainKeys",
        vec![make_operation(
            "get-domain-keys",
            "EmptyRequest",
            "GetDomainKeysResponse",
        )],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        code.contains("impl std::fmt::Display for ServiceError"),
        "should implement Display, got:\n{code}"
    );
    assert!(
        code.contains("impl std::error::Error for ServiceError"),
        "should implement Error, got:\n{code}"
    );
}

#[test]
fn multiple_operations_all_follow_pattern() {
    let spec = make_spec(vec![make_service_rule(
        "Handshake",
        vec![
            make_operation("handshake", "HandshakeRequest", "HandshakeResponse"),
            make_operation("verify", "VerifyRequest", "VerifyResponse"),
        ],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        code.contains(
            "fn handshake(&self, ctx: &Self::Context, input: HandshakeRequest) -> Result<HandshakeResponse, ServiceError>"
        ),
        "handshake method should follow pattern, got:\n{code}"
    );
    assert!(
        code.contains(
            "fn verify(&self, ctx: &Self::Context, input: VerifyRequest) -> Result<VerifyResponse, ServiceError>"
        ),
        "verify method should follow pattern, got:\n{code}"
    );
}

#[test]
fn multiple_services_each_get_context() {
    let spec = make_spec(vec![
        make_service_rule(
            "DomainKeys",
            vec![make_operation(
                "get-domain-keys",
                "EmptyRequest",
                "GetDomainKeysResponse",
            )],
        ),
        make_service_rule(
            "Handshake",
            vec![make_operation(
                "handshake",
                "HandshakeRequest",
                "HandshakeResponse",
            )],
        ),
    ]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    let context_count = code.matches("type Context;").count();
    assert_eq!(
        context_count, 2,
        "each service trait should have its own Context type, found {context_count} in:\n{code}"
    );

    // ServiceError should only be generated once
    let error_enum_count = code.matches("pub struct ServiceError {").count();
    assert_eq!(
        error_enum_count, 1,
        "ServiceError should be generated exactly once, found {error_enum_count} in:\n{code}"
    );
}

#[test]
fn no_service_error_when_no_services() {
    let spec = make_spec(vec![Rule {
        name: "MyType".to_string(),
        rule_type: RuleType::TypeDef(TypeExpression::Builtin("text".to_string())),
        position: default_position(),
    }]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        !code.contains("ServiceError"),
        "should not generate ServiceError when no services exist, got:\n{code}"
    );
}

#[test]
fn bidirectional_operations_include_context() {
    let spec = make_spec(vec![make_service_rule(
        "UserService",
        vec![ServiceOperation {
            name: "subscribe-updates".to_string(),
            input_type: TypeExpression::Reference("UserID".to_string()),
            output_type: TypeExpression::Reference("UserUpdate".to_string()),
            direction: ServiceDirection::Bidirectional,
            position: default_position(),
        }],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        code.contains("fn subscribe_updates(&self, ctx: &Self::Context, input: UserID) -> Result<UserUpdate, ServiceError>"),
        "bidirectional request method should have context, got:\n{code}"
    );
    assert!(
        code.contains("fn subscribe_updates_stream(&self, ctx: &Self::Context) -> Result<Box<dyn Stream<Item = UserUpdate>>, ServiceError>"),
        "bidirectional stream method should have context, got:\n{code}"
    );
}

#[test]
fn reverse_operations_include_context() {
    let spec = make_spec(vec![make_service_rule(
        "CallbackService",
        vec![ServiceOperation {
            name: "on-event".to_string(),
            input_type: TypeExpression::Reference("EventInput".to_string()),
            output_type: TypeExpression::Reference("EventOutput".to_string()),
            direction: ServiceDirection::Reverse,
            position: default_position(),
        }],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        code.contains("fn on_event_callback(&self, ctx: &Self::Context, output: EventOutput) -> Result<(), ServiceError>"),
        "reverse callback method should have context, got:\n{code}"
    );
}
