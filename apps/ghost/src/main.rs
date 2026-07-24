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

#[tokio::main]
async fn main() {
    // Initialize tracing to stderr so MCP stdout stays clean.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Mcp => mcp::server::run().await,
        Commands::Version => println!("ghost {}", env!("CARGO_PKG_VERSION")),
        Commands::Doctor => run_doctor().await,
        Commands::Setup => run_setup(),
    }
}

/// Request every permission Ghost needs, surfacing the macOS system prompts. Each
/// request registers the binary in the matching System Settings pane; a freshly
/// granted permission only takes effect after Ghost is restarted.
fn run_setup() {
    println!("Ghost Setup — requesting permissions\n");

    let ax = ghost_permissions::request_accessibility();
    println!(
        "[{}] Accessibility: {}",
        if ax { "OK" } else { "  " },
        if ax {
            "granted"
        } else {
            "prompt requested — enable Ghost in the pane that opened"
        }
    );

    let screen = ghost_permissions::request_screen_recording();
    println!(
        "[{}] Screen Recording: {}",
        if screen { "OK" } else { "  " },
        if screen {
            "granted"
        } else {
            "prompt requested — enable Ghost in the pane that opened"
        }
    );

    let input = ghost_permissions::request_input_monitoring();
    println!(
        "[{}] Input Monitoring: {}",
        if input { "OK" } else { "  " },
        if input {
            "granted"
        } else {
            "prompt requested — enable Ghost in the pane that opened"
        }
    );

    println!();
    if ax && screen && input {
        println!("All permissions granted. Ghost is ready.");
    } else {
        println!(
            "Enable Ghost under System Settings > Privacy & Security for anything not yet \
             granted, then fully restart Ghost (and the MCP client that launched it) for the \
             grant to apply. Verify with: ghost doctor"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    let model_path = dirs::home_dir()
        .unwrap_or_default()
        .join(".ghost")
        .join("models")
        .join("showui-2b.onnx");
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
        "[OK] Recipes: {} loaded from ~/.ghost/recipes/",
        recipe_count
    );

    println!();
    if ax_ok && screen_ok && input_ok {
        println!("All critical checks passed.");
    } else {
        println!("Fix the issues above, then rerun: ghost doctor");
    }
}
