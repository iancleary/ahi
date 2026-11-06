#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = ahi::hooks::config::parse_config_bytes(data);
});
