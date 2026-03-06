#[cfg(test)]
mod tests {
    use crate::tool::{CancellationToken, Tool, ToolContext};
    use schemars::JsonSchema;
    use serde::Deserialize;
    use std::time::Duration;
    use tokio_stream::StreamExt;

    fn test_ctx() -> ToolContext {
        ToolContext { cancel_token: CancellationToken::new() }
    }

    #[derive(Deserialize, JsonSchema)]
    struct TestParams {
        /// A test message
        message: String,
        /// Optional count
        #[serde(default)]
        count: Option<u32>,
    }

    #[test]
    fn test_tool_creation() {
        let tool = Tool::new("test_tool", "A test tool", None, |_ctx: ToolContext, _params: TestParams| {
            tokio_stream::empty::<Result<String, String>>()
        });

        assert_eq!(tool.name, "test_tool");
        assert_eq!(tool.description, "A test tool");
    }

    #[test]
    fn test_tool_param_schema() {
        let tool = Tool::new("test_tool", "A test tool", None, |_ctx: ToolContext, _params: TestParams| {
            tokio_stream::empty::<Result<String, String>>()
        });

        assert_eq!(tool.name, "test_tool");
        assert_eq!(tool.description, "A test tool");

        // Verify schema has expected properties
        let schema_json = serde_json::to_value(&tool.param_schema).unwrap();
        println!(
            "Schema: {}",
            serde_json::to_string_pretty(&schema_json).unwrap()
        );

        let props = schema_json["properties"].as_object().unwrap();
        assert!(props.contains_key("message"));
        assert!(props.contains_key("count"));

        // Verify doc comments are included as descriptions
        let message_schema = &props["message"];
        assert_eq!(
            message_schema["description"].as_str(),
            Some("A test message")
        );

        let count_schema = &props["count"];
        assert_eq!(count_schema["description"].as_str(), Some("Optional count"));
    }

    #[tokio::test]
    async fn test_tool_execute_json() {
        let tool = Tool::new("greeter", "Greet someone", None, |_ctx: ToolContext, params: TestParams| {
            let msg = params.message.clone();
            let count = params.count.unwrap_or(1);
            tokio_stream::iter((0..count).map(move |i| {
                Ok::<_, String>(format!("{}. Hello, {}!", i + 1, msg))
            }))
        });

        // Execute with JSON string
        let json_args = r#"{"message": "Rust", "count": 2}"#.to_string();
        let mut stream = tool.execute(test_ctx(), json_args);

        let mut results = Vec::new();
        while let Some(item) = stream.next().await {
            results.push(item);
        }

        assert_eq!(results.len(), 2);
        assert_eq!(results[0], "1. Hello, Rust!");
        assert_eq!(results[1], "2. Hello, Rust!");
    }

    #[tokio::test]
    async fn test_tool_execute_with_default() {
        let tool = Tool::new("greeter", "Greet someone", None, |_ctx: ToolContext, params: TestParams| {
            let msg = params.message.clone();
            let count = params.count.unwrap_or(1);
            tokio_stream::iter((0..count).map(move |i| {
                Ok::<_, String>(format!("{}. Hello, {}!", i + 1, msg))
            }))
        });

        // Execute without optional field (uses default)
        let json_args = r#"{"message": "Default"}"#.to_string();
        let mut stream = tool.execute(test_ctx(), json_args);

        let mut results = Vec::new();
        while let Some(item) = stream.next().await {
            results.push(item);
        }

        assert_eq!(results.len(), 1);
        assert_eq!(results[0], "1. Hello, Default!");
    }

    #[tokio::test]
    async fn test_tool_execute_invalid_json() {
        let tool = Tool::new("greeter", "Greet someone", None, |_ctx: ToolContext, params: TestParams| {
            tokio_stream::once(Ok::<_, String>(format!("Hello, {}!", params.message)))
        });

        // Execute with invalid JSON
        let json_args = r#"not valid json"#.to_string();
        let mut stream = tool.execute(test_ctx(), json_args);

        let result = stream.next().await.unwrap();
        assert!(result.starts_with("Error: Failed to parse tool arguments:"));
    }

    #[tokio::test]
    async fn test_tool_execute_missing_required_field() {
        let tool = Tool::new("greeter", "Greet someone", None, |_ctx: ToolContext, params: TestParams| {
            tokio_stream::once(Ok::<_, String>(format!("Hello, {}!", params.message)))
        });

        // Execute with missing required field
        let json_args = r#"{"count": 5}"#.to_string();
        let mut stream = tool.execute(test_ctx(), json_args);

        let result = stream.next().await.unwrap();
        assert!(result.starts_with("Error: Failed to parse tool arguments:"));
        assert!(result.contains("message"));
    }

