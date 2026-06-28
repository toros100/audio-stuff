use std::env;
use std::path::PathBuf;

// TODO: build options? (disable anything i don't need + enable simd optimizations)

fn main() {
    cc::Build::new()
        .file("vendor/miniaudio.c")
        .compile("miniaudio");

    let bindings = bindgen::Builder::default()
        .header("vendor/miniaudio.h")
        .generate()
        .expect("unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("unable to write bindings");
}
