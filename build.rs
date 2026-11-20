// The exe's icon: the Rogue Shard logo (icon/app.ico, made from the wiki's logo.png).
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icon/app.ico");
        res.set("ProductName", "Rogue Shard Launcher");
        res.set("FileDescription", "Rogue Shard Launcher");
        if let Err(e) = res.compile() {
            println!("cargo:warning=no exe icon: {e}");
        }
    }
}
