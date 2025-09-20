use std::env;
use std::path::PathBuf;
use tiny_editor::app::{EditorLogic, TinyApp};
use tiny_editor::{io, tree::Doc};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    let editor_logic = if args.len() > 1 {
        // Load file from path!
        let path = PathBuf::from(&args[1]);
        match io::load(&path) {
            Ok(doc) => {
                EditorLogic::new(doc).with_file(path)
            }
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
        .with_size(1024.0, 1024.0)
        .with_font_size(13.0)
        .with_continuous_rendering(true)
        .run()
}
