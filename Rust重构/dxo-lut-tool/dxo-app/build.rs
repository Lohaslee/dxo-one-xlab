fn main() {
    // 整包随可执行文件发布，不依赖用户机器上的仓库路径。
    let bundle = std::path::Path::new("../../../Lightning-L口").canonicalize().expect("Lightning 整包缺失");
    let manifest = bundle.join("SHA256SUMS.txt");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let mut source = String::from("const BUNDLE: &[(&str, &str, &[u8])] = &[\n");
    for line in std::fs::read_to_string(&manifest).unwrap().lines().filter(|s| !s.is_empty()) {
        let (sha, rel) = line.split_once("  ").expect("整包清单格式错误");
        let path = bundle.join(rel);
        println!("cargo:rerun-if-changed={}", path.display());
        source.push_str(&format!("({rel:?}, {sha:?}, include_bytes!({path:?})),\n"));
    }
    source.push_str("];\n");
    std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("lightning_bundle.rs"), source).unwrap();
    // tauri-codegen 把 icons/icon.png 嵌为运行期 Dock 图标（macOS setApplicationIconImage），
    // 但 conf 无 bundle.icon 时不监视该文件：换图标不重编译就会嵌旧图。
    println!("cargo:rerun-if-changed=icons/icon.png");
    println!("cargo:rerun-if-changed=icons/AppIcon.icns");
    // 前端资产同样编译期嵌入：不监视则改 UI 不重编译，窗口里跑旧页面
    println!("cargo:rerun-if-changed=ui/index.html");
    tauri_build::build()
}
