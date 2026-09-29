//! 把 kb/ 目录下的 markdown 编进二进制。
//!
//! 内置知识库必须随安装包一起到手：装完 sean 就能查，不必先联网。
//! 这里生成 OUT_DIR/kb_embedded.rs，内容形如：
//!     pub(crate) static EMBEDDED_FILES: &[(&str, &str)] = &[("AboutSeanbot/01-Seanbot.md", include_str!(...)), ...];
//! 因此 kb/ 下新增或删除文件不需要改任何 Rust 代码。
//!
//! 注意：只收 .md；index.json 由 kb 模块单独 include（它是更新协议用的元数据，不是条目）。

use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn collect(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, base, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            let rel = path
                .strip_prefix(base)
                .expect("子路径一定在基准目录内")
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
    Ok(())
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo 一定设置该变量"));
    let kb = manifest.join("../..").join("kb");
    println!("cargo:rerun-if-changed={}", kb.display());

    let mut files = Vec::new();
    collect(&kb, &kb, &mut files).unwrap_or_else(|e| panic!("读取 {} 失败：{e}", kb.display()));
    files.sort();

    let mut src = String::from("// 由 build.rs 生成：kb/ 下所有 markdown 的全文。\n");
    src.push_str("pub(crate) static EMBEDDED_FILES: &[(&str, &str)] = &[\n");
    for (rel, abs) in &files {
        println!("cargo:rerun-if-changed={}", abs.display());
        src.push_str(&format!(
            "    ({:?}, include_str!({:?})),\n",
            rel,
            abs.to_string_lossy()
        ));
    }
    src.push_str("];\n");

    let out =
        PathBuf::from(env::var("OUT_DIR").expect("cargo 一定设置该变量")).join("kb_embedded.rs");
    fs::write(&out, src).unwrap_or_else(|e| panic!("写入 {} 失败：{e}", out.display()));
}