    #[tokio::test]
    async fn test_tool_execute_error() {
        let tool = Tool::new("fallible", "A fallible tool", None, |_ctx: ToolContext, _params: TestParams| {
            tokio_stream::once(Err::<String, _>("something went wrong".to_string()))
        });

        let json_args = r#"{"message": "test"}"#.to_string();
        let mut stream = tool.execute(test_ctx(), json_args);

        let result = stream.next().await.unwrap();
        assert_eq!(result, "Error: something went wrong");
    }

    #[test]
    fn test_tool_default_timeout() {
        let tool = Tool::new("test", "A test tool", None, |_ctx: ToolContext, _: TestParams| {
            tokio_stream::empty::<Result<String, String>>()
        });

        assert_eq!(tool.timeout, None);
    }

    #[test]
    fn test_tool_with_timeout() {
        let tool = Tool::new(
            "test",
            "A test tool",
            Some(Duration::from_secs(30)),
            |_ctx: ToolContext, _: TestParams| tokio_stream::empty::<Result<String, String>>(),
        );

        assert_eq!(tool.timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn test_tool_with_timeout_millis() {
        let tool = Tool::new(
            "test",
            "A test tool",
            Some(Duration::from_millis(500)),
            |_ctx: ToolContext, _: TestParams| tokio_stream::empty::<Result<String, String>>(),
        );

        assert_eq!(tool.timeout, Some(Duration::from_millis(500)));
    }

    #[tokio::test]
    async fn test_tool_timeout_enforcement() {
        let tool = Tool::new(
            "slow",
            "A slow tool",
            Some(Duration::from_millis(100)),
            |_ctx: ToolContext, _: TestParams| {
                async_stream::stream! {
                    // Yield first item immediately
                    yield Ok::<_, String>("first".to_string());
                    // Then sleep longer than timeout
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    yield Ok::<_, String>("second".to_string());
                }
            },
        );

        let json_args = r#"{"message": "test"}"#.to_string();
        let mut stream = tool.execute(test_ctx(), json_args);

        let mut results = Vec::new();
        while let Some(item) = stream.next().await {
            results.push(item);
        }

        // Should get "first", then timeout error, but not "second"
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], "first");
        assert!(results[1].contains("timed out"));
    }

    #[tokio::test]
    async fn test_tool_timeout_while_waiting_for_first_item() {
        let tool = Tool::new(
            "slow",
            "A slow tool",
            Some(Duration::from_millis(50)),
            |_ctx: ToolContext, _: TestParams| {
                async_stream::stream! {
                    // Sleep longer than timeout before yielding anything
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    yield Ok::<_, String>("should not see this".to_string());
                }
            },
        );

        let json_args = r#"{"message": "test"}"#.to_string();
        let mut stream = tool.execute(test_ctx(), json_args);

        let result = stream.next().await.unwrap();
        assert!(result.contains("timed out"), "Expected timeout, got: {}", result);
    }

    #[tokio::test]
    async fn test_tool_cancellation() {
        let tool = Tool::new(
            "slow",
            "A slow tool",
            None,
            |_ctx: ToolContext, _: TestParams| {
                async_stream::stream! {
                    yield Ok::<_, String>("first".to_string());
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    yield Ok::<_, String>("second".to_string());
                }
            },
        );

        let cancel_token = CancellationToken::new();
        let ctx = ToolContext { cancel_token: cancel_token.clone() };

        let json_args = r#"{"message": "test"}"#.to_string();
        let mut stream = tool.execute(ctx, json_args);

        // Get first item
        let first = stream.next().await.unwrap();
        assert_eq!(first, "first");

        // Cancel
        cancel_token.cancel();

        // Should get cancellation message
        let cancelled = stream.next().await.unwrap();
        assert!(cancelled.contains("cancelled"), "Expected cancelled, got: {}", cancelled);

        // Stream should end
        assert!(stream.next().await.is_none());
    }

    // Tests for #[tool] macro
    mod macro_tests {
        use crate::tool;
        use crate::tool::{CancellationToken, ToolContext};
        use tokio_stream::StreamExt;

        fn test_ctx() -> ToolContext {
            ToolContext { cancel_token: CancellationToken::new() }
        }

        /// Search the codebase for a pattern
        #[tool]
        fn search_code(
            /// The search query pattern
            query: String,
            /// Maximum results to return
            #[serde(default)]
            limit: Option<u32>,
        ) -> impl tokio_stream::Stream<Item = Result<String, String>> {
            let limit = limit.unwrap_or(10);
            tokio_stream::iter((0..limit).map(move |i| {
                Ok(format!("Result {}: matched '{}'", i + 1, query))
            }))
        }

