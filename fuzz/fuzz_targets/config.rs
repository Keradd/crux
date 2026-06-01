#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if s.len() > 8192 {
            return;
        }

        let _: Result<crux_core::Config, _> = toml::from_str(s);
    }
});
