/* SPDX-License-Identifier: MIT */

use crate::config::Device;
use crate::ResultExt;
use failure::Error;
use std::borrow::Cow;
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;


pub fn run_device_setup(root: Cow<'static, str>, device: Option<Device>, device_name: String) -> Result<(), Error> {
    let device = device.ok_or_else(|| format_err!("Device {} not found", device_name))?;

    let device_sysfs_path = Path::new(&root[..]).join("sys/block/").join(&device_name);
    let disksize_path = device_sysfs_path.join("disksize");
    fs::write(&disksize_path, format!("{}", device.disksize)).with_path(disksize_path)?;

    match Command::new("mkswap").arg(Path::new(&root[..]).join("dev/").join(device_name)).status() {
        Ok(status) =>
            match status.code() {
                Some(0) => Ok(()),
                Some(code) => Err(format_err!("mkswap failed with exit code {}", code)),
                None => Err(format_err!("mkswap terminated by signal {}",
                                        status.signal().expect("on unix, status status.code() is None iff status.signal() isn't; \
                                                                this expect() will never panic, save for an stdlib bug"))),
            },
        Err(e) => Err(format_err!("mkswap call failed: {}", e)),
    }
}
