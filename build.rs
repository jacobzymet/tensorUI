use std::{env, fs, path::PathBuf};

const CHAT_STYLE_FILES: &[&str] = &[
    "src/ui/chat/styles/base.css",
    "src/ui/chat/styles/conversation.css",
    "src/ui/chat/styles/content.css",
    "src/ui/chat/styles/composer.css",
    "src/ui/chat/styles/activity.css",
    "src/ui/chat/styles/projects.css",
    "src/ui/chat/styles/notifications.css",
    "src/ui/chat/styles/profiles.css",
    "src/ui/chat/styles/bots.css",
    "src/ui/chat/styles/terminal.css",
];

const CHAT_SCRIPT_FILES: &[&str] = &[
    "src/ui/chat/scripts/state.js",
    "src/ui/chat/scripts/input.js",
    "src/ui/chat/scripts/controls.js",
    "src/ui/chat/scripts/render.js",
    "src/ui/chat/scripts/bots.js",
    "src/ui/chat/scripts/runtime.js",
    "src/ui/chat/scripts/terminal.js",
];

fn bundle_ui(files: &[&str], output: &str) {
    let mut bundled = String::new();
    for path in files {
        println!("cargo:rerun-if-changed={path}");
        bundled.push_str(
            &fs::read_to_string(path)
                .unwrap_or_else(|error| panic!("failed to read {path}: {error}")),
        );
    }

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is not set")).join(output);
    fs::write(&output, bundled)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", output.display()));
}

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=assets/browser-favicon.png");
    println!("cargo:rerun-if-changed=prompts");

    bundle_ui(CHAT_STYLE_FILES, "chat.css");
    bundle_ui(CHAT_SCRIPT_FILES, "chat.js");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set("ProductName", "TensorMI Harness");
        res.set(
            "FileDescription",
            "A local, lightweight, open source LLM harness for humanity",
        );
        res.compile()
            .expect("failed to compile Windows resources for app icon");
    }
}
