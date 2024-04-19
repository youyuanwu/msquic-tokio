// ------------------------------------------------------------
// Copyright 2024 Youyuan Wu
// Licensed under the MIT License (MIT). See License in the repo root for
// license information.
// ------------------------------------------------------------

use std::{env, path::Path};

fn main() {
    let pkg_dir = String::from("build/_deps/msquic_release-src");
    let package_root = env::var("CARGO_MANIFEST_DIR").unwrap();
    let project_root = package_root + "/../../../";
    let abs_search_dir;
    if cfg!(windows) {
        // add link dir for fabric support libs. This is propagated to downstream targets
        abs_search_dir = project_root + &pkg_dir + "/lib";
    } else if cfg!(unix) {
        abs_search_dir = project_root + "build"; // hack: we create a symlink in the build dir to let ld not deal with .so versions
                                                 //println!("cargo:rustc-link-arg=-Wl,-rpath,{}",abs_search_dir)
    } else {
        panic!("unsupport platform")
    }
    println!(
        "cargo:rustc-link-search=native={}",
        Path::new(&abs_search_dir).display()
    );
    // This does not work for cargo test
    // println!("cargo:rustc-env=LD_LIBRARY_PATH={}", abs_search_dir);
}
