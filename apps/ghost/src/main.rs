mod mcp;
mod refmap;
mod tools;
#[cfg(feature = "ort")]
mod vision_model;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ghost",
    about = "Ghost — AI eyes and hands for any desktop app"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP server on stdio (for use with Claude Code / MCP clients).
    Mcp,
    /// Print version.
    Version,
    /// Diagnose environment: AX permissions, Chrome debug port, ShowUI model, recipes.
    Doctor,
    /// Request the macOS permissions Ghost needs (Accessibility, Screen Recording)
    /// by surfacing the system prompts and registering Ghost in System Settings.
    Setup,
}

fn main() {
    // Take a Core-delivered sealing key (RYU_SEAL_KEY / RYU_SEAL_SCOPE) out of the
    // environment before any thread starts, so no child Ghost spawns inherits it.
    // Ghost seals its accessibility snapshots with this key (`refmap::SnapshotStore`).
    let seal_key_error = ryu_crypto::capture_process_seal_key().err();
    // Resolve Ghost's key before the scrub below: without a delivered key it is
    // derived from RYU_MASTER_KEY, which is one of the variables removed next.
    refmap::resolve_process_cipher();
    for name in ryu_crypto::SEAL_ENV_VARS {
        std::env::remove_var(name);
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("ghost: could not start the async runtime: {error}");
            std::process::exit(1);
        }
    };
    runtime.block_on(async_main(seal_key_error));
}

async fn async_main(seal_key_error: Option<anyhow::Error>) {
    // Initialize tracing to stderr so MCP stdout stays clean.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();

    if let Some(error) = seal_key_error {
        tracing::warn!("ghost: ignoring a malformed sealing key delivery: {error}");
    }

    let cli = Cli::parse();
    // Plaintext snapshot files from earlier builds go now, not on the first
    // snapshot call, which may never come.
    refmap::remove_legacy_snapshots_at_startup();
    // Raw macOS capture frames go to an owner-only scratch dir under the data dir
    // (cleared here of crash orphans), never the system temp dir; frames earlier
    // builds left in temp are deleted.
    let capture_scratch =
        ryu_sidecar_runtime::layout::tmp_dir_for(&ryu_sidecar_runtime::ryu_dir(), "ghost");
    match ghost_eyes::capture_scratch::set_scratch_dir(capture_scratch) {
        Ok(()) => {
            ghost_eyes::capture_scratch::remove_legacy_temp_captures();
        }
        Err(error) => tracing::warn!("ghost: could not prepare the capture scratch dir: {error}"),
    }
    match cli.command {
        Commands::Mcp => {
            // Expired sealed snapshots go at startup and then every minute while
            // the server runs, not only when a snapshot tool is next called.
            tokio::spawn(async {
                let mut interval = tokio::time::interval(refmap::SNAPSHOT_PRUNE_INTERVAL);
                loop {
                    interval.tick().await;
                    let _ = tokio::task::spawn_blocking(refmap::prune_expired_snapshots).await;
                }
            });
            mcp::server::run().await;
        }
        Commands::Version => println!("ghost {}", env!("CARGO_PKG_VERSION")),
        Commands::Doctor => run_doctor().await,
        Commands::Setup => run_setup(),
    }
}

/// Request every permission Ghost needs, surfacing the macOS system prompts. Each
/// request registers the binary in the matching System Settings pane; a freshly
/// granted permission only takes effect after Ghost is restarted.
fn setup_guidance(os: &str) -> &'static str {
    match os {
        "macos" => "Enable the responsible host application under System Settings > Privacy & Security, then restart Ghost and its host. Verify with: ghost doctor",
        "linux" => "Native screen capture and global automation require an X11 session. Input learning also needs readable keyboard/mouse devices. Wayland portal capture is not supported by this backend. Verify with: ghost doctor",
        "windows" => "No macOS-style grants are required. Elevated target windows can require additional privileges. Verify with: ghost doctor",
        _ => "Check your host's capture and automation support. Verify with: ghost doctor",
    }
}

