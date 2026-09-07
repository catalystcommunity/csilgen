fn main() {
    println!("cargo:rerun-if-env-changed=CSILGEN_VERSION");

    let version =
        std::env::var("CSILGEN_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    println!("cargo:rustc-env=CSILGEN_VERSION={version}");
}
