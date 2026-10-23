fn main() {
    for name in ["WHUT_BUILD_COMMIT", "WHUT_PACKAGE_RELEASE"] {
        println!("cargo:rerun-if-env-changed={name}");
        let value = std::env::var(name).ok().filter(|value| match name {
            "WHUT_BUILD_COMMIT" => {
                value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
            }
            _ => !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()),
        });
        println!(
            "cargo:rustc-env={name}={}",
            value.as_deref().unwrap_or("unknown")
        );
    }
    println!(
        "cargo:rustc-env=WHUT_BUILD_TARGET={}",
        std::env::var("TARGET").expect("Cargo target")
    );
}
