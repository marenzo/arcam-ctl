use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn main() {
    embuild::espidf::sysenv::output();
    // The device serves a single offline page. Local copies of the same page
    // load the two scripts from docs/wiring/three-r128 with ordinary relative
    // URLs.
    let wiring = PathBuf::from("../docs/wiring");
    println!("cargo:rerun-if-changed=../docs/wiring/guide.html");
    let mut html = fs::read_to_string(wiring.join("guide.html")).expect("wiring guide");
    for file in ["three.min.js", "OrbitControls.js"] {
        let path = format!("three-r128/{file}");
        let tag = format!(r#"<script src="{path}"></script>"#);
        assert!(html.contains(&tag), "missing script tag for {path}");
        println!("cargo:rerun-if-changed=../docs/wiring/{path}");
        let js = fs::read_to_string(wiring.join(&path)).expect("viewer script");
        html = html.replace(&tag, &format!("<script>{js}</script>"));
    }
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"));
    let mut gzip = Command::new("gzip")
        .args(["-n", "-9", "-c"])
        .stdin(Stdio::piped())
        .stdout(fs::File::create(output.join("wiring.html.gz")).expect("wiring output"))
        .spawn()
        .expect("gzip");
    gzip.stdin
        .take()
        .unwrap()
        .write_all(html.as_bytes())
        .expect("compress wiring guide");
    assert!(gzip.wait().expect("wait for gzip").success());
}
