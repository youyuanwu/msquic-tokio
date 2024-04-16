// ------------------------------------------------------------
// Copyright 2024 Youyuan Wu
// Licensed under the MIT License (MIT). See License in the repo root for
// license information.
// ------------------------------------------------------------

use std::{env, path::Path};

fn main() {
    if cfg!(windows) {
        // add link dir for fabric support libs. This is propagated to downstream targets
        let dir = String::from("build\\_deps\\msquic_release-src\\lib");
        let package_root = env::var("CARGO_MANIFEST_DIR").unwrap();
        let abs_dir = package_root + "\\..\\..\\..\\" + &dir;
        println!(
            "cargo:rustc-link-search=native={}",
            Path::new(&abs_dir).display()
        );
    } else if cfg!(unix) {
        panic!("unix not yet supported")
    }
}
