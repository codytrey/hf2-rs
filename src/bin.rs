use crc::{self, crc16, Hasher16};
use hidapi::{HidApi, HidDevice};
use maplit::hashmap;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use structopt::StructOpt;
use uf2::*;

fn main() -> Result<(), Error> {
    pretty_env_logger::init();

    let args = Opt::from_args();

    let api = HidApi::new().expect("Couldn't find system usb");

    let d = if let (Some(v), Some(p)) = (args.vid, args.pid) {
        api.open(v, p)
            .expect("Are you sure device is plugged in and in uf2 mode?")
    } else {
        println!("no vid/pid provided..");

        let mut device: Option<HidDevice> = None;

        let vendor = hashmap! {
        0x1D50 => vec![0x6110, 0x6112],
        0x239A => vec![0x0035, 0x002D, 0x0015, 0x001B, 0xB000, 0x0024, 0x000F, 0x0013, 0x0021, 0x0022, 0x0031, 0x002B, 0x0037, 0x0035, 0x002F, 0x002B, 0x0033, 0x0034, 0x003D, 0x0018, 0x001C, 0x001E, 0x0027, 0x0022],
        0x04D8=> vec![0xEDB3, 0xEDBE, 0xEF66],
        0x2341=>vec![0x024E, 0x8053, 0x024D],
        0x16D0=>vec![0x0CDA],
        0x03EB=>vec![0x2402],
        0x2886=>vec![0x000D],
        0x1B4F=>vec![0x0D23, 0x0D22],
        0x1209=>vec![0x4D44, 0x2017],

        };

        for device_info in api.devices() {
            if let Some(products) = vendor.get(&device_info.vendor_id) {
                if products.contains(&device_info.product_id) {
                    if let Ok(d) = device_info.open_device(&api) {
                        device = Some(d);
                        break;
                    }
                }
            }
        }
        device.expect("Are you sure device is plugged in and in uf2 mode?")
    };

    println!(
        "found {:?} {:?}",
        d.get_manufacturer_string(),
        d.get_product_string()
    );

    match args.cmd {
        Cmd::resetIntoApp => reset_into_app(&d)?,
        Cmd::resetIntoBootloader => reset_into_bootloader(&d)?,
        Cmd::info => info(&d)?,
        Cmd::bininfo => bininfo(&d)?,
        Cmd::dmesg => dmesg(&d)?,
        Cmd::flash { file, address } => flash(file, address, &d)?,
        Cmd::verify { file, address } => verify(file, address, &d)?,
    }

    Ok(())
}

fn reset_into_app(d: &HidDevice) -> Result<(), Error> {
    let _ = ResetIntoApp {}.send(&mut [], &d)?;
    Ok(())
}

fn reset_into_bootloader(d: &HidDevice) -> Result<(), Error> {
    let _ = ResetIntoBootloader {}.send(&mut [], &d)?;
    Ok(())
}

fn info(d: &HidDevice) -> Result<(), Error> {
    let mut scratch = vec![0_u8; 128];

    let info: InfoResponse = Info {}.send(&mut scratch, &d)?;
    println!("{:?}", info);
    Ok(())
}

fn bininfo(d: &HidDevice) -> Result<(), Error> {
    let mut scratch = vec![0_u8; 64];

    let bininfo: BinInfoResponse = BinInfo {}.send(&mut scratch, &d)?;

    println!(
        "{:?} {:?}kb",
        bininfo,
        bininfo.flash_num_pages * bininfo.flash_page_size / 1024
    );
    Ok(())
}

fn dmesg(d: &HidDevice) -> Result<(), Error> {
    let mut scratch = vec![0_u8; 64];

    // todo, test. not supported on my board
    let dmesg: DmesgResponse = Dmesg {}.send(&mut scratch, &d)?;

    println!("{:?}", dmesg);
    Ok(())
}

fn flash(file: PathBuf, address: u32, d: &HidDevice) -> Result<(), Error> {
    let mut scratch = vec![0_u8; 1024];

    let bininfo: BinInfoResponse = BinInfo {}.send(&mut scratch, &d)?;

    if bininfo.mode != BinInfoMode::Bootloader {
        let _ = StartFlash {}.send(&mut [], &d)?;
    }

    //shouldnt there be a chunking interator for this?
    let mut f = File::open(file)?;
    let mut binary = Vec::new();
    f.read_to_end(&mut binary)?;

    //pad zeros to page size
    let padded_num_pages = (binary.len() as f64 / f64::from(bininfo.flash_page_size)).ceil() as u32;
    let padded_size = padded_num_pages * bininfo.flash_page_size;
    for _i in 0..(padded_size as usize - binary.len()) {
        binary.push(0x0);
    }

    // get checksums of existing pages
    let top_address = address + padded_size as u32;
    let max_pages = bininfo.max_message_size / 2 - 2;
    let steps = max_pages * bininfo.flash_page_size;
    let mut device_checksums = vec![];

    for target_address in (address..top_address).step_by(steps as usize) {
        let pages_left = (top_address - target_address) / bininfo.flash_page_size;

        let num_pages = if pages_left < max_pages {
            pages_left
        } else {
            max_pages
        };
        let checksums: ChksumPagesResponse = ChksumPages {
            target_address,
            num_pages,
        }
        .send(&mut scratch, &d)?;

        for checksum in checksums.iter() {
            device_checksums.push(checksum)
        }
    }

    // only write changed contents
    for (page_index, page) in binary.chunks(bininfo.flash_page_size as usize).enumerate() {
        let mut digest1 = crc16::Digest::new_custom(crc16::X25, 0u16, 0u16, crc::CalcType::Normal);
        digest1.write(&page);

        if digest1.sum16() != device_checksums[page_index] {
            let target_address = address + bininfo.flash_page_size * page_index as u32;
            let _ = WriteFlashPage {
                target_address,
                data: page,
            }
            .send(&mut scratch, &d)?;
        }
    }

    println!("Success");
    let _ = ResetIntoApp {}.send(&mut [], &d)?;
    Ok(())
}

