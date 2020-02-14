/* SPDX-License-Identifier: MIT */

#[macro_use]
extern crate failure;
extern crate ini;

mod config;
mod generator;
mod setup;

use self::config::Config;
use std::fmt;
use std::path::Path;
use std::result;

pub trait ResultExt<T, E>: failure::ResultExt<T, E>
where
    E: fmt::Display,
{
    fn with_path<P: AsRef<Path>>(self, path: P) -> result::Result<T, failure::Context<String>>
    where
        Self: Sized,
    {
        self.with_context(|e| format!("{}: {}", path.as_ref().display(), e))
    }
}

impl<T, E: fmt::Display> ResultExt<T, E> for result::Result<T, E> where
    result::Result<T, E>: failure::ResultExt<T, E>
{}

fn main() {
    std::process::exit(real_main());
}

fn real_main() -> i32 {
    match Config::parse() {
        Ok(config) =>
            match config.run() {
                Ok(()) => 0,
                Err(e) => {
                    println!("{}", e);
                    2
                }
            },
        Err(e) => {
            println!("{}", e);
            1
        },
    }
}
