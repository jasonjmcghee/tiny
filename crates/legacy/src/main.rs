use std::env;
use std::path::PathBuf;
use tiny_editor::app::{EditorLogic, TinyApp};
use tiny_editor::config::AppConfig;
use tiny_editor::gpu_ffi_host;
use tiny_editor::{io, Doc};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize GPU FFI for plugins
    gpu_ffi_host::init_ffi();

    // Load configuration (use defaults if parse fails)
    let config = AppConfig::load().unwrap_or_else(|e| {
        eprintln!("❌ Failed to parse init.toml: {}", e);
        eprintln!("   Using default configuration");
        AppConfig::default()
    });

    let args: Vec<String> = env::args().collect();

    // Check for --demo-styles flag
    let demo_styles = args.contains(&"--demo-styles".to_string());

    if demo_styles {
        tiny_ui::text_renderer::DEMO_STYLES_MODE.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    // Find file path (skip flags)
    let file_path = args.iter().skip(1).find(|arg| !arg.starts_with("--"));

    let editor_logic = if let Some(path_str) = file_path {
        // Load file from path!
        let path = PathBuf::from(path_str);
        match io::load(&path) {
            Ok(doc) => EditorLogic::new(doc).with_file(path),
            Err(e) => {
                eprintln!("Failed to load file: {}", e);
                eprintln!("Creating new file instead...");
                let doc = Doc::new();
                EditorLogic::new(doc).with_file(path)
            }
        }
    } else {
        // Use demo text
        let doc = Doc::from_str(&include_str!("../assets/sample.rs").repeat(10));
        EditorLogic::new(doc)
    };

    TinyApp::new(editor_logic)
        .with_config(&config)
        .run()
}
