//! Comprehensive error message regression tests

use crate::{CsilgenError, ErrorLocation, ParseErrorKind, ValidationErrorKind};

#[cfg(test)]
mod error_message_regression_tests {
    use super::*;

    #[test]
    fn test_parse_error_suggestions_service_definition() {
        let error = CsilgenError::parse_error_with_suggestion(
            "Service operation uses wrong arrow",
            Some(ErrorLocation {
                line: 5,
                column: 10,
                file: Some("test.csil".to_string()),
            }),
            ParseErrorKind::ServiceDefinition,
        );

        if let CsilgenError::ParseError(pe) = error {
            assert!(pe.suggestion.is_some());
            let suggestion = pe.suggestion.unwrap();
            assert!(suggestion.contains("->"));
            assert!(suggestion.contains("Example"));
            assert!(suggestion.contains("service ServiceName"));
        } else {
            panic!("Expected ParseError");
        }
    }

    #[test]
    fn test_parse_error_suggestions_bidirectional_service() {
        let error = CsilgenError::parse_error_with_suggestion(
            "Bidirectional service operation error <->",
            None,
            ParseErrorKind::ServiceDefinition,
        );

        if let CsilgenError::ParseError(pe) = error {
            assert!(pe.suggestion.is_some());
            let suggestion = pe.suggestion.unwrap();
            assert!(suggestion.contains("<->"));
            assert!(suggestion.contains("real-time"));
        } else {
            panic!("Expected ParseError");
        }
    }

    #[test]
    fn test_parse_error_suggestions_field_metadata() {
        let error = CsilgenError::parse_error_with_suggestion(
            "Invalid @depends-on syntax error",
            None,
            ParseErrorKind::FieldMetadata,
        );

        if let CsilgenError::ParseError(pe) = error {
            assert!(pe.suggestion.is_some());
            let suggestion = pe.suggestion.unwrap();
            assert!(suggestion.contains("@depends-on"));
            assert!(suggestion.contains("field_name"));
            assert!(suggestion.contains("Example"));
        } else {
            panic!("Expected ParseError");
        }
    }

    #[test]
    fn test_parse_error_suggestions_length_constraints() {
        let error = CsilgenError::parse_error_with_suggestion(
            "Invalid @min-length parameter",
            None,
            ParseErrorKind::FieldMetadata,
        );

        if let CsilgenError::ParseError(pe) = error {
            assert!(pe.suggestion.is_some());
            let suggestion = pe.suggestion.unwrap();
            assert!(suggestion.contains("positive integer"));
            assert!(suggestion.contains("@min-length(8)"));
        } else {
            panic!("Expected ParseError");
        }
    }

    #[test]
    fn test_parse_error_suggestions_missing_braces() {
        let error = CsilgenError::parse_error_with_suggestion(
            "Missing opening brace '{'",
            None,
            ParseErrorKind::MissingToken,
        );

        if let CsilgenError::ParseError(pe) = error {
            assert!(pe.suggestion.is_some());
            let suggestion = pe.suggestion.unwrap();
            assert!(suggestion.contains("group"));
            assert!(suggestion.contains("service"));
            assert!(suggestion.contains("{ field: type }"));
        } else {
            panic!("Expected ParseError");
        }
    }

    #[test]
    fn test_parse_error_suggestions_missing_colon() {
        let error = CsilgenError::parse_error_with_suggestion(
            "Missing colon ':'",
            None,
            ParseErrorKind::MissingToken,
        );

        if let CsilgenError::ParseError(pe) = error {
            assert!(pe.suggestion.is_some());
            let suggestion = pe.suggestion.unwrap();
            assert!(suggestion.contains("name: type"));
            assert!(suggestion.contains("input -> output"));
        } else {
            panic!("Expected ParseError");
        }
    }

    #[test]
    fn test_validation_error_suggestions_undefined_type() {
        let error = CsilgenError::validation_error_with_context(
            "Type 'UserProfile' is not defined",
            None,
            ValidationErrorKind::UndefinedType,
        );

        if let CsilgenError::ValidationError(ve) = error {
            assert!(ve.suggestion.is_some());
            let suggestion = ve.suggestion.unwrap();
            assert!(suggestion.contains("built-in CDDL type"));
            assert!(suggestion.contains("include"));
        } else {
            panic!("Expected ValidationError");
        }
    }

