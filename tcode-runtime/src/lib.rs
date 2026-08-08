pub mod bootstrap;
pub mod config;
pub mod display_scan;
pub mod fts;
pub mod project;
pub mod protocol;
pub mod server;
pub mod session;
mod system_prompt;

pub use project::project_config_dir;

#[cfg(test)]
mod bootstrap_tests;

#[cfg(test)]
mod display_scan_tests;

#[cfg(test)]
mod project_tests;

#[cfg(test)]
mod server_tests;

#[cfg(test)]
mod session_tests;

#[cfg(test)]
mod system_prompt_tests;
