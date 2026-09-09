fn main() {
    println!("cargo:rerun-if-changed=assets/openaircast.ico");

    #[cfg(target_os = "windows")]
    {
        let icon_path =
            std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
                .join("assets/openaircast.ico");
        if icon_path.is_file() {
            let mut resource = winresource::WindowsResource::new();
            resource.set_icon(icon_path.to_str().expect("UTF-8 icon path"));
            resource.set("ProductName", "OpenAirCast");
            resource.set("FileDescription", "OpenAirCast Control Center");
            resource.set("OriginalFilename", "OpenAirCast.exe");
            resource.compile().expect("compile Windows resources");
        }
    }
}
