fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=assets/browser-favicon.png");
    println!("cargo:rerun-if-changed=prompts");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set("ProductName", "tensorUI");
        res.set("FileDescription", "tensorUI");
        res.compile()
            .expect("failed to compile Windows resources for app icon");
    }
}
