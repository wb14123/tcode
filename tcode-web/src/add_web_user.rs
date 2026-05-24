use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::Context;
use argon2::password_hash::PasswordHasher;
use argon2::{Algorithm, Argon2, Params, Version};

use crate::config::{WebUser, WebUsersFile};

pub fn run(username: String, force: bool) -> anyhow::Result<()> {
    if username.trim().is_empty() {
        anyhow::bail!("username must not be empty");
    }

    // Prompt for password (hidden input)
    eprint!("Password: ");
    io::stderr().flush().context("failed to flush stderr")?;
    let password = rpassword::read_password().context("failed to read password from terminal")?;

    if password.trim().is_empty() {
        anyhow::bail!("password must not be empty");
    }

    // Prompt for password confirmation
    eprint!("Confirm password: ");
    io::stderr().flush().context("failed to flush stderr")?;
    let confirm =
        rpassword::read_password().context("failed to read password confirmation from terminal")?;

    if password != confirm {
        anyhow::bail!("passwords do not match");
    }

    // Prompt for session directory
    let default_session_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
        .join("tcode-sessions")
        .join(&username);
    eprint!("Session directory [{}]: ", default_session_dir.display());
    io::stderr().flush().context("failed to flush stderr")?;

    let mut session_dir_input = String::new();
    io::stdin()
        .read_line(&mut session_dir_input)
        .context("failed to read session directory")?;
    let session_dir_str = session_dir_input.trim();

    let session_dir = if session_dir_str.is_empty() {
        default_session_dir
    } else {
        PathBuf::from(session_dir_str)
    };

    // Create session directory if it doesn't exist
    if !session_dir.exists() {
        fs::create_dir_all(&session_dir).with_context(|| {
            format!(
                "failed to create session directory: {}",
                session_dir.display()
            )
        })?;
    }
    fs::set_permissions(&session_dir, fs::Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "failed to set 0700 permissions on {}; you may need to run: chmod 700 {}",
            session_dir.display(),
            session_dir.display()
        )
    })?;

    let session_dir = session_dir
        .canonicalize()
        .context("failed to canonicalize session directory path")?;

    // Prompt for trash directory
    let default_trash_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
        .join(".tcode")
        .join("trash");
    eprint!("Trash directory [{}]: ", default_trash_dir.display());
    io::stderr().flush().context("failed to flush stderr")?;

    let mut trash_dir_input = String::new();
    io::stdin()
        .read_line(&mut trash_dir_input)
        .context("failed to read trash directory")?;
    let trash_dir_str = trash_dir_input.trim();

    let trash_dir = if trash_dir_str.is_empty() {
        default_trash_dir
    } else {
        PathBuf::from(trash_dir_str)
    };

    // Create trash directory if it doesn't exist
    if !trash_dir.exists() {
        fs::create_dir_all(&trash_dir).with_context(|| {
            format!("failed to create trash directory: {}", trash_dir.display())
        })?;
    }
    fs::set_permissions(&trash_dir, fs::Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "failed to set 0700 permissions on {}; you may need to run: chmod 700 {}",
            trash_dir.display(),
            trash_dir.display()
        )
    })?;

    let trash_dir = trash_dir
        .canonicalize()
        .context("failed to canonicalize trash directory path")?;

    // Generate argon2id hash
    let params = Params::new(65536, 3, 4, None).expect("hardcoded params are valid");
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let hash = argon2
        .hash_password(password.as_bytes())
        .map_err(|e| anyhow::anyhow!("failed to hash password: {}", e))?
        .to_string();

    // Write to ~/.tcode/web-users.toml
    let tcode_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
        .join(".tcode");
    fs::create_dir_all(&tcode_dir)
        .with_context(|| format!("failed to create {}", tcode_dir.display()))?;
    let web_users_path = tcode_dir.join("web-users.toml");

    let web_users: WebUsersFile = if web_users_path.exists() {
        let content = fs::read_to_string(&web_users_path)
            .with_context(|| format!("failed to read {}", web_users_path.display()))?;
        toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", web_users_path.display()))?
    } else {
        // Create new file with 0o600 permissions
        let header = "[users]\n";
        fs::write(&web_users_path, header)
            .with_context(|| format!("failed to create {}", web_users_path.display()))?;
        fs::set_permissions(&web_users_path, fs::Permissions::from_mode(0o600)).with_context(
            || format!("failed to set permissions on {}", web_users_path.display()),
        )?;
        WebUsersFile {
            users: std::collections::HashMap::new(),
        }
    };

    let mut users = web_users.users;

    if users.contains_key(&username) && !force {
        anyhow::bail!(
            "User '{}' already exists. Use --force to overwrite.",
            username
        );
    }

    users.insert(
        username.clone(),
        WebUser {
            password_hash: hash,
            session_dir,
            trash_dir,
        },
    );

    let new_file = WebUsersFile { users };
    let toml_content = toml::to_string_pretty(&new_file)?;
    fs::write(&web_users_path, toml_content)
        .with_context(|| format!("failed to write {}", web_users_path.display()))?;
    fs::set_permissions(&web_users_path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set permissions on {}", web_users_path.display()))?;

    println!("User '{}' added successfully.", username);

    Ok(())
}
