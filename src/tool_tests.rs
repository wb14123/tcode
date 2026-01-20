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
}