fn run_setup() {
    println!("Ghost Setup — checking device access\n");
    for capability in ghost_permissions::ALL {
        let granted = ghost_permissions::request(capability);
        let status = if granted {
            if ghost_permissions::required(capability) {
                "granted"
            } else {
                "no OS prompt required"
            }
        } else if std::env::consts::OS == "macos" {
            "not granted — review the responsible application in System Settings"
        } else {
            "unavailable — check session and device access"
        };
        println!(
            "[{}] {}: {}",
            if granted { "OK" } else { "  " },
            capability.label(),
            status
        );
    }
    println!("\n{}", setup_guidance(std::env::consts::OS));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_instructions_name_the_platforms_actual_gates() {
        assert!(setup_guidance("macos").contains("responsible host application"));
        assert!(setup_guidance("linux").contains("readable keyboard/mouse devices"));
        assert!(setup_guidance("linux").contains("Wayland portal capture is not supported"));
        assert!(!setup_guidance("linux").contains("System Settings"));
        assert!(setup_guidance("windows").contains("Elevated target windows"));
    }

    #[test]
    fn each_subcommand_parses_to_its_variant() {
        assert!(matches!(
            Cli::try_parse_from(["ghost", "mcp"]).unwrap().command,
            Commands::Mcp
        ));
        assert!(matches!(
            Cli::try_parse_from(["ghost", "version"]).unwrap().command,
            Commands::Version
        ));
        assert!(matches!(
            Cli::try_parse_from(["ghost", "doctor"]).unwrap().command,
            Commands::Doctor
        ));
        assert!(matches!(
            Cli::try_parse_from(["ghost", "setup"]).unwrap().command,
            Commands::Setup
        ));
    }

    #[test]
    fn missing_subcommand_is_an_error() {
        assert!(Cli::try_parse_from(["ghost"]).is_err());
    }

    #[test]
    fn unknown_subcommand_is_an_error() {
        assert!(Cli::try_parse_from(["ghost", "haunt"]).is_err());
    }

    #[test]
    fn subcommands_are_case_sensitive() {
        // clap subcommands are case-sensitive by default.
        assert!(Cli::try_parse_from(["ghost", "MCP"]).is_err());
    }
}

async fn run_doctor() {
    println!("Ghost Doctor — environment check\n");

    // 1. Chrome CDP port
    let chrome_ok = ghost_core::cdp::is_available().await;
    println!(
        "[{}] Chrome remote debugging (port 9222): {}",
        if chrome_ok { "OK" } else { "  " },
        if chrome_ok {
            "available".to_string()
        } else {
            "not found — launch Chrome with --remote-debugging-port=9222".to_string()
        }
    );

    // 2. Accessibility permission (real TCC check via AXIsProcessTrusted; building
    // an AX tree is not a reliable signal because the constructor always succeeds).
    let ax_ok = ghost_permissions::accessibility_granted();
    println!(
        "[{}] Accessibility permissions: {}",
        if ax_ok { "OK" } else { "  " },
        if ax_ok {
            "granted".to_string()
        } else {
            "denied — run `ghost setup`, or grant in System Settings > Privacy & Security > Accessibility".to_string()
        }
    );

    // 3. Screen Recording permission (required for screenshots / visual grounding).
    let screen_ok = ghost_permissions::screen_recording_granted();
    println!(
        "[{}] Screen Recording permission: {}",
        if screen_ok { "OK" } else { "  " },
        if screen_ok {
            "granted".to_string()
        } else {
            "denied — run `ghost setup`, or grant in System Settings > Privacy & Security > Screen Recording".to_string()
        }
    );

    // 4. Input Monitoring permission (Ghost's learn mode observes global input).
    let input_ok = ghost_permissions::input_monitoring_granted();
    println!(
        "[{}] Input Monitoring permission: {}",
        if input_ok { "OK" } else { "  " },
        if input_ok {
            "granted".to_string()
        } else {
            "denied — run `ghost setup`, or grant in System Settings > Privacy & Security > Input Monitoring".to_string()
        }
    );

    // 5. ShowUI-2B model file
    let model_path = ghost_core::paths::showui_model_path();
    let model_ok = model_path.exists();
    println!(
        "[{}] ShowUI-2B model ({}): {}",
        if model_ok { "OK" } else { "  " },
        model_path.display(),
        if model_ok {
            "found".to_string()
        } else {
            "not found — download from https://huggingface.co/showlab/ShowUI-2B-ONNX".to_string()
        }
    );

    // 6. Recipe store
    let recipe_count = ghost_core::recipe::store::RecipeStore::open()
        .and_then(|s| s.list())
        .map(|v| v.len())
        .unwrap_or(0);
    println!(
        "[OK] Recipes: {} loaded from {}",
        recipe_count,
        ghost_core::paths::recipes_dir().display()
    );

    println!();
    if ax_ok && screen_ok && input_ok {
        println!("All critical checks passed.");
    } else {
        println!("Fix the issues above, then rerun: ghost doctor");
    }
}