fn verify(file: PathBuf, address: u32, d: &HidDevice) -> Result<(), Error> {
    let mut scratch = vec![0_u8; 1024];

    let bininfo: BinInfoResponse = BinInfo {}.send(&mut scratch, &d)?;

    if bininfo.mode != BinInfoMode::Bootloader {
        let _ = StartFlash {}.send(&mut [], &d)?;
    }

    //shouldnt there be a chunking interator for this?
    let mut f = File::open(file)?;
    let mut binary = Vec::new();
    f.read_to_end(&mut binary)?;

    //pad zeros to page size
    let padded_num_pages = (binary.len() as f64 / f64::from(bininfo.flash_page_size)).ceil() as u32;
    let padded_size = padded_num_pages * bininfo.flash_page_size;
    for _i in 0..(padded_size as usize - binary.len()) {
        binary.push(0x0);
    }

    // get checksums of existing pages
    let top_address = address + padded_size as u32;
    let max_pages = bininfo.max_message_size / 2 - 2;
    let steps = max_pages * bininfo.flash_page_size;
    let mut device_checksums = vec![];

    for target_address in (address..top_address).step_by(steps as usize) {
        let pages_left = (top_address - target_address) / bininfo.flash_page_size;

        let num_pages = if pages_left < max_pages {
            pages_left
        } else {
            max_pages
        };
        let checksums: ChksumPagesResponse = ChksumPages {
            target_address,
            num_pages,
        }
        .send(&mut scratch, &d)?;

        for checksum in checksums.iter() {
            device_checksums.push(checksum)
        }
    }

    let mut binary_checksums = vec![];

    //collect and sums so we can view all mismatches, not just first
    for page in binary.chunks(bininfo.flash_page_size as usize) {
        let mut digest1 = crc16::Digest::new_custom(crc16::X25, 0u16, 0u16, crc::CalcType::Normal);
        digest1.write(&page);

        binary_checksums.push(digest1.sum16());
    }

    //only check as many as our binary has
    assert_eq!(
        &binary_checksums[..binary_checksums.len()],
        &device_checksums[..binary_checksums.len()]
    );
    println!("Success");

    Ok(())
}

fn parse_hex_32(input: &str) -> Result<u32, std::num::ParseIntError> {
    if input.starts_with("0x") {
        u32::from_str_radix(&input[2..], 16)
    } else {
        input.parse::<u32>()
    }
}
fn parse_hex_16(input: &str) -> Result<u16, std::num::ParseIntError> {
    if input.starts_with("0x") {
        u16::from_str_radix(&input[2..], 16)
    } else {
        input.parse::<u16>()
    }
}

#[allow(non_camel_case_types)]
#[derive(StructOpt, Debug, PartialEq)]
pub enum Cmd {
    /// Reset the device into user-space app.
    resetIntoApp,
    /// Reset the device into bootloader, usually for flashing. A BININFO command can be issued to verify that.
    resetIntoBootloader,

    /// Various device information. See INFO_UF2.TXT in UF2 format for details.
    info,

    /// This command states the current mode of the device.
    bininfo,

    /// Return internal log buffer if any.
    dmesg,

    /// Flash
    flash {
        #[structopt(short = "f", name = "file", long = "file")]
        file: PathBuf,
        #[structopt(short = "a", name = "address", long = "address", parse(try_from_str = parse_hex_32))]
        address: u32,
    },

    /// Verify
    verify {
        #[structopt(short = "f", name = "file", long = "file")]
        file: PathBuf,
        #[structopt(short = "a", name = "address", long = "address", parse(try_from_str = parse_hex_32))]
        address: u32,
    },
}

#[derive(Debug, StructOpt)]
#[structopt(name = "uf2", about = "Microsoft HID Flashing Format")]
struct Opt {
    #[structopt(subcommand)]
    cmd: Cmd,

    #[structopt(short = "p", name = "pid", long = "pid", parse(try_from_str = parse_hex_16))]
    pid: Option<u16>,
    #[structopt(short = "v", name = "vid", long = "vid", parse(try_from_str = parse_hex_16))]
    vid: Option<u16>,
}
