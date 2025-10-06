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

    let editor_logic = if args.len() > 1 {
        // Load file from path!
        let path = PathBuf::from(&args[1]);
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

    TinyApp::new(editor_logic).with_config(&config).run()
}
