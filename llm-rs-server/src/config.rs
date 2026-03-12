//! Provider configuration for upstream LLM backends.

use clap::ValueEnum;

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum Provider {
    #[default]
    OpenRouter,
    Claude,
    OpenAi,
}

impl Provider {
    pub fn default_base_url(&self) -> &'static str {
        match self {
            Provider::Claude => "https://api.anthropic.com",
            Provider::OpenAi => "https://api.openai.com/v1",
            Provider::OpenRouter => "https://openrouter.ai/api/v1",
        }
    }

    pub fn env_var_name(&self) -> &'static str {
        match self {
            Provider::Claude => "ANTHROPIC_API_KEY",
            Provider::OpenAi => "OPENAI_API_KEY",
            Provider::OpenRouter => "OPENROUTER_API_KEY",
        }
    }
}