    #[test]
    fn test_validation_error_suggestions_service_undefined_type() {
        let error = CsilgenError::validation_error_with_context(
            "Service input type 'CreateRequest' is not defined",
            None,
            ValidationErrorKind::UndefinedType,
        );

        if let CsilgenError::ValidationError(ve) = error {
            assert!(ve.suggestion.is_some());
            let suggestion = ve.suggestion.unwrap();
            assert!(suggestion.contains("Service"));
            assert!(suggestion.contains("inline group"));
            assert!(suggestion.contains("{ field: type }"));
        } else {
            panic!("Expected ValidationError");
        }
    }

    #[test]
    fn test_validation_error_suggestions_conflicting_visibility() {
        let error = CsilgenError::validation_error_with_context(
            "Field has conflicting visibility metadata",
            None,
            ValidationErrorKind::ConflictingMetadata,
        );

        if let CsilgenError::ValidationError(ve) = error {
            assert!(ve.suggestion.is_some());
            let suggestion = ve.suggestion.unwrap();
            assert!(suggestion.contains("one visibility"));
            assert!(suggestion.contains("@send-only"));
            assert!(suggestion.contains("@receive-only"));
            assert!(suggestion.contains("@bidirectional"));
        } else {
            panic!("Expected ValidationError");
        }
    }

    #[test]
    fn test_validation_error_suggestions_circular_dependency() {
        let error = CsilgenError::validation_error_with_context(
            "Circular dependencies detected",
            None,
            ValidationErrorKind::InvalidDependency,
        );

        if let CsilgenError::ValidationError(ve) = error {
            assert!(ve.suggestion.is_some());
            let suggestion = ve.suggestion.unwrap();
            assert!(suggestion.contains("Circular"));
            assert!(suggestion.contains("directly or indirectly"));
        } else {
            panic!("Expected ValidationError");
        }
    }

    #[test]
    fn test_wasm_error_context_not_found() {
        let error =
            CsilgenError::wasm_error_with_context("Generator 'typescript' not found", "typescript");

        let error_msg = error.to_string();
        assert!(error_msg.contains("list-generators"));
        assert!(error_msg.contains("installed correctly"));
    }

    #[test]
    fn test_wasm_error_context_compile_failure() {
        let error =
            CsilgenError::wasm_error_with_context("Failed to compile WASM module", "rust-gen");

        let error_msg = error.to_string();
        assert!(error_msg.contains("corrupted") || error_msg.contains("incompatible"));
        assert!(error_msg.contains("reinstalling"));
    }

    #[test]
    fn test_wasm_error_context_memory_exceeded() {
        let error = CsilgenError::wasm_error_with_context("Memory limit exceeded", "python-gen");

        let error_msg = error.to_string();
        assert!(error_msg.contains("memory limits"));
        assert!(error_msg.contains("smaller files"));
        assert!(error_msg.contains("python-gen"));
    }

    #[test]
    fn test_wasm_error_context_timeout() {
        let error =
            CsilgenError::wasm_error_with_context("Execution timeout exceeded", "openapi-gen");

        let error_msg = error.to_string();
        assert!(error_msg.contains("too long"));
        assert!(error_msg.contains("execution time limits"));
        assert!(error_msg.contains("openapi-gen"));
    }

    #[test]
    fn test_error_prioritization() {
        let errors = vec![
            CsilgenError::GenerationError("Generation failed".to_string()),
            CsilgenError::parse_error_with_suggestion(
                "Syntax error",
                None,
                ParseErrorKind::CddlSyntax,
            ),
            CsilgenError::WasmError("WASM error".to_string()),
            CsilgenError::validation_error_with_context(
                "Type error",
                None,
                ValidationErrorKind::UndefinedType,
            ),
        ];

        let sorted = CsilgenError::sort_errors_by_priority(errors);

        // Check that critical errors come first
        assert!(sorted[0].priority() <= sorted[1].priority());
        assert!(sorted[1].priority() <= sorted[2].priority());
        assert!(sorted[2].priority() <= sorted[3].priority());

        // Parse syntax errors should be highest priority (1)
        assert_eq!(sorted[0].priority(), 1);

        // Generation errors should be lowest priority (10)
        assert_eq!(sorted[3].priority(), 10);
    }

    #[test]
    fn test_code_snippet_extraction() {
        let source_code = r#"service UserAPI {
  create-user: Request -> Response,
  invalid syntax here
  get-user: {id: int} -> UserProfile
}"#;

