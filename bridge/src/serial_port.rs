//! Find the CYD's serial port without being told its name.
//!
//! The ESP32-2432S028R ("Cheap Yellow Display") talks to the host through an
//! on-board USB-serial chip, so its port can be recognised by USB vendor and
//! product id. That beats any default name: the right default differs per OS
//! (`COM7`, `/dev/ttyUSB0`, `/dev/cu.usbserial-*`) and changes when the device
//! is replugged while another adapter holds the lower number.
//!
//! The selection logic ([`pick`]) is separate from enumeration ([`detect`]) so
//! it can be tested on any platform without hardware attached.

use serialport::SerialPortType;

/// USB-serial chips found on CYD boards: (vendor id, product id, name).
pub const KNOWN_CHIPS: &[(u16, u16, &str)] =
    &[(0x1a86, 0x7523, "CH340"), (0x10c4, 0xea60, "CP2102")];

/// One enumerated serial port, reduced to what selection needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    /// USB (vendor id, product id); `None` for non-USB ports.
    pub usb: Option<(u16, u16)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    Found {
        port: String,
        chip: &'static str,
        /// Other ports that also matched a known chip, in case the first is
        /// not the CYD (e.g. a second ESP32 board on the same machine).
        also_matching: Vec<String>,
    },
    NotFound {
        /// Every port that was seen, for the "is it plugged in?" message.
        seen: Vec<String>,
    },
}

fn chip_name(usb: Option<(u16, u16)>) -> Option<&'static str> {
    let (vid, pid) = usb?;
    KNOWN_CHIPS
        .iter()
        .find(|(v, p, _)| *v == vid && *p == pid)
        .map(|(_, _, name)| *name)
}

/// macOS lists every USB-serial device twice: `/dev/tty.*` (dial-in, blocks on
/// open until carrier detect) and `/dev/cu.*` (call-out, opens immediately).
/// Only the call-out node is usable here.
fn is_macos_dial_in(name: &str) -> bool {
    name.starts_with("/dev/tty.")
}

/// Choose the CYD among enumerated ports. Deterministic: candidates are
/// considered in name order, so the result does not depend on OS enumeration
/// order.
pub fn pick(candidates: &[Candidate]) -> Detection {
    let mut matching: Vec<(&Candidate, &'static str)> = candidates
        .iter()
        .filter(|c| !is_macos_dial_in(&c.name))
        .filter_map(|c| chip_name(c.usb).map(|chip| (c, chip)))
        .collect();
    matching.sort_by(|a, b| a.0.name.cmp(&b.0.name));

    match matching.split_first() {
        Some(((first, chip), rest)) => Detection::Found {
            port: first.name.clone(),
            chip,
            also_matching: rest.iter().map(|(c, _)| c.name.clone()).collect(),
        },
        None => {
            let mut seen: Vec<String> = candidates.iter().map(|c| c.name.clone()).collect();
            seen.sort();
            Detection::NotFound { seen }
        }
    }
}

/// Enumerate the machine's serial ports and [`pick`] the CYD.
pub fn detect() -> Detection {
    let candidates: Vec<Candidate> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| Candidate {
            name: p.port_name,
            usb: match p.port_type {
                SerialPortType::UsbPort(info) => Some((info.vid, info.pid)),
                _ => None,
            },
        })
        .collect();
    pick(&candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CH340: Option<(u16, u16)> = Some((0x1a86, 0x7523));
    const CP2102: Option<(u16, u16)> = Some((0x10c4, 0xea60));
    const FTDI: Option<(u16, u16)> = Some((0x0403, 0x6001));

    fn port(name: &str, usb: Option<(u16, u16)>) -> Candidate {
        Candidate {
            name: name.into(),
            usb,
        }
    }

    #[test]
    fn finds_a_ch340_among_unrelated_ports() {
        let ports = [
            port("/dev/ttyS0", None),
            port("/dev/ttyUSB0", FTDI),
            port("/dev/ttyUSB1", CH340),
        ];
        assert_eq!(
            pick(&ports),
            Detection::Found {
                port: "/dev/ttyUSB1".into(),
                chip: "CH340",
                also_matching: vec![],
            }
        );
    }

    #[test]
    fn finds_a_cp2102_on_windows_style_names() {
        let ports = [port("COM1", None), port("COM9", CP2102)];
        assert!(matches!(
            pick(&ports),
            Detection::Found { port, chip: "CP2102", .. } if port == "COM9"
        ));
    }

    #[test]
    fn on_macos_picks_the_call_out_node_not_the_dial_in_twin() {
        // Same physical device, enumerated twice.
        let ports = [
            port("/dev/tty.usbserial-1420", CH340),
            port("/dev/cu.usbserial-1420", CH340),
        ];
        assert_eq!(
            pick(&ports),
            Detection::Found {
                port: "/dev/cu.usbserial-1420".into(),
                chip: "CH340",
                also_matching: vec![],
            }
        );
    }

    #[test]
    fn two_matching_boards_picks_the_first_by_name_and_reports_the_other() {
        // Reversed on purpose: the result must not depend on enumeration order.
        let ports = [port("/dev/ttyUSB3", CH340), port("/dev/ttyUSB1", CP2102)];
        assert_eq!(
            pick(&ports),
            Detection::Found {
                port: "/dev/ttyUSB1".into(),
                chip: "CP2102",
                also_matching: vec!["/dev/ttyUSB3".into()],
            }
        );
    }

    #[test]
    fn nothing_matching_reports_every_port_seen() {
        let ports = [port("/dev/ttyUSB0", FTDI), port("/dev/ttyS0", None)];
        assert_eq!(
            pick(&ports),
            Detection::NotFound {
                seen: vec!["/dev/ttyS0".into(), "/dev/ttyUSB0".into()],
            }
        );
    }

    #[test]
    fn no_ports_at_all() {
        assert_eq!(pick(&[]), Detection::NotFound { seen: vec![] });
    }

    #[test]
    fn a_matching_vendor_with_a_different_product_is_not_a_cyd() {
        // 1a86:55d4 is a CH9102 — same vendor, different chip.
        let ports = [port("/dev/ttyACM0", Some((0x1a86, 0x55d4)))];
        assert!(matches!(pick(&ports), Detection::NotFound { .. }));
    }

    #[test]
    fn detect_runs_on_this_machine_without_panicking() {
        // Whatever hardware is attached, enumeration must not panic.
        let _ = detect();
    }
}
