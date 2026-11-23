// puts the icon and version info into the windows exe, so explorer and the taskbar show them
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/icon.ico");
        resource.set("FileDescription", "3dsrecomp");
        resource.set("ProductName", "3dsrecomp");
        if let Err(error) = resource.compile() {
            // no resource compiler around, the exe just goes without an icon
            println!("cargo:warning=could not embed the icon, {error}");
        }
    }
}
