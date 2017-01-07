extern crate argparse;
extern crate domain;
extern crate futures;
extern crate tokio_core;
extern crate toml;

use std::{fs, process};
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use futures::Future;
use tokio_core::reactor::Core;
use domain::bits::DNameBuf;
use domain::iana::Class;
use domain::master::FileReaderIter;
use domain::server::transport::{UdpTransport, TcpTransport};
use domain::server::service::MockService;
use domain::server::zones::AuthoritativeZones;


//------------ Options -------------------------------------------------------

struct Options {
    configfile: String,
}

impl Options {
    fn new() -> Self { 
        Options {
            configfile: "conf/named.toml".into(),
        }
    }

    fn from_args() -> Self {
        let mut res = Self::new();
        res.parse();
        res
    }

    fn parse(&mut self) {
        use argparse::{ArgumentParser, Store};

        let mut parser = ArgumentParser::new();

        parser.refer(&mut self.configfile)
              .add_option(&["-c", "--config-file"], Store, "config file");

        parser.parse_args_or_exit();
    }
}

impl Options {
    fn config(&self) -> Config {
        let path = PathBuf::from(&self.configfile);
        let mut value = String::new();
        let _ = fs::File::open(&path)
                         .expect("Unable to open config file")
                         .read_to_string(&mut value)
                         .expect("Unable to read config file");
        let table = toml::Parser::new(&value)
                                 .parse()
                                 .expect("Unable to parse config file");
        Config::from_table(&table, path.parent().unwrap())
    }
}


//------------ Config, etc. --------------------------------------------------

struct Zone {
    pub name: DNameBuf,
    pub zonefile: PathBuf,
}

struct Config {
    pub zones: Vec<Zone>,
}

impl Config {
    fn new() -> Self {
        Config {
            zones: Vec::new()
        }
    }

    fn from_table(table: &toml::Table, base: &Path) -> Self {
        let mut res = Self::new();
        let zones = match table.get("zone")
                               .expect("No zones in config file.") {
            &toml::Value::Array(ref array) => array,
            _ => {
                println!("Syntax error in config file.");
                process::exit(1)
            }
        };
        if zones.is_empty() {
            println!("No zones in config file.");
            process::exit(1);
        }
        for zone in zones {
            let zone = match *zone {
                toml::Value::Table(ref table) => table,
                _ => {
                    println!("Syntax error in config file.");
                    process::exit(1);
                }
            };
            let name = match zone.get("name") {
                Some(&toml::Value::String(ref s)) => s,
                _ => {
                    println!("Syntax error in config file.");
                    process::exit(1)
                }
            };
            let mut name = DNameBuf::from_str(&name)
                                    .expect("Syntax error in config file");
            name.append_root().expect("Syntax error in config file");
            let rel_zonefile = match zone.get("zonefile") {
                Some(&toml::Value::String(ref s)) => s,
                _ => {
                    println!("Syntax error in config file.");
                    process::exit(1)
                }
            };
            let rel_zonefile = PathBuf::from(rel_zonefile);
            let mut zonefile = PathBuf::from(base);
            zonefile.push(rel_zonefile);
            res.zones.push(Zone{name: name, zonefile: zonefile})
        }
        res
    }

    fn load_zones(&self) -> AuthoritativeZones {
        let mut res = AuthoritativeZones::new();
        for zone in &self.zones {
            let records = FileReaderIter::new(&zone.zonefile)
                                         .expect("Cannot open zonefile");
            res.load_zone(&zone.name, Class::In, records)
               .expect("Cannot load zone");
        }
        res
    }
}


//------------ main ----------------------------------------------------------

fn main() {
    let options = Options::from_args();
    let zones = options.config().load_zones();

    let addr = SocketAddr::from_str("0.0.0.0:8053").unwrap();
    let mut core = Core::new().unwrap();
    let service = MockService;
    let udp = UdpTransport::bind(&addr, &core.handle(), &service).unwrap();
    let tcp = TcpTransport::bind(&addr, &core.handle(), &service).unwrap();
    println!("Starting server at {}", addr);
    core.run(udp.join(tcp).map(|_| ())).unwrap()
}
