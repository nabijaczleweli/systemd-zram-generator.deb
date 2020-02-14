/* SPDX-License-Identifier: MIT */

use crate::generator::run_generator;
use crate::setup::run_device_setup;
use crate::ResultExt;
use failure::Error;
use ini::ini::{Ini, Properties as IniProperties, SectionIntoIter};
use std::borrow::Cow;
use std::cmp;
use std::env;
use std::fmt;
use std::fs;
use std::io::{prelude::*, BufReader};
use std::iter::FromIterator;
use std::path::{self, Path, PathBuf};


pub struct Device {
    pub name: String,
    pub host_memory_limit_mb: Option<u64>,
    pub zram_fraction: f64,
    pub max_zram_size_mb: Option<u64>,
    pub compression_algorithm: Option<String>,
    pub disksize: u64,
}

impl Device {
    fn new(name: String) -> Device {
        Device {
            name,
            host_memory_limit_mb: Some(2 * 1024),
            zram_fraction: 0.25,
            max_zram_size_mb: Some(4 * 1024),
            compression_algorithm: None,
            disksize: 0,
        }
    }

    fn write_optional_mb(f: &mut fmt::Formatter<'_>, val: Option<u64>) -> fmt::Result {
        match val {
            Some(val) => {
                write!(f, "{}", val)?;
                f.write_str("MB")?;
            }
            None => f.write_str("<none>")?,
        }
        Ok(())
    }
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: host-memory-limit=", self.name)?;
        Device::write_optional_mb(f, self.host_memory_limit_mb)?;
        write!(f, " zram-fraction={} max-zram-size=", self.zram_fraction)?;
        Device::write_optional_mb(f, self.max_zram_size_mb)?;
        f.write_str(" compression-algorithm=")?;
        match self.compression_algorithm.as_ref() {
            Some(alg) => f.write_str(alg)?,
            None => f.write_str("<default>")?,
        }
        Ok(())
    }
}


pub struct Config {
    pub root: Cow<'static, str>,
    pub module: ModuleConfig,
}

pub enum ModuleConfig {
    Generator {
        devices: Vec<Device>,
        output_directory: PathBuf,
    },
    DeviceSetup {
        device: Option<Device>,
        name: String,
    },
}


impl Config {
    pub fn parse() -> Result<Config, Error> {
        let root: Cow<'static, str> =
            env::var("ZRAM_GENERATOR_ROOT").map(|mut root| {
                if !root.ends_with(path::is_separator) {
                    root.push('/');
                }
                println!("Using {:?} as root directory", root);
                root.into()
            }).unwrap_or("/".into());

        let mut args = env::args().skip(1);
        let module = match args.next() {
            Some(outdir) => {
                match &outdir[..] {
                    "--setup-device" => {
                        let name = args.next()
                                       .filter(|dev| &dev[0..4] == "zram")
                                       .ok_or_else(|| failure::err_msg("--setup-device requires device argument"))?;
                        ModuleConfig::DeviceSetup { device: Config::read_device(&root, &name)?, name }
                    }
                    _ =>
                        match (args.next(), args.next(), args.next()) {
                            (Some(_), Some(_), None) |
                            (None, None, None) => {
                                let devices = Config::read_all_devices(&root)?;
                                ModuleConfig::Generator { devices, output_directory: PathBuf::from(outdir) }
                            }
                            _ =>
                                return Err(failure::err_msg("This program requires 1 or 3 arguments")),
                        }
                }
            }
            None => return Err(failure::err_msg("This program requires 1 or 3 arguments")),
        };

        Ok(Config { root, module })
    }

    fn read_device(root: &str, name: &str) -> Result<Option<Device>, Error> {
        match Config::read_devices(root)?.find(|(section_name, _)| section_name.as_ref().map(String::as_str) == Some(name)) {
            Some((section_name, section)) => {
                let memtotal_mb = get_total_memory_kb(root)? as f64 / 1024.;

                Config::parse_device(section_name, section, memtotal_mb)
            }
            None => Ok(None),
        }
    }

    fn read_all_devices(root: &str) -> Result<Vec<Device>, Error> {
        let memtotal_mb = get_total_memory_kb(root)? as f64 / 1024.;

        Result::from_iter(Config::read_devices(root)?.map(|(sn, s)| Config::parse_device(sn, s, memtotal_mb)).map(Result::transpose).flatten())
    }

    fn read_devices(root: &str) -> Result<SectionIntoIter, Error> {
        let path = Path::new(root).join("etc/systemd/zram-generator.conf");
        if !path.exists() {
            println!("No configuration file found.");
            return Ok(Ini::new().into_iter());
        }

        Ok(Ini::load_from_file(&path).with_path(&path)?.into_iter())
    }

    fn parse_optional_size(val: &str) -> Result<Option<u64>, Error> {
        Ok(if val == "none" {
            None
        } else {
            Some(val.parse().map_err(|e| format_err!("Failed to parse host-memory-limit \"{}\": {}", val, e))?)
        })
    }

    fn parse_device(section_name: Option<String>, mut section: IniProperties, memtotal_mb: f64) -> Result<Option<Device>, Error> {
        let section_name = section_name.map(Cow::Owned).unwrap_or(Cow::Borrowed("(no title)"));

        if !section_name.starts_with("zram") {
            println!("Ignoring section \"{}\"", section_name);
            return Ok(None);
        }

        let mut dev = Device::new(section_name.into_owned());

        if let Some(val) = section.get("host-memory-limit") {
            dev.host_memory_limit_mb = Config::parse_optional_size(val)?;
        }

        if let Some(val) = section.get("zram-fraction") {
            dev.zram_fraction = val.parse()
                .map_err(|e| format_err!("Failed to parse zram-fraction \"{}\": {}", val, e))?;
        }

        if let Some(val) = section.get("max-zram-size") {
            dev.max_zram_size_mb = Config::parse_optional_size(val)?;
        }

        if let Some((_, val)) = section.remove_entry("compression-algorithm") {
            dev.compression_algorithm = Some(val);
        }

        println!("Found configuration for {}", dev);

        match dev.host_memory_limit_mb {
            Some(limit) if memtotal_mb > limit as f64 => {
                println!("{}: system has too much memory ({:.1}MB), limit is {}MB, ignoring.",
                         dev.name, memtotal_mb, limit);
                Ok(None)
            }
            _ => {
                dev.disksize = (dev.zram_fraction * memtotal_mb) as u64 * 1024 * 1024;
                if let Some(max_mb) = dev.max_zram_size_mb {
                    dev.disksize = cmp::min(dev.disksize, max_mb * 1024 * 1024);
                }
                Ok(Some(dev))
            }
        }
    }

    pub fn run(self) -> Result<(), Error> {
        match self.module {
            ModuleConfig::Generator { devices, output_directory } => run_generator(self.root, devices, output_directory),
            ModuleConfig::DeviceSetup { device, name } => run_device_setup(self.root, device, name),
        }
    }
}


fn get_total_memory_kb(root: &str) -> Result<u64, Error> {
    let path = Path::new(root).join("proc/meminfo");

    for line in BufReader::new(fs::File::open(&path).with_path(&path)?).lines() {
        let line = line?;
        let mut fields = line.split_whitespace();
        if let Some("MemTotal:") = fields.next() {
            if let Some(v) = fields.next() {
                return Ok(v.parse()?);
            }
        }
    }

    Err(format_err!("Couldn't find MemTotal in {}", path.display()))
}
