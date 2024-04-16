// ------------------------------------------------------------
// Copyright 2024 Youyuan Wu
// Licensed under the MIT License (MIT). See License in the repo root for
// license information.
// ------------------------------------------------------------

use windows_bindgen::{bindgen, Result};

fn main() -> Result<()> {
    let log = bindgen(["--etc", "bindings.txt"])?;
    println!("{}", log);
    Ok(())
}
