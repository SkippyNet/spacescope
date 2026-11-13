fn main() {
    println!("cargo:rerun-if-changed=assets/spacescope.ico");
    #[cfg(windows)]
    {
        let ico = std::path::Path::new("assets/spacescope.ico");
        if ico.exists() {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("assets/spacescope.ico");
            res.set("ProductName", "SpaceScope");
            res.set("FileDescription", "SpaceScope disk space treemap");
            if let Err(e) = res.compile() {
                println!("cargo:warning=could not embed the icon: {e}");
            }
        }
    }
}