        #[test]
        fn test_tool_macro_basic() {
            let tool = search_code_tool();

            assert_eq!(tool.name, "search_code");
            assert_eq!(tool.description, "Search the codebase for a pattern");
        }

        #[test]
        fn test_tool_macro_schema() {
            let tool = search_code_tool();

            let schema_json = serde_json::to_value(&tool.param_schema).unwrap();
            let props = schema_json["properties"].as_object().unwrap();

            // Check field names preserved
            assert!(props.contains_key("query"));
            assert!(props.contains_key("limit"));

            // Check doc comments preserved
            assert_eq!(
                props["query"]["description"].as_str(),
                Some("The search query pattern")
            );
            assert_eq!(
                props["limit"]["description"].as_str(),
                Some("Maximum results to return")
            );
        }

        #[tokio::test]
        async fn test_tool_macro_execute() {
            let tool = search_code_tool();

            let json_args = r#"{"query": "foo", "limit": 3}"#.to_string();
            let mut stream = tool.execute(test_ctx(), json_args);

            let mut results = Vec::new();
            while let Some(item) = stream.next().await {
                results.push(item);
            }

            assert_eq!(results.len(), 3);
            assert_eq!(results[0], "Result 1: matched 'foo'");
            assert_eq!(results[1], "Result 2: matched 'foo'");
            assert_eq!(results[2], "Result 3: matched 'foo'");
        }

        #[tokio::test]
        async fn test_tool_macro_with_default() {
            let tool = search_code_tool();

            // Without optional limit parameter
            let json_args = r#"{"query": "bar"}"#.to_string();
            let mut stream = tool.execute(test_ctx(), json_args);

            let mut results = Vec::new();
            while let Some(item) = stream.next().await {
                results.push(item);
            }

            // Default limit is 10
            assert_eq!(results.len(), 10);
        }

        /// A slow operation
        #[tool(timeout_ms = 60000)]
        fn slow_operation(
            /// The data to process
            data: String,
        ) -> impl tokio_stream::Stream<Item = Result<String, String>> {
            tokio_stream::once(Ok(format!("Processed: {}", data)))
        }

        #[test]
        fn test_tool_macro_with_timeout() {
            let tool = slow_operation_tool();

            assert_eq!(tool.name, "slow_operation");
            assert_eq!(tool.description, "A slow operation");
            assert_eq!(
                tool.timeout,
                Some(std::time::Duration::from_millis(60000))
            );
        }

        /// Quick operation without timeout
        #[tool]
        fn quick_operation(
            /// The query
            query: String,
        ) -> impl tokio_stream::Stream<Item = Result<String, String>> {
            tokio_stream::once(Ok(format!("Result: {}", query)))
        }

        #[test]
        fn test_tool_macro_without_timeout() {
            let tool = quick_operation_tool();

            assert_eq!(tool.name, "quick_operation");
            assert!(tool.timeout.is_none());
        }

        /// A fallible operation
        #[tool]
        fn fallible_operation(
            /// Whether to fail
            should_fail: bool,
        ) -> impl tokio_stream::Stream<Item = Result<String, String>> {
            if should_fail {
                tokio_stream::once(Err("intentional failure".to_string()))
            } else {
                tokio_stream::once(Ok("success".to_string()))
            }
        }

        #[tokio::test]
        async fn test_tool_macro_error_handling() {
            let tool = fallible_operation_tool();

            // Test success case
            let mut stream = tool.execute(test_ctx(), r#"{"should_fail": false}"#.to_string());
            let result = stream.next().await.unwrap();
            assert_eq!(result, "success");

            // Test error case
            let mut stream = tool.execute(test_ctx(), r#"{"should_fail": true}"#.to_string());
            let result = stream.next().await.unwrap();
            assert_eq!(result, "Error: intentional failure");
        }

        /// A tool with ToolContext
        #[tool]
        fn ctx_aware_tool(
            ctx: ToolContext,
            /// Some input data
            data: String,
        ) -> impl tokio_stream::Stream<Item = Result<String, String>> {
            let is_cancelled = ctx.cancel_token.is_cancelled();
            tokio_stream::once(Ok(format!("data={}, cancelled={}", data, is_cancelled)))
        }

        #[tokio::test]
        async fn test_tool_macro_with_tool_context() {
            let tool = ctx_aware_tool_tool();

            // ToolContext should not appear in the schema
            let schema_json = serde_json::to_value(&tool.param_schema).unwrap();
            let props = schema_json["properties"].as_object().unwrap();
            assert!(props.contains_key("data"));
            assert!(!props.contains_key("ctx"));

            let mut stream = tool.execute(test_ctx(), r#"{"data": "hello"}"#.to_string());
            let result = stream.next().await.unwrap();
            assert_eq!(result, "data=hello, cancelled=false");
        }
    }
}
