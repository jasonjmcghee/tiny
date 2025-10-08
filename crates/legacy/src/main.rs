use std::env;
use std::path::PathBuf;
use tiny_editor::app::{EditorLogic, TinyApp};
use tiny_editor::config::AppConfig;
use tiny_editor::gpu_ffi_host;
use tiny_editor::{io, Doc};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    gpu_ffi_host::init_ffi();

    let config = AppConfig::load()?;
    let args: Vec<String> = env::args().collect();

    if args.contains(&"--demo-styles".to_string()) {
        tiny_ui::text_renderer::DEMO_STYLES_MODE.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    let editor_logic = if let Some(path_str) = args.iter().skip(1).find(|arg| !arg.starts_with("--")) {
        let path = PathBuf::from(path_str);
        let doc = io::load(&path).unwrap_or_else(|_| Doc::new());
        EditorLogic::new(doc).with_file(path)
    } else {
        let doc = Doc::from_str(&include_str!("../assets/sample.rs").repeat(10));
        EditorLogic::new(doc)
    };

    TinyApp::new(editor_logic).with_config(&config).run()
}
