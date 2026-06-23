// Copyright 2021 The ChromiumOS Authors
// Copyright 2023 Red Hat, Inc.
// Copyright 2024 The Capivara Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::env;

fn gfxstream() -> Result<(), anyhow::Error> {
    // Support the same GFXSTREAM_PATH_DEBUG / GFXSTREAM_PATH_RELEASE / GFXSTREAM_PATH
    // convention as the upstream rutabaga_gfx standalone crate so that build scripts
    // and CI environments are interchangeable.
    let profile = env::var("PROFILE").unwrap_or_default();
    let mut gfxstream_path = if profile == "debug" {
        env::var("GFXSTREAM_PATH_DEBUG").ok()
    } else {
        env::var("GFXSTREAM_PATH_RELEASE").ok()
    };

    // Filter out empty strings (env var set but empty)
    gfxstream_path = gfxstream_path.filter(|s| !s.is_empty());

    // Fall back to the profile-agnostic variable
    if gfxstream_path.is_none() {
        gfxstream_path = env::var("GFXSTREAM_PATH").ok().filter(|s| !s.is_empty());
    }

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if let Some(path) = gfxstream_path {
        // Direct path override — skip pkg-config entirely.
        // This is the recommended path for macOS where pkg-config for
        // gfxstream_backend is not available without `meson install`.
        println!("cargo:rustc-link-lib=gfxstream_backend");
        println!("cargo:rustc-link-search=native={path}");

        // gfxstream_backend is a C++ library; Rust needs to link the C++ runtime.
        // On Apple platforms only clang/libc++ is available.
        if target_os == "macos" {
            println!("cargo:rustc-link-lib=dylib=c++");
            // Frameworks required by libgfxstream_backend.dylib
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
            println!("cargo:rustc-link-lib=framework=IOKit");
            println!("cargo:rustc-link-lib=framework=AppKit");
            println!("cargo:rustc-link-lib=framework=Foundation");
            println!("cargo:rustc-link-lib=framework=Metal");
            println!("cargo:rustc-link-lib=framework=QuartzCore");
        } else if target_os == "linux" || target_os == "android" {
            println!("cargo:rustc-link-lib=dylib=stdc++");
        }
    } else {
        // No env-var override: try pkg-config (standard Linux path).
        let gfxstream_lib = pkg_config::Config::new()
            .atleast_version("0.1.2")
            .probe("gfxstream_backend")
            .map_err(|e| anyhow::anyhow!("pkg-config failed for gfxstream_backend: {e}\n\
                Hint: set GFXSTREAM_PATH to the directory containing \
                libgfxstream_backend.dylib / libgfxstream_backend.so"))?;

        if gfxstream_lib.defines.contains_key("GFXSTREAM_UNSTABLE") {
            // Signal unstable API to the Rust crate
            println!("cargo:rustc-cfg=gfxstream_unstable");
        }

        if target_os == "macos" {
            println!("cargo:rustc-link-lib=dylib=c++");
        } else if target_os == "linux" || target_os == "android" {
            println!("cargo:rustc-link-lib=dylib=stdc++");
        }
    }

    Ok(())
}

fn main() -> Result<(), anyhow::Error> {
    // Skip installing dependencies when generating documents.
    if env::var("CARGO_DOC").is_ok() {
        return Ok(());
    }

    #[cfg(feature = "gpu")]
    {
        // virglrenderer is only available on Linux
        #[cfg(target_os = "linux")]
        {
            pkg_config::Config::new().probe("epoxy")?;
            pkg_config::Config::new().probe("libdrm")?;
            pkg_config::Config::new().probe("virglrenderer")?;
        }
    }

    if env::var("CARGO_FEATURE_GFXSTREAM").is_ok()
        && env::var("CARGO_FEATURE_GFXSTREAM_STUB").is_err()
    {
        gfxstream()?;
    }

    Ok(())
}
