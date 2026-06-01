#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if s.len() > 4096 {
            return;
        }

        let _: Result<crux_mcp::protocol::Request, _> = serde_json::from_str(s);
        let _: Result<crux_mcp::protocol::CallToolParams, _> = serde_json::from_str(s);
    }
});
