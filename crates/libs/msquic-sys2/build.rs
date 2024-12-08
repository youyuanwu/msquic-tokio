// ------------------------------------------------------------
// Copyright 2024 Youyuan Wu
// Licensed under the MIT License (MIT). See License in the repo root for
// license information.
// ------------------------------------------------------------

fn main() {
    if cfg!(windows) {
        let pkg_dir = String::from("build/_deps/msquic_release-src");
        let package_root = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let project_root = package_root + "/../../../";
        // add link dir for fabric support libs. This is propagated to downstream targets
        let abs_search_dir = project_root + &pkg_dir + "/lib";
        println!(
            "cargo:rustc-link-search=native={}",
            std::path::Path::new(&abs_search_dir).display()
        );
    } else if cfg!(unix) {
        // unix can link with apt install.
    } else {
        panic!("unsupport platform")
    }
}