        let error = CsilgenError::parse_error_with_snippet(
            "Unexpected token",
            Some(ErrorLocation {
                line: 3,
                column: 10,
                file: None,
            }),
            ParseErrorKind::UnexpectedToken,
            source_code,
        );

        if let CsilgenError::ParseError(pe) = error {
            assert!(pe.snippet.is_some());
            let snippet = pe.snippet.unwrap();
            assert!(snippet.contains("invalid syntax here"));
            assert!(snippet.contains("^ here"));
            assert!(snippet.contains("   1 |"));
            assert!(snippet.contains("   3 |"));
        } else {
            panic!("Expected ParseError with snippet");
        }
    }

    #[test]
    fn test_levenshtein_distance_calculation() {
        // Test exact match
        assert_eq!(
            CsilgenError::suggest_similar_names("test", &["test".to_string()]),
            vec!["test"]
        );

        // Test single character difference
        let suggestions = CsilgenError::suggest_similar_names(
            "user",
            &["users".to_string(), "User".to_string(), "use".to_string()],
        );
        assert!(suggestions.contains(&"users".to_string()));
        assert!(suggestions.contains(&"User".to_string()));

        // Test no close matches
        let suggestions = CsilgenError::suggest_similar_names("xyz", &["abcdefg".to_string()]);
        assert!(suggestions.is_empty());

        // Test multiple matches sorted by distance
        let suggestions = CsilgenError::suggest_similar_names(
            "UserProfile",
            &[
                "UserProfiles".to_string(),
                "UserProfil".to_string(),
                "UserData".to_string(),
                "Profile".to_string(),
            ],
        );

        assert!(!suggestions.is_empty());
        // Should prefer closer matches
        assert!(
            suggestions
                .iter()
                .any(|s| s == "UserProfiles" || s == "UserProfil")
        );
    }

    #[test]
    fn test_common_fix_suggestions() {
        // Test service identifier suggestion
        let suggestion = CsilgenError::suggest_common_fixes("Expected identifier for service name");
        assert!(suggestion.is_some());
        assert!(suggestion.unwrap().contains("valid identifiers"));

        // Test service arrow suggestion
        let suggestion = CsilgenError::suggest_common_fixes("Expected -> in service operation");
        assert!(suggestion.is_some());
        assert!(suggestion.unwrap().contains("InputType -> OutputType"));

        // Test metadata placement suggestion
        let suggestion = CsilgenError::suggest_common_fixes("metadata annotation @ error");
        assert!(suggestion.is_some());
        assert!(suggestion.unwrap().contains("before the field"));

        // Test circular dependency suggestion
        let suggestion = CsilgenError::suggest_common_fixes("circular dependency detected");
        assert!(suggestion.is_some());
        assert!(suggestion.unwrap().contains("optional"));

        // Test undefined type suggestion
        let suggestion = CsilgenError::suggest_common_fixes("Type not found or undefined");
        assert!(suggestion.is_some());
        assert!(suggestion.unwrap().contains("include"));
    }

    #[test]
    fn test_error_fatality_classification() {
        // Critical errors should be fatal
        let parse_error = CsilgenError::parse_error_with_suggestion(
            "Syntax error",
            None,
            ParseErrorKind::CddlSyntax,
        );
        assert!(parse_error.is_fatal());

        let validation_error = CsilgenError::validation_error_with_context(
            "Type not found",
            None,
            ValidationErrorKind::UndefinedType,
        );
        assert!(validation_error.is_fatal());

        // Non-critical errors should not be fatal
        let generation_error = CsilgenError::GenerationError("Template error".to_string());
        assert!(!generation_error.is_fatal());

        let wasm_error = CsilgenError::wasm_error_with_context("Memory warning", "test-gen");
        assert!(!wasm_error.is_fatal());

        // Multiple errors with any fatal should be fatal
        let multi_error = CsilgenError::MultipleErrors(vec![
            generation_error,
            parse_error, // This makes the whole thing fatal
        ]);
        assert!(multi_error.is_fatal());
    }

    #[test]
    fn test_enhanced_wasm_error_messages() {
        // Test "not found" enhancement
        let error = CsilgenError::wasm_error_with_context("Generator not found", "missing-gen");
        assert!(error.to_string().contains("list-generators"));

        // Test compile error enhancement
        let error =
            CsilgenError::wasm_error_with_context("Failed to compile WASM module", "broken-gen");
        assert!(error.to_string().contains("corrupted"));
        assert!(error.to_string().contains("reinstalling"));

        // Test memory error enhancement
        let error = CsilgenError::wasm_error_with_context("Out of memory", "memory-hungry");
        assert!(error.to_string().contains("memory limits"));
        assert!(error.to_string().contains("memory-hungry"));

        // Test timeout error enhancement
        let error = CsilgenError::wasm_error_with_context("Fuel exhausted", "slow-gen");
        assert!(error.to_string().contains("too long"));
        assert!(error.to_string().contains("slow-gen"));

        // Test function error enhancement
        let error =
            CsilgenError::wasm_error_with_context("Function 'generate' not found", "bad-gen");
        assert!(error.to_string().contains("required interface"));
        assert!(error.to_string().contains("incompatible"));
    }

    #[test]
    fn test_error_location_display() {
        let location = ErrorLocation {
            line: 42,
            column: 15,
            file: Some("api.csil".to_string()),
        };
        assert_eq!(location.to_string(), "api.csil:42:15");

        let location_no_file = ErrorLocation {
            line: 10,
            column: 5,
            file: None,
        };
        assert_eq!(location_no_file.to_string(), "10:5");
    }

    #[test]
    fn test_multiple_error_message_extraction() {
        let errors = vec![
            CsilgenError::parse_error_with_suggestion(
                "Parse error 1",
                None,
                ParseErrorKind::CddlSyntax,
            ),
            CsilgenError::validation_error_with_context(
                "Validation error 1",
                None,
                ValidationErrorKind::UndefinedType,
            ),
            CsilgenError::GenerationError("Generation error 1".to_string()),
        ];

        let multi_error = CsilgenError::MultipleErrors(errors);
        let messages = multi_error.get_all_messages();

        assert_eq!(messages.len(), 3);
        assert!(messages.iter().any(|m| m.contains("Parse error 1")));
        assert!(messages.iter().any(|m| m.contains("Validation error 1")));
        assert!(messages.iter().any(|m| m.contains("Generation error 1")));
    }

    #[test]
    fn test_snippet_extraction_edge_cases() {
        // Test single line file
        let single_line = "invalid syntax";
        let error = CsilgenError::parse_error_with_snippet(
            "Error",
            Some(ErrorLocation {
                line: 1,
                column: 5,
                file: None,
            }),
            ParseErrorKind::CddlSyntax,
            single_line,
        );

        if let CsilgenError::ParseError(pe) = error {
            assert!(pe.snippet.is_some());
            let snippet = pe.snippet.unwrap();
            assert!(snippet.contains("invalid syntax"));
            assert!(snippet.contains("^ here"));
        }

        // Test line out of bounds
        let error = CsilgenError::parse_error_with_snippet(
            "Error",
            Some(ErrorLocation {
                line: 100,
                column: 5,
                file: None,
            }),
            ParseErrorKind::CddlSyntax,
            single_line,
        );

        if let CsilgenError::ParseError(pe) = error {
            assert!(pe.snippet.is_none());
        }

        // Test empty file
        let error = CsilgenError::parse_error_with_snippet(
            "Error",
            Some(ErrorLocation {
                line: 1,
                column: 1,
                file: None,
            }),
            ParseErrorKind::CddlSyntax,
            "",
        );

        if let CsilgenError::ParseError(pe) = error {
            assert!(pe.snippet.is_none());
        }
    }

    #[test]
    fn test_enhanced_validation_error_suggestions() {
        // Test constraint conflict
        let error = CsilgenError::validation_error_with_context(
            "Duplicate constraint detected",
            None,
            ValidationErrorKind::ConflictingMetadata,
        );

        if let CsilgenError::ValidationError(ve) = error {
            let suggestion = ve.suggestion.unwrap();
            assert!(suggestion.contains("once per field"));
        }

        // Test service direction error
        let error = CsilgenError::validation_error_with_context(
            "Service operation missing direction",
            None,
            ValidationErrorKind::InvalidServiceOperation,
        );

        if let CsilgenError::ValidationError(ve) = error {
            let suggestion = ve.suggestion.unwrap();
            assert!(suggestion.contains("->"));
            assert!(suggestion.contains("<-"));
            assert!(suggestion.contains("<->"));
        }

        // Test type mismatch
        let error = CsilgenError::validation_error_with_context(
            "Value type mismatch",
            None,
            ValidationErrorKind::TypeMismatch,
        );

        if let CsilgenError::ValidationError(ve) = error {
            let suggestion = ve.suggestion.unwrap();
            assert!(suggestion.contains("expected type"));
        }
    }
}
