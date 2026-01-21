#[cfg(test)]
mod tests {
    use crate::tool::Tool;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use tokio_stream::StreamExt;

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
        let tool = Tool::new("test_tool", "A test tool", |_params: TestParams| {
            tokio_stream::empty::<String>()
        });

        assert_eq!(tool.name, "test_tool");
        assert_eq!(tool.description, "A test tool");
    }

    #[test]
    fn test_tool_param_schema() {
        let tool = Tool::new("test_tool", "A test tool", |_params: TestParams| {
            tokio_stream::empty::<String>()
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
        let tool = Tool::new("greeter", "Greet someone", |params: TestParams| {
            let msg = params.message.clone();
            let count = params.count.unwrap_or(1);
            tokio_stream::iter((0..count).map(move |i| {
                format!("{}. Hello, {}!", i + 1, msg)
            }))
        });

        // Execute with JSON string
        let json_args = r#"{"message": "Rust", "count": 2}"#.to_string();
        let mut stream = tool.execute(json_args);

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
        let tool = Tool::new("greeter", "Greet someone", |params: TestParams| {
            let msg = params.message.clone();
            let count = params.count.unwrap_or(1);
            tokio_stream::iter((0..count).map(move |i| {
                format!("{}. Hello, {}!", i + 1, msg)
            }))
        });

        // Execute without optional field (uses default)
        let json_args = r#"{"message": "Default"}"#.to_string();
        let mut stream = tool.execute(json_args);

        let mut results = Vec::new();
        while let Some(item) = stream.next().await {
            results.push(item);
        }

        assert_eq!(results.len(), 1);
        assert_eq!(results[0], "1. Hello, Default!");
    }

    #[tokio::test]
    async fn test_tool_execute_invalid_json() {
        let tool = Tool::new("greeter", "Greet someone", |params: TestParams| {
            tokio_stream::once(format!("Hello, {}!", params.message))
        });

        // Execute with invalid JSON
        let json_args = r#"not valid json"#.to_string();
        let mut stream = tool.execute(json_args);

        let result = stream.next().await.unwrap();
        assert!(result.starts_with("Error: Failed to parse tool arguments:"));
    }

    #[tokio::test]
    async fn test_tool_execute_missing_required_field() {
        let tool = Tool::new("greeter", "Greet someone", |params: TestParams| {
            tokio_stream::once(format!("Hello, {}!", params.message))
        });

        // Execute with missing required field
        let json_args = r#"{"count": 5}"#.to_string();
        let mut stream = tool.execute(json_args);

        let result = stream.next().await.unwrap();
        assert!(result.starts_with("Error: Failed to parse tool arguments:"));
        assert!(result.contains("message"));
    }

    // Tests for #[tool] macro
    mod macro_tests {
        use crate::tool;
        use tokio_stream::StreamExt;

        /// Search the codebase for a pattern
        #[tool(crate = crate)]
        fn search_code(
            /// The search query pattern
            query: String,
            /// Maximum results to return
            #[serde(default)]
            limit: Option<u32>,
        ) -> impl tokio_stream::Stream<Item = String> {
            let limit = limit.unwrap_or(10);
            tokio_stream::iter((0..limit).map(move |i| {
                format!("Result {}: matched '{}'", i + 1, query)
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
            let mut stream = tool.execute(json_args);

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
            let mut stream = tool.execute(json_args);

            let mut results = Vec::new();
            while let Some(item) = stream.next().await {
                results.push(item);
            }

            // Default limit is 10
            assert_eq!(results.len(), 10);
        }
    }
}
