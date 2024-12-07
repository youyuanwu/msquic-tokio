// ------------------------------------------------------------
// Copyright 2024 Youyuan Wu
// Licensed under the MIT License (MIT). See License in the repo root for
// license information.
// ------------------------------------------------------------

use windows_bindgen::{bindgen, Result};

fn main() -> Result<()> {
    let log = bindgen([
        "--in",
        "./.windows/winmd/Microsoft.MsQuic.winmd",
        "--out",
        "crates/libs/msquic-sys/src/Microsoft.rs",
        "--filter",
        "Microsoft",
        "--config",
        "implement",
    ])?;
    println!("{}", log);
    Ok(())
}
