/* SPDX-License-Identifier: MIT */

use crate::config::Device;
use anyhow::{anyhow, Context, Result};
use std::cmp;
use std::fs;
use std::iter::FromIterator;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

fn make_parent(of: &Path) -> Result<()> {
    let parent = of
        .parent()
        .ok_or_else(|| anyhow!("Couldn't get parent of {}", of.display()))?;
    fs::create_dir_all(&parent)?;
    Ok(())
}

fn make_symlink(dst: &str, src: &Path) -> Result<()> {
    make_parent(src)?;
    symlink(dst, src)
        .with_context(|| format!("Failed to create symlink {}→{}", src.display(), dst))?;
    Ok(())
}

fn virtualization_container() -> Result<bool> {
    match Command::new("systemd-detect-virt")
        .arg("--quiet")
        .arg("--container")
        .status()
    {
        Ok(status) => Ok(status.success()),
        Err(e) => Err(anyhow!("systemd-detect-virt call failed: {}", e)),
    }
}

pub fn run_generator(devices: &[Device], output_directory: &Path) -> Result<()> {
    if devices.is_empty() {
        println!("No devices configured, exiting.");
        return Ok(());
    }

    if virtualization_container()? {
        println!("Running in a container, exiting.");
        return Ok(());
    }

    let devices_made: Vec<_> = Result::from_iter(
        devices
            .iter()
            .map(|dev| handle_device(output_directory, dev)),
    )?;
    if !devices_made.is_empty() {
        /* We created some devices, let's make sure the module is loaded and they exist */
        if !Path::new("/sys/class/zram-control").exists() {
            Command::new("modprobe")
                .arg("zram")
                .status()
                .context("modprobe call failed")?;
        }

        let max_device = devices_made.into_iter().fold(0, cmp::max);
        if !Path::new("/dev")
            .join(format!("zram{}", max_device))
            .exists()
        {
            while fs::read_to_string("/sys/class/zram-control/hot_add")
                .context("Adding zram device")?
                .trim_end()
                .parse::<u64>()
                .context("Fresh zram device number")?
                < max_device
            {}
        }
    }

    Ok(())
}

fn handle_device(output_directory: &Path, device: &Device) -> Result<u64> {
    let swap_name = format!("dev-{}.swap", device.name);
    println!(
        "Creating {} for /dev/{} ({}MB)",
        swap_name,
        device.name,
        device.disksize / 1024 / 1024
    );

    let swap_path = output_directory.join(&swap_name);

    let contents = format!(
        "\
# Automatically generated by zram-generator

[Unit]
Description=Compressed swap on /dev/{zram_device}
Requires=swap-create@{zram_device}.service
After=swap-create@{zram_device}.service

[Swap]
What=/dev/{zram_device}
Priority=100
",
        zram_device = device.name
    );
    fs::write(&swap_path, contents).with_context(|| {
        format!(
            "Failed to write a swap service into {}",
            swap_path.display()
        )
    })?;

    let symlink_path = output_directory.join("swap.target.wants").join(&swap_name);
    let target_path = format!("../{}", swap_name);
    make_symlink(&target_path, &symlink_path)?;

    device.name[4..]
        .parse()
        .with_context(|| format!("zram device \"{}\" number", device.name))
}
